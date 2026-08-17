//! Client side of the daemon control plane: discovery, the attach handshake
//! (`connect`), one-shot control verbs (`control`), and the monitor stream
//! (ADR-0093/0096). `neenee` / `neenee attach` / `neenee status` drive
//! sessions owned by the unified session daemon (`neenee-server`) through
//! this module. Discovery is global (one daemon per user); connections
//! prefer the Unix domain socket and fall back to TCP.
//!
//! The wire protocol this client speaks is [`crate::serve::Wire`] — client
//! and server live in the same crate so the protocol cannot drift.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::serve::Wire;
use crate::serve_discovery as discovery;
use futures::{SinkExt, StreamExt};
use neenee_contracts::{
    AgentRequest, AgentResponse, Message, MonitorAction, MonitorEvent, MonitoredSession,
    SessionOverview,
};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

pub use crate::serve::AttachAction;
pub use crate::serve_discovery::Discovery as DaemonInfo;

const LIVENESS_TIMEOUT: Duration = Duration::from_millis(500);
const SERVER_START_TIMEOUT: Duration = Duration::from_secs(10);
const SERVER_START_POLL: Duration = Duration::from_millis(100);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Find the unified daemon (ADR-0096): one global record. The project
/// argument is accepted for source compatibility but no longer scopes the
/// lookup — the daemon serves every project.
pub fn discover(_project_root: &Path) -> Option<DaemonInfo> {
    let global_path = discovery::global_discovery_path();
    if let Some(info) = discover_at(&global_path) {
        return Some(info);
    }

    // Self-healing fallback: if daemon.json is missing or unlinked,
    // but a live daemon holds the instance lock and is responsive on UDS/TCP:
    let lock_path = discovery::global_lock_path();
    if neenee_persistence::lock::ProcessLock::is_locked(&lock_path) {
        if let Some(pid) = neenee_persistence::lock::ProcessLock::probe_holder(&lock_path) {
            if is_process_alive(pid) {
                #[cfg(unix)]
                let uds = discovery::default_uds_path();
                #[cfg(unix)]
                let uds_connectable = if uds.exists() {
                    std::os::unix::net::UnixStream::connect(&uds).is_ok()
                } else {
                    false
                };
                #[cfg(not(unix))]
                let uds_connectable = false;

                let tcp_addr = std::net::SocketAddr::from((
                    [127, 0, 0, 1],
                    crate::startup::DEFAULT_SERVE_PORT,
                ));
                let tcp_connectable =
                    std::net::TcpStream::connect_timeout(&tcp_addr, Duration::from_millis(300))
                        .is_ok();

                if uds_connectable || tcp_connectable {
                    let recovered = DaemonInfo {
                        pid,
                        port: crate::startup::DEFAULT_SERVE_PORT,
                        token: None,
                        project_root: String::new(),
                        started_at: 0,
                        #[cfg(unix)]
                        uds_path: if uds.exists() { Some(uds) } else { None },
                        #[cfg(not(unix))]
                        uds_path: None,
                        version: Some(crate::serve::daemon_version().to_string()),
                    };
                    // Restore discovery file so future lookups are immediate
                    let _ = discovery::write_global(&recovered);
                    tracing::info!(
                        pid,
                        "discover: recovered discovery record from live lock holder"
                    );
                    return Some(recovered);
                }
            }
        }
    }

    None
}

fn discover_at(path: &Path) -> Option<DaemonInfo> {
    let bytes = std::fs::read(path).ok()?;
    let info: DaemonInfo = serde_json::from_slice(&bytes).ok()?;
    if !is_process_alive(info.pid) {
        discovery::remove(path);
        return None;
    }
    if !is_alive(&info) {
        return None;
    }
    Some(info)
}

/// Directional relation between client version and daemon version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionRelation {
    Equal,
    ClientNewer,
    ClientOlder,
    Unknown,
}

/// Compare client and daemon version strings using SemVer.
pub fn compare_versions(client: &str, daemon: &str) -> VersionRelation {
    if client == daemon {
        return VersionRelation::Equal;
    }
    match (
        semver::Version::parse(client),
        semver::Version::parse(daemon),
    ) {
        (Ok(c), Ok(d)) => {
            if c > d {
                VersionRelation::ClientNewer
            } else if c < d {
                VersionRelation::ClientOlder
            } else {
                VersionRelation::Equal
            }
        }
        _ => VersionRelation::Unknown,
    }
}

