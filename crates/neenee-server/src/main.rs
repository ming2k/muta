#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
//! `neenee-server`: the headless multi-session daemon binary.
//!
//! Spawned detached (by `neenee attach` when no daemon is running for the
//! project), this binary hosts any number of sessions for one project and
//! serves them over WebSocket so TUI/browser clients can co-drive the same
//! live session(s). It is a thin shell: every piece of session logic lives
//! in `neenee-transport::daemon` (ADR-0089); this binary only supplies the
//! product identity/principal (ADR-0054), a headless UI bridge, and CLI
//! parsing. `neenee daemon` is the same runtime under a different invocation.

mod identity;
mod ui;

use std::path::PathBuf;

use neenee_transport::daemon::{DaemonIdentity, DaemonOptions};
use neenee_transport::serve::ServeExpose;
use neenee_transport::startup::init_tracing;

use crate::identity::{neenee_identity, principal_code};
use crate::ui::HeadlessUi;

const USAGE: &str = "Usage: neenee-server [--project <path>] [--port <n>] [--public]";

/// The parsed command line. Hand-rolled (the workspace does not use clap):
/// the surface is deliberately tiny — this binary is spawned by wrappers,
/// not typed interactively.
struct Args {
    project: PathBuf,
    port: u16,
    public: bool,
}

fn parse_args(args: Vec<String>) -> Result<Args, String> {
    let mut project: Option<PathBuf> = None;
    let mut port: u16 = 0;
    let mut public = false;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        if arg == "--project" {
            project = Some(PathBuf::from(iter.next().ok_or("--project requires a value")?));
        } else if let Some(value) = arg.strip_prefix("--project=") {
            project = Some(PathBuf::from(value));
        } else if arg == "--port" {
            let value = iter.next().ok_or("--port requires a value")?;
            port = value.parse().map_err(|_| format!("invalid --port value '{value}'"))?;
        } else if let Some(value) = arg.strip_prefix("--port=") {
            port = value.parse().map_err(|_| format!("invalid --port value '{value}'"))?;
        } else if arg == "--public" {
            public = true;
        } else {
            return Err(format!("unknown argument '{arg}'"));
        }
    }
    let project = match project {
        Some(p) => p,
        None => std::env::current_dir().map_err(|e| format!("could not resolve current directory: {e}"))?,
    };
    Ok(Args { project, port, public })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _tracing_guard = init_tracing();
    let args = match parse_args(std::env::args().skip(1).collect()) {
        Ok(args) => args,
        Err(error) => { eprintln!("neenee-server: {error}\n{USAGE}"); std::process::exit(2); }
    };
    let expose = if args.public { ServeExpose::Public } else { ServeExpose::Local };
    {
        use std::io::Write as _;
        let bind = if args.public { "0.0.0.0" } else { "127.0.0.1" };
        eprintln!("neenee-server: project={} starting on {bind}:{}", args.project.display(), if args.port == 0 { "0 (OS-assigned)".to_string() } else { args.port.to_string() });
        let _ = std::io::stderr().flush();
    }
    neenee_transport::daemon::run(
        DaemonIdentity { identity: neenee_identity(), principal: principal_code(), ui: std::sync::Arc::new(HeadlessUi) },
        &args.project,
        DaemonOptions { port: args.port, expose, token: None },
    ).await?;
    eprintln!("neenee-server: stopped.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_flags_both_styles() {
        let args = parse_args(vec!["--project".to_string(), "/tmp/x".to_string(), "--port".to_string(), "8080".to_string(), "--public".to_string()]).unwrap();
        assert_eq!(args.project, PathBuf::from("/tmp/x"));
        assert_eq!(args.port, 8080);
        assert!(args.public);
        let args = parse_args(vec!["--project=/tmp/y".to_string()]).unwrap();
        assert_eq!(args.project, PathBuf::from("/tmp/y"));
    }

    #[test]
    fn defaults_are_os_port_loopback() {
        let args = parse_args(Vec::new()).unwrap();
        assert_eq!(args.port, 0);
        assert!(!args.public);
    }

    #[test]
    fn unknown_args_are_rejected() {
        assert!(parse_args(vec!["--nope".to_string()]).is_err());
        assert!(parse_args(vec!["--session".to_string()]).is_err());
        assert!(parse_args(vec!["resume".to_string()]).is_err());
    }

    #[test]
    fn missing_or_bad_values_are_rejected() {
        assert!(parse_args(vec!["--project".to_string()]).is_err());
        assert!(parse_args(vec!["--port".to_string()]).is_err());
        assert!(parse_args(vec!["--port".to_string(), "nope".to_string()]).is_err());
        assert!(parse_args(vec!["--port=99999".to_string()]).is_err());
    }
}
