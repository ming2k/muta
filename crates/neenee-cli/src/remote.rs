//! Attach-mode client: `neenee --attach` drives a session hosted by a running
//! `neenee-server` instead of assembling a local harness (ADR-0037 §7).
//!
//! Three pieces:
//!
//! - [`ServeInfo`] / [`discover`]: find the project's live server through the
//!   shared discovery record ([`neenee_transport::serve_discovery`]) — the
//!   same module the server writes with, so reader and writer can never drift.
//! - [`ensure_server`]: discover, or spawn a `neenee-server` for the project
//!   when none is running (one server per project bucket in v1).
//! - [`connect`]: the WebSocket client. It performs the handshake (bearer
//!   token when the record carries one), consumes the one-shot
//!   [`Wire::History`] frame, then bridges the socket to the same
//!   `AgentRequest`/`AgentResponse` channel pair the standalone TUI uses — so
//!   attach mode reuses the TUI unchanged, and requests from every attached
//!   client are indistinguishable to the session driver.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use neenee_core::{AgentRequest, AgentResponse, Message};
use neenee_transport::serve::Wire;
use neenee_transport::serve_discovery as discovery;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// The discovery record a running `neenee-server` publishes. Re-exported from
/// the transport crate (where the server-side writer also lives) so the wire
/// format has exactly one definition.
pub use neenee_transport::serve_discovery::Discovery as ServeInfo;

/// How long [`discover_at`] waits for the liveness TCP probe. Loopback
/// connects are refused instantly when nothing listens, so this only bounds
/// pathological cases (firewalled SYN drops).
const LIVENESS_TIMEOUT: Duration = Duration::from_millis(200);

/// How long [`ensure_server`] waits for a freshly spawned server to write its
/// discovery record before giving up.
const SERVER_START_TIMEOUT: Duration = Duration::from_secs(10);

/// How often [`ensure_server`] re-checks for the discovery record.
const SERVER_START_POLL: Duration = Duration::from_millis(100);

/// How long [`connect`] waits for the server's `History` frame. The server
/// sends it immediately after the upgrade; a longer wait means a wedged peer.
const HISTORY_TIMEOUT: Duration = Duration::from_secs(10);

/// Find the live session server for `project_root`, if any. Reads the
/// discovery record via the transport crate's path resolution and validates
/// liveness; a stale record is removed and reported as `None`.
pub fn discover(project_root: &Path) -> Option<ServeInfo> {
    discover_at(&discovery::discovery_path(project_root))
}

/// [`discover`] against an explicit record path, split out so tests never
/// touch the process-wide XDG dirs.
fn discover_at(path: &Path) -> Option<ServeInfo> {
    let bytes = std::fs::read(path).ok()?;
    // An unparseable record is left in place (a newer/older version may have
    // written it); it only means "no usable server found here".
    let info: ServeInfo = serde_json::from_slice(&bytes).ok()?;
    // Liveness probe: a live server accepts TCP on its advertised port. This
    // cannot distinguish "our server" from an unrelated process that reused
    // the port after the server died — the WS handshake in [`connect`] is the
    // real validation; this probe only prunes the common stale-file case.
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], info.port));
    if std::net::TcpStream::connect_timeout(&addr, LIVENESS_TIMEOUT).is_ok() {
        Some(info)
    } else {
        discovery::remove(path);
        None
    }
}

