//! Attach-mode client: `neenee` / `neenee attach` drive sessions owned by
//! the unified session daemon (`neenee-server`; ADR-0096). Discovery is
//! global (one daemon per user); connection prefers the Unix domain socket
//! and falls back to TCP.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use neenee_core::{AgentRequest, AgentResponse, Message, SessionOverview};
use neenee_transport::serve::Wire;
use neenee_transport::serve_discovery as discovery;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

pub use neenee_transport::serve::AttachAction;
pub use neenee_transport::serve_discovery::Discovery as ServeInfo;

const LIVENESS_TIMEOUT: Duration = Duration::from_millis(200);
const SERVER_START_TIMEOUT: Duration = Duration::from_secs(10);
const SERVER_START_POLL: Duration = Duration::from_millis(100);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Find the unified daemon (ADR-0096): one global record. The project
/// argument is accepted for source compatibility but no longer scopes the
/// lookup — the daemon serves every project.
pub fn discover(_project_root: &Path) -> Option<ServeInfo> {
    discover_at(&discovery::global_discovery_path())
}

fn discover_at(path: &Path) -> Option<ServeInfo> {
    let bytes = std::fs::read(path).ok()?;
    let info: ServeInfo = serde_json::from_slice(&bytes).ok()?;
    if is_alive(&info) {
        Some(info)
    } else {
        discovery::remove(path);
        None
    }
}

/// Liveness probe: prefer the UDS (the daemon's primary local channel),
/// fall back to the TCP port. Either reachable means the daemon is up.
fn is_alive(info: &ServeInfo) -> bool {
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

pub async fn ensure_server(project_root: &Path) -> Result<ServeInfo, String> {
    if let Some(info) = discover(project_root) {
        return Ok(info);
    }
    spawn_server()?;
    let deadline = std::time::Instant::now() + SERVER_START_TIMEOUT;
    loop {
        tokio::time::sleep(SERVER_START_POLL).await;
        if let Some(info) = discover(project_root) {
            return Ok(info);
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "timed out after {}s waiting for neenee-server to start",
                SERVER_START_TIMEOUT.as_secs(),
            ));
        }
    }
}

fn spawn_server() -> Result<(), String> {
    let program = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("neenee-server")))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("neenee-server"));
    let mut command = std::process::Command::new(&program);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
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

pub async fn connect(info: &ServeInfo, action: AttachAction) -> Result<Handshake, String> {
    // Prefer the Unix domain socket (the daemon's primary local channel,
    // ADR-0096); fall back to TCP for exposed/legacy deployments.
    #[cfg(unix)]
    if let Some(uds) = &info.uds_path {
        if let Ok(stream) = tokio::net::UnixStream::connect(uds).await {
            let request = "ws://localhost/"
                .into_client_request()
                .map_err(|e| format!("bad uds ws request: {e}"))?;
            let (ws, _) = tokio_tungstenite::client_async(request, stream)
                .await
                .map_err(|e| format!("ws handshake over uds: {e}"))?;
            return finish_handshake(ws.split(), action).await;
        }
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

    let select = serde_json::to_string(&Wire::Select { action })
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
                    Ok(Wire::Error { message }) => {
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
    info: &ServeInfo,
    request: neenee_transport::serve::ControlRequest,
) -> Result<(), String> {
    use neenee_transport::serve::AttachAction;
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
    let select = serde_json::to_string(&Wire::Select { action })
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
                    Ok(Wire::Error { message }) => return Err(message),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn record(port: u16, token: Option<String>) -> ServeInfo {
        ServeInfo {
            pid: std::process::id(),
            port,
            token,
            project_root: "/tmp/proj".to_string(),
            started_at: 0,
            uds_path: None,
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
