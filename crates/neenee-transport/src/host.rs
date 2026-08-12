//! The session daemon runtime (ADR-0096): one process that owns every
//! session across every project for the user and serves them over the
//! control plane (Unix domain socket by default, TCP + bearer token with
//! `--expose`) so TUI/CLI/web clients can drive, observe, and manage them.
//!
//! Vocabulary (ADR-0094/0096): the *role* is the **daemon**; `neenee serve`
//! runs it in the foreground, `neenee serve --detach` in the background.

use crate::UiBridge;
use crate::bootstrap;
use crate::registry::{HostParams, SessionRegistry};
use crate::serve::{ServeExpose, ServeOptions, start_server};
use crate::serve_discovery as discovery;
use neenee_agent::{AgentIdentity, PrincipalProfile};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct HostOptions {
    pub port: u16,
    pub expose: ServeExpose,
    pub token: Option<String>,
    /// Serve the control plane over a Unix domain socket at this path
    /// (ADR-0096). `None` disables the UDS listener (unix-only).
    #[cfg(unix)]
    pub uds_path: Option<std::path::PathBuf>,
}

pub struct HostIdentity {
    pub identity: AgentIdentity,
    pub principal: PrincipalProfile,
    pub ui: Arc<dyn UiBridge>,
}

/// Run the daemon in the foreground until Ctrl-C.
pub async fn run(
    identity: HostIdentity,
    opts: HostOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let HostIdentity {
        identity,
        principal,
        ui,
    } = identity;
    bootstrap::ensure_app_roots();
    // One global registry: HostParams no longer pins a project (ADR-0096);
    // each session records its own project root.
    let registry = Arc::new(SessionRegistry::new(HostParams {
        identity,
        principal,
        ui,
    }));
    let started_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    registry.set_monitor_meta(String::new(), started_at).await;

    #[cfg(unix)]
    let uds_path = opts.uds_path.clone();
    let handle = start_server(
        ServeOptions {
            port: opts.port,
            expose: opts.expose,
            token: opts.token,
            #[cfg(unix)]
            uds_path,
        },
        Arc::clone(&registry),
    );
    // Reclaim abandoned never-persisted sessions so create-then-disconnect
    // churn cannot grow the registry unboundedly. Stops on server shutdown.
    registry.spawn_idle_reaper(handle.cancel.clone());
    let port = handle.port.await?;
    #[cfg(unix)]
    let bound_uds = handle.uds_ready.await.ok().flatten();
    #[cfg(not(unix))]
    let bound_uds: Option<std::path::PathBuf> = None;

    // Global discovery record (ADR-0096): one per user, not per project.
    let record = discovery::Discovery {
        pid: std::process::id(),
        port,
        token: handle.token.clone(),
        project_root: String::new(), // daemon is project-agnostic now
        started_at,
        uds_path: bound_uds.clone(),
    };
    let dp = match discovery::write_global(&record) {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::warn!(%e,"neenee serve: could not write discovery file");
            None
        }
    };

    // Foreground mode is typed interactively: say where the daemon listens
    // and how to reach it, on stderr so piping stays clean.
    let bind = if handle.token.is_some() {
        "0.0.0.0"
    } else {
        "127.0.0.1"
    };
    if let Some(uds) = &bound_uds {
        eprintln!("neenee-server: control plane on unix://{}", uds.display());
    }
    eprintln!("neenee-server: serving sessions on ws://{bind}:{port}");
    eprintln!("neenee: observe with `neenee status --watch`, drive with `neenee attach [id]`");
    if let Some(token) = &handle.token {
        eprintln!("neenee: exposed listener requires Authorization: Bearer {token}");
    }
    tracing::info!(%bind,port,"neenee serve: listening");
    tokio::signal::ctrl_c().await?;
    tracing::info!("neenee serve: shutdown requested (ctrl-c)");
    handle.cancel.cancel();
    if let Some(p) = dp {
        discovery::remove(&p);
    }
    Ok(())
}
