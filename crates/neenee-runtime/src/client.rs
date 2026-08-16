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

const LIVENESS_TIMEOUT: Duration = Duration::from_millis(200);
const SERVER_START_TIMEOUT: Duration = Duration::from_secs(10);
const SERVER_START_POLL: Duration = Duration::from_millis(100);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Find the unified daemon (ADR-0096): one global record. The project
/// argument is accepted for source compatibility but no longer scopes the
/// lookup — the daemon serves every project.
pub fn discover(_project_root: &Path) -> Option<DaemonInfo> {
    discover_at(&discovery::global_discovery_path())
}

fn discover_at(path: &Path) -> Option<DaemonInfo> {
    let bytes = std::fs::read(path).ok()?;
    let info: DaemonInfo = serde_json::from_slice(&bytes).ok()?;
    if !is_alive(&info) {
        discovery::remove(path);
        return None;
    }
    Some(info)
}

/// The actionable version-skew error (ADR-0100 rule 4), naming both builds
/// and the fix. Public so `neenee`-level commands can surface it uniformly
/// wherever a discovered daemon is about to be spoken to.
pub fn version_mismatch(info: &DaemonInfo) -> String {
    let daemon = info
        .version
        .as_deref()
        .unwrap_or("unknown (older than 0.24)");
    format!(
        "client/daemon version mismatch: this neenee is {} but the running daemon (pid {}) is {}. \
         Stop it with `neenee stop` and rerun — the daemon restarts on demand at the new version.",
        crate::serve::daemon_version(),
        info.pid,
        daemon
    )
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

pub async fn ensure_daemon(project_root: &Path) -> Result<DaemonInfo, String> {
    if let Some(info) = discover(project_root) {
        return Ok(info);
    }
    spawn_daemon()?;
    let deadline = std::time::Instant::now() + SERVER_START_TIMEOUT;
    loop {
        tokio::time::sleep(SERVER_START_POLL).await;
        if let Some(info) = discover(project_root) {
            return Ok(info);
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "timed out after {}s waiting for neenee daemon to start",
                SERVER_START_TIMEOUT.as_secs(),
            ));
        }
    }
}

fn spawn_daemon() -> Result<(), String> {
    let program = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("neenee"));
    let mut command = std::process::Command::new(&program);
    command.arg("serve");
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
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
        .map_err(|error| format!("could not spawn {}: {error}", program.display()))?;
    Ok(())
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

    fn record(port: u16, token: Option<String>) -> DaemonInfo {
        DaemonInfo {
            pid: std::process::id(),
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
    fn discover_at_returns_none_and_removes_stale_record() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("serve.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&record(dead_port(), None)).unwrap(),
        )
        .unwrap();
        assert!(discover_at(&path).is_none());
        assert!(!path.exists());
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
}