/// The actionable version-skew error (ADR-0100 rule 4), naming both builds
/// and the directional fix (server behind -> stop/restart server; client behind -> update client).
/// Public so `neenee`-level commands can surface it uniformly
/// wherever a discovered daemon is about to be spoken to.
pub fn version_mismatch(info: &DaemonInfo) -> String {
    let client_ver = crate::serve::daemon_version();
    let Some(daemon_ver) = info.version.as_deref() else {
        return format!(
            "client/daemon version mismatch: this client is {client_ver} but the running daemon (pid {}) is unknown (older than 0.24). \
             Stop it with `neenee stop` and rerun — the daemon restarts on demand at the new version.",
            info.pid
        );
    };

    match compare_versions(client_ver, daemon_ver) {
        VersionRelation::ClientNewer => format!(
            "client/daemon version mismatch: running daemon (pid {}, version {daemon_ver}) is older than this client ({client_ver}). \
             Stop it with `neenee stop` and rerun — the daemon restarts on demand at the new version.",
            info.pid
        ),
        VersionRelation::ClientOlder => format!(
            "client/daemon version mismatch: this client ({client_ver}) is older than the running daemon (pid {}, version {daemon_ver}). \
             Please update your neenee client to {daemon_ver} or newer.",
            info.pid
        ),
        VersionRelation::Equal => format!(
            "client/daemon version mismatch: client and daemon both report {client_ver} but failed compatibility check."
        ),
        VersionRelation::Unknown => format!(
            "client/daemon version mismatch: this client is {client_ver} but the running daemon (pid {}) is {daemon_ver}. \
             If the daemon is outdated, stop it with `neenee stop` and rerun; if the client is outdated, update your client.",
            info.pid
        ),
    }
}

/// Whether a discovered daemon speaks this client's version (ADR-0100
/// rule 4). `None` on the record (a pre-versioning daemon) counts as a
/// mismatch: the wire protocol has no negotiation, so guessing is exactly
/// the failure mode the rule exists to prevent.
pub fn versions_compatible(info: &DaemonInfo) -> bool {
    info.version
        .as_deref()
        .is_some_and(|daemon| daemon == crate::serve::daemon_version())
}

/// Liveness probe: prefer the UDS (the daemon's primary local channel),
/// fall back to the TCP port. Either reachable means the daemon is up.
fn is_alive(info: &DaemonInfo) -> bool {
    if !is_process_alive(info.pid) {
        return false;
    }
    #[cfg(unix)]
    if let Some(uds) = &info.uds_path {
        use std::os::unix::net::UnixStream;
        if UnixStream::connect(uds).is_ok() {
            return true;
        }
    }
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], info.port));
    std::net::TcpStream::connect_timeout(&addr, LIVENESS_TIMEOUT).is_ok()
}

/// Whether a process with `pid` exists and can receive signals.
pub fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Wait up to `timeout` for process `pid` to exit.
async fn wait_for_process_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if !is_process_alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    !is_process_alive(pid)
}

/// Stop the daemon through a tiered shutdown pipeline:
/// 1. Tier 1 (Protocol): If the daemon speaks this client's version, send `Shutdown` control verb.
/// 2. Tier 2 (OS Signal): If skewed or control fails, send `SIGTERM` to `info.pid`.
/// 3. Tier 3 (Force): If the daemon remains alive after grace budget, escalate to `SIGKILL`.
/// 4. Tier 4 (Cleanup): Clean up the discovery record and UDS socket.
pub async fn stop(info: &DaemonInfo) -> Result<(), String> {
    let mut stopped = false;

    // Tier 1: Try graceful protocol shutdown if versions are compatible.
    if versions_compatible(info) {
        if let Ok(Ok(())) = tokio::time::timeout(
            Duration::from_millis(1500),
            control(info, crate::serve::ControlRequest::Shutdown),
        )
        .await
        {
            stopped = wait_for_process_exit(info.pid, Duration::from_millis(2000)).await;
        }
    }

    // Tier 2 & 3: Fall back to OS signals if protocol did not stop it.
    if !stopped {
        #[cfg(unix)]
        {
            let pid = info.pid as libc::pid_t;
            if is_process_alive(info.pid) {
                let _ = unsafe { libc::kill(pid, libc::SIGTERM) };
                stopped = wait_for_process_exit(info.pid, Duration::from_millis(1500)).await;

                if !stopped && is_process_alive(info.pid) {
                    let _ = unsafe { libc::kill(pid, libc::SIGKILL) };
                    stopped = wait_for_process_exit(info.pid, Duration::from_millis(1000)).await;
                }
            } else {
                stopped = true;
            }
        }
        #[cfg(not(unix))]
        {
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &info.pid.to_string(), "/F"])
                .output();
            stopped = true;
        }
    }

    // Tier 4: Cleanup discovery record & socket
    discovery::remove_if_matching_pid(&discovery::global_discovery_path(), info.pid);
    #[cfg(unix)]
    if let Some(uds) = &info.uds_path {
        let _ = std::fs::remove_file(uds);
    }

    if stopped || !is_process_alive(info.pid) {
        Ok(())
    } else {
        Err(format!("could not stop daemon (pid {})", info.pid))
    }
}