/// Resolve the session server for `project_root`, spawning `neenee-server`
/// when none is running. When `session_id` is given, the running/spawned
/// server must host that session — v1 supports one server per project bucket,
/// so a server hosting a *different* session is an error, not a second spawn.
pub async fn ensure_server(
    project_root: &Path,
    session_id: Option<&str>,
    autopilot: bool,
) -> Result<ServeInfo, String> {
    if let Some(info) = discover(project_root) {
        if let Some(want) = session_id
            && info.session_id != want
        {
            return Err(format!(
                "a session server is already running for this project, hosting session {} \
                 (pid {}, port {}).\nOnly one server per project is supported for now — stop it \
                 first (kill {}) before attaching to session {want}.",
                info.session_id, info.pid, info.port, info.pid,
            ));
        }
        return Ok(info);
    }

    spawn_server(project_root, session_id, autopilot)?;

    // The server writes its discovery record only after the listener has
    // bound (which follows full harness assembly), so poll for the file.
    let deadline = std::time::Instant::now() + SERVER_START_TIMEOUT;
    loop {
        tokio::time::sleep(SERVER_START_POLL).await;
        if let Some(info) = discover(project_root) {
            if let Some(want) = session_id
                && info.session_id != want
            {
                // The server came up but could not resume the requested id
                // (bootstrap falls back to a fresh session on resume failure).
                return Err(format!(
                    "the session server started but hosts session {} instead of {want} — \
                     the requested session could not be resumed (unknown id?).\nStop the server \
                     (kill {}) or attach to {} instead.",
                    info.session_id, info.pid, info.session_id,
                ));
            }
            return Ok(info);
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "timed out after {}s waiting for neenee-server to start \
                 (no discovery file appeared); check NEENEE_LOG output for spawn errors",
                SERVER_START_TIMEOUT.as_secs(),
            ));
        }
    }
}

/// Spawn `neenee-server` for `project_root`, detached: stdio to null and the
/// `Child` handle dropped without `wait`, so the server outlives this client
/// by design (it owns the session lifecycle; clients come and go).
fn spawn_server(
    project_root: &Path,
    session_id: Option<&str>,
    autopilot: bool,
) -> Result<(), String> {
    // Prefer the binary installed next to this executable (same install set ⇒
    // matching wire protocol), falling back to PATH lookup.
    let program = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("neenee-server")))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("neenee-server"));
    let mut command = std::process::Command::new(&program);
    command
        .arg("--project")
        .arg(project_root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(id) = session_id {
        command.arg("--session").arg(id);
    }
    if autopilot {
        command.arg("--autopilot");
    }
    command
        .spawn()
        .map_err(|error| format!("could not spawn {}: {error}", program.display()))?;
    Ok(())
}

/// Connect to the server described by `info` and bridge the socket to the
/// channel pair the TUI drives. Returns the request sender, the response
/// receiver, the id of the session the server actually hosts (learned from
/// the `History` frame — it may differ from a requested id that failed to
/// resume), its authoritative round counter, and the transcript replayed at
/// handshake.
///
/// When the socket closes (server shutdown, network loss), the response
/// receiver ends after a final [`AgentResponse::Exit`] so the TUI winds down
/// through its ordinary `/exit` path instead of hanging on a dead channel;
/// when the TUI exits, the request side closes and the server sees an
/// ordinary client disconnect.
pub async fn connect(
    info: &ServeInfo,
) -> Result<
    (
        mpsc::UnboundedSender<AgentRequest>,
        mpsc::UnboundedReceiver<AgentResponse>,
        String,
        u64,
        Vec<Message>,
    ),
    String,
