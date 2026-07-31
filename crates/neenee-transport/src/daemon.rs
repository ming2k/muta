use crate::UiBridge;
use crate::bootstrap;
use crate::registry::{HostParams, SessionRegistry};
use crate::serve::{ServeExpose, ServeOptions, start_server};
use crate::serve_discovery as discovery;
use neenee_agent::{AgentIdentity, PrincipalProfile};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
pub struct DaemonOptions {
    pub port: u16,
    pub expose: ServeExpose,
    pub token: Option<String>,
}
pub struct DaemonIdentity {
    pub identity: AgentIdentity,
    pub principal: PrincipalProfile,
    pub ui: Arc<dyn UiBridge>,
}
pub async fn run(
    identity: DaemonIdentity,
    project_root: &std::path::Path,
    opts: DaemonOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let DaemonIdentity {
        identity,
        principal,
        ui,
    } = identity;
    bootstrap::ensure_app_roots();
    let registry = SessionRegistry::new(HostParams {
        identity,
        principal,
        ui,
        project_root: project_root.to_path_buf(),
    });
    let handle = start_server(
        ServeOptions {
            port: opts.port,
            expose: opts.expose,
            token: opts.token,
        },
        Arc::new(registry),
    );
    let port = handle.port.await?;
    let record = discovery::Discovery {
        pid: std::process::id(),
        port,
        token: handle.token.clone(),
        project_root: project_root.display().to_string(),
        started_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    };
    let dp = match discovery::write(project_root, &record) {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::warn!(%e,"neenee daemon: could not write discovery file");
            None
        }
    };
    let bind = if handle.token.is_some() {
        "0.0.0.0"
    } else {
        "127.0.0.1"
    };
    tracing::info!(%bind,port,project=%project_root.display(),"neenee daemon: listening");
    tokio::signal::ctrl_c().await?;
    tracing::info!("neenee daemon: shutdown requested (ctrl-c)");
    handle.cancel.cancel();
    if let Some(p) = dp {
        discovery::remove(&p);
    }
    Ok(())
}