/// Path to the daemon startup stderr log file.
pub fn startup_log_path() -> PathBuf {
    let dirs = neenee_persistence::paths::get();
    dirs.state_dir.join("log").join("daemon-startup.log")
}

pub async fn ensure_daemon(project_root: &Path) -> Result<DaemonInfo, String> {
    if let Some(info) = discover(project_root) {
        if versions_compatible(&info) {
            return Ok(info);
        }
        // Discovered an outdated daemon (daemon version < client version, or unknown pre-0.24).
        // Automatically stop the outdated daemon and spawn the current version on demand.
        let is_client_newer = match info.version.as_deref() {
            Some(v) => {
                compare_versions(crate::serve::daemon_version(), v) == VersionRelation::ClientNewer
            }
            None => true,
        };
        if is_client_newer {
            tracing::info!(
                daemon_pid = info.pid,
                daemon_ver = ?info.version,
                client_ver = crate::serve::daemon_version(),
                "ensure_daemon: running daemon is older than this client; restarting daemon"
            );
            let _ = stop(&info).await;
            let _ = wait_for_process_exit(info.pid, Duration::from_secs(2)).await;
        } else {
            // Client is older than running daemon; return info so caller can surface actionable upgrade error.
            return Ok(info);
        }
    }

    // Check if another daemon is holding the instance lock
    let lock_path = discovery::global_lock_path();
    if neenee_persistence::lock::ProcessLock::is_locked(&lock_path) {
        if let Some(holder_pid) = neenee_persistence::lock::ProcessLock::probe_holder(&lock_path) {
            if is_process_alive(holder_pid) {
                tracing::info!(
                    holder_pid,
                    "ensure_daemon: daemon instance lock held; waiting for startup or draining"
                );
                let init_deadline = std::time::Instant::now() + Duration::from_secs(2);
                while std::time::Instant::now() < init_deadline {
                    tokio::time::sleep(SERVER_START_POLL).await;
                    if let Some(info) = discover(project_root) {
                        if versions_compatible(&info) {
                            return Ok(info);
                        }
                    }
                    if !is_process_alive(holder_pid) {
                        break;
                    }
                }

                // If still locked and discover() failed, the process holding the lock is deadlocked or unresponsive.
                // Clear the deadlock by stopping the unresponsive process.
                if is_process_alive(holder_pid) {
                    tracing::warn!(
                        holder_pid,
                        "ensure_daemon: process holding lock is unresponsive; clearing deadlock"
                    );
                    let ghost = DaemonInfo {
                        pid: holder_pid,
                        port: crate::startup::DEFAULT_SERVE_PORT,
                        token: None,
                        project_root: String::new(),
                        started_at: 0,
                        #[cfg(unix)]
                        uds_path: Some(discovery::default_uds_path()),
                        #[cfg(not(unix))]
                        uds_path: None,
                        version: None,
                    };
                    let _ = stop(&ghost).await;
                    let _ = wait_for_process_exit(holder_pid, Duration::from_secs(2)).await;
                }
            }
        }
    }

    let mut child = spawn_daemon()?;
    let deadline = std::time::Instant::now() + SERVER_START_TIMEOUT;
    loop {
        tokio::time::sleep(SERVER_START_POLL).await;
        if let Some(info) = discover(project_root) {
            if versions_compatible(&info) {
                return Ok(info);
            }
        }
        if let Ok(Some(status)) = child.try_wait() {
            let log_text = std::fs::read_to_string(startup_log_path()).unwrap_or_default();
            let log_trimmed = log_text.trim();
            if !log_trimmed.is_empty() {
                return Err(format!(
                    "neenee daemon exited prematurely ({status}): {log_trimmed}"
                ));
            } else {
                return Err(format!("neenee daemon exited prematurely with {status}"));
            }
        }
        if std::time::Instant::now() >= deadline {
            let log_text = std::fs::read_to_string(startup_log_path()).unwrap_or_default();
            let log_trimmed = log_text.trim();
            let lock_info = if neenee_persistence::lock::ProcessLock::is_locked(&lock_path) {
                neenee_persistence::lock::ProcessLock::probe_holder(&lock_path)
                    .map(|pid| format!(" (instance lock held by PID {pid})"))
                    .unwrap_or_else(|| " (instance lock is held)".to_string())
            } else {
                String::new()
            };
            if !log_trimmed.is_empty() {
                return Err(format!(
                    "timed out after {}s waiting for neenee daemon to start{lock_info}: {log_trimmed}",
                    SERVER_START_TIMEOUT.as_secs(),
                ));
            } else {
                return Err(format!(
                    "timed out after {}s waiting for neenee daemon to start{lock_info}. \
                     Run `neenee daemon status` to inspect server state or view logs in ~/.local/state/neenee/log/",
                    SERVER_START_TIMEOUT.as_secs(),
                ));
            }
        }
    }
}

