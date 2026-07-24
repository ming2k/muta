#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
//! `neenee-server`: the headless session host.
//!
//! Spawned on demand (by users directly, or by `neenee --attach` when no
//! server is running for the project yet), this binary hosts ONE session and
//! serves it over WebSocket so TUI/browser clients can co-drive the same
//! live session. It is a thin shell: every piece of session logic lives in
//! `neenee-transport` ([`bootstrap`] for harness assembly, [`serve`] for the
//! wire protocol, [`serve_discovery`] for the discovery record); this binary
//! only supplies its identity/principal (the application layer, ADR-0054), a
//! headless UI bridge, the discovery-file write/remove lifecycle, and the
//! shutdown sequence.

mod identity;
mod ui;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use neenee_core::AgentResponse;
use neenee_transport::bootstrap::{self, BootstrapParams};
use neenee_transport::serve::{ServeExpose, ServeOptions, start_server};
use neenee_transport::serve_discovery as discovery;
use neenee_transport::startup::{StartupMode, init_tracing};
use tokio::sync::broadcast;

use crate::identity::{neenee_identity, principal_code};
use crate::ui::HeadlessUi;

const USAGE: &str =
    "Usage: neenee-server [--project <path>] [--session <id>] [--port <n>] [--public]";

/// The parsed command line. Hand-rolled (the workspace does not use clap):
/// the surface is deliberately tiny — this binary is spawned by wrappers,
/// not typed interactively.
struct Args {
    /// `--project <path>`; defaults to the current directory.
    project: PathBuf,
    /// `--session <id>`; `None` starts a fresh session.
    session: Option<String>,
    /// `--port <n>`; `0` lets the OS pick.
    port: u16,
    /// `--public`: bind 0.0.0.0 (bearer token enforced by the serve layer).
    public: bool,
}