> {
    let url = format!("ws://127.0.0.1:{}/", info.port);
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("bad ws url {url}: {e}"))?;
    if let Some(token) = &info.token {
        // Tokens the serve layer generates are hex; any header-invalid byte
        // would surface here as an error rather than a malformed request.
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| format!("bad bearer token in discovery record: {e}"))?;
        request.headers_mut().insert("Authorization", value);
    }
    let (ws, _response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| format!("ws connect to {url}: {e}"))?;
    let (mut ws_sink, mut ws_source) = ws.split();

    // The server replays the transcript exactly once, immediately after the
    // upgrade. Read until it arrives; anything else before it is ignored.
    let (session_id, round_counter, history) = tokio::time::timeout(HISTORY_TIMEOUT, async {
        loop {
            match ws_source.next().await {
                Some(Ok(WsMessage::Text(text))) => match serde_json::from_str::<Wire>(&text) {
                    Ok(Wire::History {
                        session_id,
                        round_counter,
                        messages,
                    }) => break Ok((session_id, round_counter, messages)),
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(%error, "attach: bad frame while waiting for history")
                    }
                },
                Some(Ok(_)) => {} // binary/ping/pong
                Some(Err(error)) => return Err(format!("ws recv before history: {error}")),
                None => {
                    return Err("server closed the connection before sending history".to_string())
                }
            }
        }
    })
    .await
    .map_err(|_| format!("timed out waiting for history from {url}"))??;

    // Bridge: TUI-shaped channels on this side, `Wire` frames on the other.
    let (req_out_tx, mut req_out_rx) = mpsc::unbounded_channel::<AgentRequest>();
    let (resp_in_tx, resp_in_rx) = mpsc::unbounded_channel::<AgentResponse>();

    // Task A: local AgentRequests → `Wire::Request` text frames. Ends when the
    // TUI drops its sender (exit), then closes the socket so the server sees
    // an ordinary disconnect.
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
                tracing::warn!(%error, "attach: ws send failed, closing request bridge");
                break;
            }
        }
        let _ = ws_sink.close().await;
    });

    // Task B: inbound `Wire::Response` frames → the TUI's response channel.
    // A second History frame would mean a server bug; log-and-ignore. When
    // the socket ends, deliver a final Exit so the TUI shuts down like it
    // would on `/exit`, then drop the channel.
    tokio::spawn(async move {
        while let Some(frame) = ws_source.next().await {
            match frame {
                Ok(WsMessage::Text(text)) => match serde_json::from_str::<Wire>(&text) {
                    Ok(Wire::Response { response }) => {
                        if resp_in_tx.send(response).is_err() {
                            return; // TUI gone; no Exit needed.
                        }
                    }
                    Ok(Wire::History { .. }) => {
                        tracing::warn!("attach: unexpected post-handshake history frame, ignored")
                    }
                    Ok(Wire::Request { .. }) => {
                        tracing::warn!("attach: server sent a request frame, ignored")
                    }
                    Err(error) => {
                        tracing::warn!(%error, "attach: bad frame from server, ignored")
                    }
                },
                Ok(_) => {} // binary/ping/pong/close handled by tungstenite
                Err(error) => {
                    tracing::warn!(%error, "attach: ws recv failed, closing response bridge");
                    break;
                }
            }
        }
        let _ = resp_in_tx.send(AgentResponse::Exit);
    });

    Ok((req_out_tx, resp_in_rx, session_id, round_counter, history))
}

#[cfg(test)]
mod tests {
    use super::*;
    use neenee_core::Role;
    use neenee_persistence::session::SessionStore;
    use neenee_transport::serve::{ServeExpose, ServeOptions, start_server};
    use std::sync::Arc;
    use tokio::sync::broadcast;

    fn record(port: u16, token: Option<String>) -> ServeInfo {
        ServeInfo {
            pid: std::process::id(),
            port,
            token,
            session_id: "sess-x".to_string(),
            project_root: "/tmp/proj".to_string(),
            started_at: 0,
        }
    }