fn spawn_daemon() -> Result<std::process::Child, String> {
    let program = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("neenee"));
    let mut command = std::process::Command::new(&program);
    command.arg("serve");
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null());

    let startup_log = startup_log_path();
    if let Some(parent) = startup_log.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&startup_log)
    {
        command.stderr(file);
    } else {
        command.stderr(std::process::Stdio::null());
    }

    // Pin the daemon's cwd to a stable, always-existing directory instead of
    // inheriting this client's project. ADR-0096 made the daemon the host for
    // *every* project's sessions, so a project directory inherited from the
    // first lucky client is exactly the wrong default — any code path that
    // still consults the daemon's cwd (rather than a session-scoped root)
    // would silently land in that project. Per-session scoping is explicit
    // via the Select frame's `project` field.
    command.current_dir("/");
    // Own process group (ADR-0101): a daemon spawned from an interactive
    // shell must not share the shell's foreground group, or the terminal's
    // Ctrl-C SIGINTs the "background" daemon along with everything else in
    // the group. `setsid`-equivalent on Unix; harmless elsewhere.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    command
        .spawn()
        .map_err(|error| format!("could not spawn {}: {error}", program.display()))
}

/// Comprehensive diagnostics for the daemon control plane and system status.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DaemonDiagnostics {
    pub discovery_path: PathBuf,
    pub discovery_record: Option<DaemonInfo>,
    pub discovery_valid: bool,
    pub lock_path: PathBuf,
    pub lock_held: bool,
    pub lock_holder_pid: Option<u32>,
    pub lock_holder_alive: bool,
    pub uds_path: PathBuf,
    pub uds_exists: bool,
    pub uds_connectable: bool,
    pub tcp_port: u16,
    pub tcp_listening: bool,
    pub startup_log_path: PathBuf,
    pub last_startup_log: Option<String>,
}