fn parse_args(args: Vec<String>) -> Result<Args, String> {
    let mut project: Option<PathBuf> = None;
    let mut session: Option<String> = None;
    let mut port: u16 = 0;
    let mut public = false;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        if arg == "--project" {
            project = Some(PathBuf::from(
                iter.next().ok_or("--project requires a value")?,
            ));
        } else if let Some(value) = arg.strip_prefix("--project=") {
            project = Some(PathBuf::from(value));
        } else if arg == "--session" {
            session = Some(iter.next().ok_or("--session requires a value")?);
        } else if let Some(value) = arg.strip_prefix("--session=") {
            session = Some(value.to_string());
        } else if arg == "--port" {
            let value = iter.next().ok_or("--port requires a value")?;
            port = value
                .parse()
                .map_err(|_| format!("invalid --port value '{value}'"))?;
        } else if let Some(value) = arg.strip_prefix("--port=") {
            port = value
                .parse()
                .map_err(|_| format!("invalid --port value '{value}'"))?;
        } else if arg == "--public" {
            public = true;
        } else {
            return Err(format!("unknown argument '{arg}'"));
        }
    }
    let project = match project {
        Some(p) => p,
        None => std::env::current_dir()
            .map_err(|e| format!("could not resolve current directory: {e}"))?,
    };
    Ok(Args {
        project,
        session,
        port,
        public,
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _tracing_guard = init_tracing();

    let args = match parse_args(std::env::args().skip(1).collect()) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("neenee-server: {error}\n{USAGE}");
            std::process::exit(2);
        }
    };

    let startup = match &args.session {
        Some(id) => StartupMode::Resume(Some(id.clone())),
        None => StartupMode::Fresh,
    };

    // Assemble the shared session harness — the same factory the TUI uses.
    // This binary supplies the product identity, the coding principal, and a
    // headless UI bridge; everything else is application-neutral. First-run
    // friendliness (creating the XDG app roots before any store opens) is
    // handled inside `assemble` itself, so a fresh XDG root works for both
    // binaries.
    let boot = bootstrap::assemble(BootstrapParams {
        identity: neenee_identity(),
        principal: principal_code(),
        ui: Arc::new(HeadlessUi),
        startup,
        project_root: Some(args.project.clone()),
        unattended: false,
        single_instance: false,
    })
    .await?;

    // Fan-out tap (mirrors the TUI's `/serve` tap): every AgentResponse the
    // driver emits is forwarded into a broadcast channel that each WebSocket
    // client subscribes to. `send` errors only mean "no subscribers right
    // now" — a full buffer must never back-pressure the driver. The task
    // ends when the driver closes its response channel.
    let (events_tx, _) = broadcast::channel::<AgentResponse>(1024);
    {
        let events_tx = events_tx.clone();
        let mut resp_rx = boot.resp_rx;
        tokio::spawn(async move {
            while let Some(response) = resp_rx.recv().await {
                let _ = events_tx.send(response);
            }
        });
    }

    let handle = start_server(
        ServeOptions {
            port: args.port,
            expose: if args.public {
                ServeExpose::Public
            } else {
                ServeExpose::Local
            },
            // Local stays unauthenticated; under Public the serve layer
            // generates a token and returns it in the handle.
            token: None,
        },
        boot.req_tx.clone(),
        events_tx,
        boot.session.clone(),
    );
    // Await the bound port (resolves `--port 0` to the OS-assigned value). An
    // error means the listener task died before binding (details in the log).
    let port = handle.port.await?;

    // Keep-alive: SessionDriver::run exits when ALL AgentRequest senders
    // drop. The serve listener holds a req_tx clone for its own lifetime, but
    // the session's liveness must not depend on the listener task's internals
    // — hold our own sender in main's scope until shutdown completes.
    let _keep_alive_req_tx = boot.req_tx;

    let session_id = boot.session.id().await;

    // Discovery record — written only now that the port is known, so clients
    // never read a record pointing at an unbound port.
    let record = discovery::Discovery {
        pid: std::process::id(),
        port,
        token: handle.token.clone(),
        session_id: session_id.clone(),
        project_root: args.project.display().to_string(),
        started_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    };
    let discovery_path = match discovery::write(&args.project, &record) {
        Ok(path) => Some(path),
        Err(error) => {
            // Non-fatal: a client can still be pointed at the printed port.
            tracing::warn!(%error, "neenee-server: could not write discovery file");
            eprintln!("neenee-server: warning: could not write discovery file: {error}");
            None
        }
    };

    {
        use std::io::Write as _;
        let bind = if args.public { "0.0.0.0" } else { "127.0.0.1" };
        print!(
            "neenee-server: project={} session={} listening={bind}:{port}",
            args.project.display(),
            session_id,
        );
        if let Some(token) = &handle.token {
            print!(" token={token}");
        }
        println!();
        // stdout is block-buffered when piped; flush so a spawning process
        // can read the startup line without waiting for shutdown.
        let _ = std::io::stdout().flush();
    }

    // Run until ctrl-c (or the driver finishes on its own, e.g. every
    // request sender is gone).
    tokio::select! {
        result = tokio::signal::ctrl_c() => {
            if let Err(error) = result {
                tracing::warn!(%error, "neenee-server: ctrl-c listener failed");
            }
            tracing::info!("neenee-server: shutdown requested (ctrl-c)");
        }
        _ = boot.driver.run() => {
            tracing::info!("neenee-server: session driver exited");
        }
    }

    // Clean shutdown: SessionEnd hooks (ADR-0025), stop the listener, drop
    // the discovery file (best-effort).
    boot.agent_for_session_end.fire_session_end().await;
    handle.cancel.cancel();
    if let Some(path) = discovery_path {
        discovery::remove(&path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_flags_both_styles() {
        let args = parse_args(vec![
            "--project".to_string(),
            "/tmp/x".to_string(),
            "--session=abc".to_string(),
            "--port".to_string(),
            "8080".to_string(),
            "--public".to_string(),
        ])
        .unwrap();
        assert_eq!(args.project, PathBuf::from("/tmp/x"));
        assert_eq!(args.session.as_deref(), Some("abc"));
        assert_eq!(args.port, 8080);
        assert!(args.public);

        let args = parse_args(vec!["--project=/tmp/y".to_string()]).unwrap();
        assert_eq!(args.project, PathBuf::from("/tmp/y"));
    }

    #[test]
    fn defaults_are_fresh_session_os_port_loopback() {
        let args = parse_args(Vec::new()).unwrap();
        assert!(args.session.is_none());
        assert_eq!(args.port, 0);
        assert!(!args.public);
    }

    #[test]
    fn unknown_args_are_rejected() {
        assert!(parse_args(vec!["--nope".to_string()]).is_err());
        assert!(parse_args(vec!["resume".to_string()]).is_err());
    }

    #[test]
    fn missing_or_bad_values_are_rejected() {
        assert!(parse_args(vec!["--project".to_string()]).is_err());
        assert!(parse_args(vec!["--session".to_string()]).is_err());
        assert!(parse_args(vec!["--port".to_string()]).is_err());
        assert!(parse_args(vec!["--port".to_string(), "nope".to_string()]).is_err());
        assert!(parse_args(vec!["--port=99999".to_string()]).is_err());
    }

    #[test]
    fn discovery_module_is_the_shared_transport_one() {
        // The discovery record/path logic lives in `neenee-transport` so the
        // attaching client (`neenee --attach`) resolves the exact same file.
        // This binary only drives the write/remove lifecycle. Path resolution
        // is pure (no filesystem access), so calling it here is safe.
        let path = discovery::discovery_path(std::path::Path::new("/tmp/some-project"));
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("json"));
        // The record type round-trips through serde — the same shape clients
        // deserialize.
        let record = discovery::Discovery {
            pid: 1,
            port: 2,
            token: None,
            session_id: "s".to_string(),
            project_root: "/tmp/some-project".to_string(),
            started_at: 3,
        };
        let json = serde_json::to_string(&record).unwrap();
        let back: discovery::Discovery = serde_json::from_str(&json).unwrap();
        assert_eq!(back, record);
    }
}