    /// A port nothing listens on: bound, read, then released.
    fn dead_port() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    }

    #[test]
    fn discover_at_returns_none_and_removes_stale_record() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("serve.json");
        let bytes = serde_json::to_vec(&record(dead_port(), None)).unwrap();
        std::fs::write(&path, bytes).unwrap();

        assert!(discover_at(&path).is_none(), "a dead port means no server");
        assert!(!path.exists(), "the stale record must be removed");
    }

    #[test]
    fn discover_at_returns_record_when_port_is_live() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("serve.json");
        let bytes = serde_json::to_vec(&record(port, None)).unwrap();
        std::fs::write(&path, bytes).unwrap();

        let info = discover_at(&path).expect("a live port keeps the record");
        assert_eq!(info.port, port);
        assert_eq!(info.session_id, "sess-x");
        assert!(path.exists(), "a live record must be left in place");
    }

    #[test]
    fn discover_at_tolerates_missing_and_corrupt_files() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("serve.json");
        assert!(discover_at(&path).is_none(), "missing file → None");

        std::fs::write(&path, b"not json").unwrap();
        assert!(discover_at(&path).is_none(), "corrupt file → None");
        assert!(
            path.exists(),
            "a corrupt record is left alone (only stale ones are pruned)"
        );
    }

    /// In-process serve harness: a real `start_server` over a throwaway
    /// `SessionStore::for_path`, driven through the real client bridge.
    /// `for_path` derives every on-disk location from the given path, so the
    /// test never touches the process-wide XDG dirs.
    async fn serve_harness(
        token: Option<String>,
    ) -> (
        tempfile::TempDir,
        Arc<SessionStore>,
        mpsc::UnboundedReceiver<AgentRequest>,
        broadcast::Sender<AgentResponse>,
        u16,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let session = Arc::new(SessionStore::for_path(tmp.path().join("session.json")));
        let (req_tx, req_rx) = mpsc::unbounded_channel::<AgentRequest>();
        let (bc_tx, _) = broadcast::channel::<AgentResponse>(1024);
        let handle = start_server(
            ServeOptions {
                port: 0,
                expose: ServeExpose::Local,
                token,
            },
            req_tx,
            bc_tx.clone(),
            session.clone(),
        );
        let port = handle.port.await.unwrap();
        (tmp, session, req_rx, bc_tx, port)
    }

    #[tokio::test]
    async fn connect_replays_handshake_and_bridges_both_directions() {
        let (tmp, session, mut req_rx, bc_tx, port) = serve_harness(None).await;
        session
            .replace_messages(vec![
                Message::new(Role::User, "earlier question"),
                Message::new(Role::Assistant, "earlier answer"),
            ])
            .await
            .unwrap();
        session.set_round_counter(12).await.unwrap();
        let session_id = session.id().await;

        let info = record(port, None);
        let (tx, mut rx, attached_id, round_counter, history) = connect(&info).await.unwrap();

        // The handshake teaches the client the hosted session id + transcript.
        assert_eq!(attached_id, session_id);
        assert_eq!(round_counter, 12);
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].role, Role::User);
        assert_eq!(history[0].content, "earlier question");
        assert_eq!(history[1].content, "earlier answer");

        // Client → server: a request through the bridge lands on the harness's
        // request channel (indistinguishable from a local one).
        tx.send(AgentRequest::Chat {
            text: "hello from attach test".to_string(),
            images: vec![],
            sent_at_ms: None,
        })
        .unwrap();
        let arrived = tokio::time::timeout(Duration::from_secs(2), req_rx.recv())
            .await
            .expect("request must arrive")
            .expect("req channel open");
        assert!(
            matches!(arrived, AgentRequest::Chat { text, .. } if text == "hello from attach test")
        );

        // Server → client: a broadcast response lands on the client receiver.
        let _ = bc_tx.send(AgentResponse::Round {
            session_id: session_id.clone(),
            event: neenee_core::RoundEvent::Text("hello back".to_string()),
        });
        let got = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("response must arrive")
            .expect("resp channel open");
        assert!(
            matches!(got, AgentResponse::Round { event: neenee_core::RoundEvent::Text(t), .. } if t == "hello back")
        );

        drop(tmp);
    }

    #[tokio::test]
    async fn connect_enforces_the_bearer_token() {
        let (_tmp, session, _req_rx, _bc_tx, port) = serve_harness(Some("sekret".to_string())).await;
        let session_id = session.id().await;

        // No token → the handshake is rejected before any session data flows.
        assert!(connect(&record(port, None)).await.is_err());
        // Wrong token → same.
        assert!(connect(&record(port, Some("nope".to_string()))).await.is_err());
        // The record's token → connects and replays the handshake.
        let (_tx, _rx, attached_id, _round_counter, _history) =
            connect(&record(port, Some("sekret".to_string()))).await.unwrap();
        assert_eq!(attached_id, session_id);
    }
}