/// Perform a diagnostic probe of the daemon environment without modifying state.
pub fn diagnose_daemon() -> DaemonDiagnostics {
    let discovery_path = discovery::global_discovery_path();
    let discovery_record = discover_at(&discovery_path);
    let raw_record: Option<DaemonInfo> = std::fs::read(&discovery_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let lock_path = discovery::global_lock_path();
    let lock_held = neenee_persistence::lock::ProcessLock::is_locked(&lock_path);
    let lock_holder_pid = neenee_persistence::lock::ProcessLock::probe_holder(&lock_path);
    let lock_holder_alive = lock_holder_pid.map(is_process_alive).unwrap_or(false);

    #[cfg(unix)]
    let uds_path = discovery::default_uds_path();
    #[cfg(not(unix))]
    let uds_path = PathBuf::from("");

    let uds_exists = uds_path.exists();
    #[cfg(unix)]
    let uds_connectable = if uds_exists {
        std::os::unix::net::UnixStream::connect(&uds_path).is_ok()
    } else {
        false
    };
    #[cfg(not(unix))]
    let uds_connectable = false;

    let port = discovery_record
        .as_ref()
        .map(|d| d.port)
        .or_else(|| raw_record.as_ref().map(|d| d.port))
        .unwrap_or(9800);

    let tcp_addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let tcp_listening =
        std::net::TcpStream::connect_timeout(&tcp_addr, Duration::from_millis(200)).is_ok();

    let startup_log = startup_log_path();
    let last_startup_log = std::fs::read_to_string(&startup_log)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    DaemonDiagnostics {
        discovery_path,
        discovery_record: discovery_record.or(raw_record),
        discovery_valid: discover_at(&discovery::global_discovery_path()).is_some(),
        lock_path,
        lock_held,
        lock_holder_pid,
        lock_holder_alive,
        uds_path,
        uds_exists,
        uds_connectable,
        tcp_port: port,
        tcp_listening,
        startup_log_path: startup_log,
        last_startup_log,
    }
}

pub enum Handshake {
    Attached {
        req_tx: mpsc::UnboundedSender<AgentRequest>,
        resp_rx: mpsc::UnboundedReceiver<AgentResponse>,
        session_id: String,
        round_counter: u64,
        history: Vec<Message>,
        /// The provider/model the session is currently serving, carried on
        /// the welcome so the TUI's hint bar shows them from the first frame
        /// instead of waiting for the next provider mutation.
        provider: String,
        model: String,
    },
    Pick(Vec<SessionOverview>),
}

pub async fn connect(info: &DaemonInfo, action: AttachAction) -> Result<Handshake, String> {
    // Prefer the Unix domain socket (the daemon's primary local channel,
    // ADR-0096); fall back to TCP for exposed/legacy deployments.
    #[cfg(unix)]
    if let Some(uds) = &info.uds_path
        && let Ok(stream) = tokio::net::UnixStream::connect(uds).await
    {
        let request = "ws://localhost/"
            .into_client_request()
            .map_err(|e| format!("bad uds ws request: {e}"))?;
        let (ws, _) = tokio_tungstenite::client_async(request, stream)
            .await
            .map_err(|e| format!("ws handshake over uds: {e}"))?;
        return finish_handshake(ws.split(), action).await;
    }
    let url = format!("ws://127.0.0.1:{}/", info.port);
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("bad ws url {url}: {e}"))?;
    if let Some(token) = &info.token {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| format!("bad bearer token: {e}"))?;
        request.headers_mut().insert("Authorization", value);
    }
    let (ws, _response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| format!("ws connect to {url}: {e}"))?;
    finish_handshake(ws.split(), action).await
}

/// The stream-generic attach handshake, shared by the UDS and TCP paths.
async fn finish_handshake<S>(
    parts: (
        futures::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, WsMessage>,
        futures::stream::SplitStream<tokio_tungstenite::WebSocketStream<S>>,
    ),
    action: AttachAction,
) -> Result<Handshake, String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut ws_sink, mut ws_source) = parts;

    // Declare this client's working directory so the daemon scopes a fresh or
    // auto-attached session to the project the user actually invoked us in —
    // the daemon's own cwd is whatever the first client that spawned it
    // happened to use. A daemon predating the field ignores it; a failed cwd
    // read degrades to the daemon's fallback.
    let project = std::env::current_dir().ok();
    let select = serde_json::to_string(&Wire::Select {
        action,
        project,
        version: Some(crate::serve::daemon_version().to_string()),
    })
    .map_err(|e| format!("serialize select: {e}"))?;
    ws_sink
        .send(WsMessage::Text(select.into()))
        .await
        .map_err(|e| format!("ws send select: {e}"))?;

    let reply = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        loop {
            match ws_source.next().await {
                Some(Ok(WsMessage::Text(text))) => match serde_json::from_str::<Wire>(&text) {
                    Ok(Wire::Welcome {
                        session_id,
                        round_counter,
                        messages,
                        provider,
                        model,
                    }) => {
                        return Ok(Reply::Welcome(Welcome {
                            session_id,
                            round_counter,
                            messages,
                            provider,
                            model,
                        }));
                    }
                    Ok(Wire::Pick { sessions }) => return Ok(Reply::Pick(sessions)),
                    Ok(Wire::Error { message, .. }) => {
                        return Err(format!("daemon rejected the attach: {message}"));
                    }
                    Ok(_) => tracing::warn!("attach: unexpected frame during handshake, ignored"),
                    Err(error) => tracing::warn!(%error, "attach: bad frame during handshake"),
                },
                Some(Ok(_)) => {}
                Some(Err(error)) => return Err(format!("ws recv during handshake: {error}")),
                None => return Err("server closed the connection".to_string()),
            }
        }
    })
    .await
    .map_err(|_| "timed out waiting for handshake from daemon".to_string())??;

    let welcome = match reply {
        Reply::Welcome(w) => w,
        Reply::Pick(sessions) => {
            let _ = ws_sink.close().await;
            return Ok(Handshake::Pick(sessions));
        }
    };

    let (req_out_tx, mut req_out_rx) = mpsc::unbounded_channel::<AgentRequest>();
    let (resp_in_tx, resp_in_rx) = mpsc::unbounded_channel::<AgentResponse>();

    tokio::spawn(async move {
        while let Some(request) = req_out_rx.recv().await {
            let text = match serde_json::to_string(&Wire::Request { request }) {
                Ok(text) => text,
                Err(error) => {
                    tracing::warn!(%error, "attach: could not serialize request");
                    continue;
                }
            };
            if let Err(error) = ws_sink.send(WsMessage::Text(text.into())).await {
                tracing::warn!(%error, "attach: ws send failed");
                break;
            }
        }
        let _ = ws_sink.close().await;
    });

    tokio::spawn(async move {
        while let Some(frame) = ws_source.next().await {
            match frame {
                Ok(WsMessage::Text(text)) => match serde_json::from_str::<Wire>(&text) {
                    Ok(Wire::Response { response }) => {
                        if resp_in_tx.send(response).is_err() {
                            return;
                        }
                    }
                    Ok(_) => tracing::warn!("attach: unexpected post-handshake frame, ignored"),
                    Err(error) => tracing::warn!(%error, "attach: bad frame from server, ignored"),
                },
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "attach: ws recv failed");
                    break;
                }
            }
        }
        let _ = resp_in_tx.send(AgentResponse::Exit);
    });

    Ok(Handshake::Attached {
        req_tx: req_out_tx,
        resp_rx: resp_in_rx,
        session_id: welcome.session_id,
        round_counter: welcome.round_counter,
        history: welcome.messages,
        provider: welcome.provider,
        model: welcome.model,
    })
}

/// Issue one control-plane verb (ADR-0096) to the daemon and await its reply:
/// create, prompt, interrupt, answer a permission, or kill — without attaching
/// as a session client. The dashboard's session-management keys (`i` interrupt,
/// `p` prompt, `n` new session) go through here. Prefers the Unix socket, falls
/// back to TCP, exactly like [`connect`].
pub async fn control(
    info: &DaemonInfo,
    request: crate::serve::ControlRequest,
) -> Result<(), String> {
    use crate::serve::AttachAction;
    let action = AttachAction::Control(request);

    #[cfg(unix)]
    if let Some(uds) = &info.uds_path
        && let Ok(stream) = tokio::net::UnixStream::connect(uds).await
    {
        let req = "ws://localhost/"
            .into_client_request()
            .map_err(|e| format!("bad uds ws request: {e}"))?;
        let (ws, _) = tokio_tungstenite::client_async(req, stream)
            .await
            .map_err(|e| format!("ws handshake over uds: {e}"))?;
        return finish_control(ws.split(), action).await;
    }
    let url = format!("ws://127.0.0.1:{}/", info.port);
    let mut req = url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("bad ws url {url}: {e}"))?;
    if let Some(token) = &info.token {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| format!("bad bearer token: {e}"))?;
        req.headers_mut().insert("Authorization", value);
    }
    let (ws, _) = tokio_tungstenite::connect_async(req)
        .await
        .map_err(|e| format!("ws connect to {url}: {e}"))?;
    finish_control(ws.split(), action).await
}

/// The stream-generic control handshake: send the `Select{Control}` frame and
/// await the single `ControlReply`. One verb per connection.
async fn finish_control<S>(
    parts: (
        futures::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, WsMessage>,
        futures::stream::SplitStream<tokio_tungstenite::WebSocketStream<S>>,
    ),
    action: AttachAction,
) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut ws_sink, mut ws_source) = parts;
    // Control verbs carry their own scope (`CreateSession::project`); the
    // daemon never consults a select-level project for them.
    let select = serde_json::to_string(&Wire::Select {
        action,
        project: None,
        version: Some(crate::serve::daemon_version().to_string()),
    })
    .map_err(|e| format!("serialize control select: {e}"))?;
    ws_sink
        .send(WsMessage::Text(select.into()))
        .await
        .map_err(|e| format!("ws send control select: {e}"))?;

    tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        loop {
            match ws_source.next().await {
                Some(Ok(WsMessage::Text(text))) => match serde_json::from_str::<Wire>(&text) {
                    Ok(Wire::ControlReply { ok, error, .. }) => {
                        return if ok {
                            Ok(())
                        } else {
                            Err(error.unwrap_or_else(|| "control verb rejected".to_string()))
                        };
                    }
                    Ok(Wire::Error { message, .. }) => return Err(message),
                    Ok(_) => tracing::warn!("control: unexpected frame during handshake, ignored"),
                    Err(error) => tracing::warn!(%error, "control: bad frame during handshake"),
                },
                Some(Ok(_)) => {}
                Some(Err(error)) => return Err(format!("ws recv during control: {error}")),
                None => return Err("server closed the control connection".to_string()),
            }
        }
    })
    .await
    .map_err(|_| "timed out waiting for control reply from daemon".to_string())?
}

struct Welcome {
    session_id: String,
    round_counter: u64,
    messages: Vec<Message>,
    provider: String,
    model: String,
}
enum Reply {
    Welcome(Welcome),
    Pick(Vec<SessionOverview>),
}

// ---- Monitor-protocol client (ADR-0093) ----
/// Open the WebSocket, perform the monitor handshake, and return a channel of
/// stream frames. The WS pump runs on a background task; the channel closes
/// when the daemon hangs up.
pub async fn monitor_stream(
    info: &DaemonInfo,
    action: MonitorAction,
) -> Result<tokio::sync::mpsc::UnboundedReceiver<MonitorEvent>, String> {
    // Prefer the Unix domain socket (the daemon's primary local channel,
    // ADR-0096); fall back to TCP for exposed/legacy deployments — the same
    // transport policy as `remote::connect`/`remote::control`, so the monitor
    // stream works against a UDS-only daemon.
    #[cfg(unix)]
    if let Some(uds) = &info.uds_path
        && let Ok(stream) = tokio::net::UnixStream::connect(uds).await
    {
        let request = "ws://localhost/"
            .into_client_request()
            .map_err(|e| format!("bad uds ws request: {e}"))?;
        let (ws, _) = tokio_tungstenite::client_async(request, stream)
            .await
            .map_err(|e| format!("ws handshake over uds: {e}"))?;
        return finish_monitor(ws.split(), action, "uds").await;
    }
    let url = format!("ws://127.0.0.1:{}/", info.port);
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("bad ws url {url}: {e}"))?;
    if let Some(token) = &info.token {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| format!("bad bearer token: {e}"))?;
        request.headers_mut().insert("Authorization", value);
    }
    let (ws, _response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| format!("ws connect to {url}: {e}"))?;
    finish_monitor(ws.split(), action, &url).await
}

/// The stream-generic monitor handshake + framing, shared by the UDS and TCP
/// paths: send the `Select{Monitor}` handshake, await the opening snapshot
/// (bounded), then forward every diff frame into the returned channel.
async fn finish_monitor<S>(
    parts: (
        futures::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, WsMessage>,
        futures::stream::SplitStream<tokio_tungstenite::WebSocketStream<S>>,
    ),
    action: MonitorAction,
    target: &str,
) -> Result<tokio::sync::mpsc::UnboundedReceiver<MonitorEvent>, String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut ws_sink, mut ws_source) = parts;

    let select = serde_json::to_string(&Wire::Select {
        action: crate::serve::AttachAction::Monitor(action),
        // Monitor streams are host-wide; no project scope applies.
        project: None,
        version: Some(crate::serve::daemon_version().to_string()),
    })
    .map_err(|e| format!("serialize select: {e}"))?;
    ws_sink
        .send(WsMessage::Text(select.into()))
        .await
        .map_err(|e| format!("ws send select: {e}"))?;

    // Await the opening snapshot (or a handshake-level error) with a bound.
    let first = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        loop {
            match ws_source.next().await {
                Some(Ok(WsMessage::Text(text))) => match serde_json::from_str::<Wire>(&text) {
                    Ok(Wire::Monitor { event }) => return Ok(event),
                    Ok(Wire::Error { message, .. }) => return Err(message),
                    Ok(_) => tracing::warn!("status: unexpected frame during handshake, ignored"),
                    Err(error) => tracing::warn!(%error, "status: bad frame during handshake"),
                },
                Some(Ok(_)) => {}
                Some(Err(error)) => return Err(format!("ws recv during handshake: {error}")),
                None => return Err("server closed the connection".to_string()),
            }
        }
    })
    .await
    .map_err(|_| format!("timed out waiting for monitor snapshot from {target}"))??;

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = tx.send(first);
    tokio::spawn(async move {
        while let Some(frame) = ws_source.next().await {
            match frame {
                Ok(WsMessage::Text(text)) => match serde_json::from_str::<Wire>(&text) {
                    Ok(Wire::Monitor { event }) => {
                        if tx.send(event).is_err() {
                            return;
                        }
                    }
                    Ok(_) => tracing::warn!("status: unexpected post-handshake frame, ignored"),
                    Err(error) => tracing::warn!(%error, "status: bad frame from daemon, ignored"),
                },
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "status: ws recv failed");
                    break;
                }
            }
        }
    });
    Ok(rx)
}

pub fn upsert_session_row(rows: &mut Vec<MonitoredSession>, row: MonitoredSession) {
    match rows.iter_mut().find(|existing| existing.id == row.id) {
        Some(existing) => *existing = row,
        None => rows.push(row),
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.updated_at));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compare_versions() {
        assert_eq!(compare_versions("0.25.0", "0.25.0"), VersionRelation::Equal);
        assert_eq!(
            compare_versions("0.26.0", "0.25.0"),
            VersionRelation::ClientNewer
        );
        assert_eq!(
            compare_versions("0.24.0", "0.25.0"),
            VersionRelation::ClientOlder
        );
        assert_eq!(
            compare_versions("1.0.0", "0.25.0"),
            VersionRelation::ClientNewer
        );
        assert_eq!(
            compare_versions("not-a-semver", "0.25.0"),
            VersionRelation::Unknown
        );
    }

    #[test]
    fn test_version_mismatch_messages() {
        let daemon_older = DaemonInfo {
            pid: 1234,
            port: 9800,
            token: None,
            project_root: String::new(),
            started_at: 0,
            uds_path: None,
            version: Some("0.24.0".to_string()),
        };
        let msg = version_mismatch(&daemon_older);
        assert!(msg.contains("is older than this client"));
        assert!(msg.contains("neenee stop"));

        let daemon_newer = DaemonInfo {
            pid: 1234,
            port: 9800,
            token: None,
            project_root: String::new(),
            started_at: 0,
            uds_path: None,
            version: Some("99.0.0".to_string()),
        };
        let msg = version_mismatch(&daemon_newer);
        assert!(msg.contains("older than the running daemon"));
        assert!(msg.contains("update your neenee client"));

        let daemon_none = DaemonInfo {
            pid: 1234,
            port: 9800,
            token: None,
            project_root: String::new(),
            started_at: 0,
            uds_path: None,
            version: None,
        };
        let msg = version_mismatch(&daemon_none);
        assert!(msg.contains("unknown (older than 0.24)"));
        assert!(msg.contains("neenee stop"));
    }

    fn record(port: u16, token: Option<String>) -> DaemonInfo {
        DaemonInfo {
            pid: 99999999, // Unused/dead pid
            port,
            token,
            project_root: "/tmp/proj".to_string(),
            started_at: 0,
            uds_path: None,
            version: None,
        }
    }
    fn dead_port() -> u16 {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    }
    #[test]
    fn discover_at_returns_none_and_removes_stale_record_for_dead_pid() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("serve.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&record(dead_port(), None)).unwrap(),
        )
        .unwrap();
        assert!(discover_at(&path).is_none());
        assert!(
            !path.exists(),
            "stale discovery file with dead PID must be removed"
        );
    }
    #[test]
    fn discover_at_preserves_record_if_pid_is_still_alive() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("serve.json");
        let live_rec = DaemonInfo {
            pid: std::process::id(),
            port: dead_port(),
            token: None,
            project_root: "/tmp/proj".to_string(),
            started_at: 0,
            uds_path: None,
            version: None,
        };
        std::fs::write(&path, serde_json::to_vec(&live_rec).unwrap()).unwrap();
        assert!(discover_at(&path).is_none());
        assert!(
            path.exists(),
            "discovery file for living PID must NOT be deleted on transient probe fail"
        );
    }
    #[test]
    fn discover_at_tolerates_missing_and_corrupt_files() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("serve.json");
        assert!(discover_at(&path).is_none());
        std::fs::write(&path, b"not json").unwrap();
        assert!(discover_at(&path).is_none());
        assert!(path.exists());
    }

    #[tokio::test]
    async fn stop_handles_already_dead_process_and_cleans_up() {
        let info = DaemonInfo {
            pid: 99999999, // Unused pid
            port: 1,
            token: None,
            project_root: String::new(),
            started_at: 0,
            uds_path: None,
            version: Some("0.24.0".to_string()),
        };
        let res = stop(&info).await;
        assert!(res.is_ok());
    }
}
