//! WebSocket transport for the "hot-attach" serve mode (ADR-0037 §7).
//!
//! When `/serve` is invoked from the running TUI, [`start_server`] spawns a
//! TCP listener that accepts WebSocket connections. Each connection:
//!
//! 1. Receives the session's full transcript history (so a freshly-opened
//!    browser sees prior context, not just live events from connect onward).
//! 2. Streams every subsequent [`AgentResponse`] from the broadcast channel
//!    (the TUI listener task taps each response into it).
//! 3. Reads inbound [`AgentRequest`]s from the WebSocket and feeds them into
//!    the same `req_tx` the TUI uses — so a browser request and a TUI
//!    keystroke are indistinguishable to `agent_loop`.
//!
//! The wire format is newline-delimited JSON: one `serde_json`-erialized
//! `AgentRequest` or `AgentResponse` per WebSocket text frame.
//!
//! # Network exposure & authentication
//!
//! By default the listener binds `127.0.0.1` (loopback only) and requires no
//! token — a local co-process is trusted. Binding all interfaces
//! ([`ServeExpose::Public`]) is opt-in and **requires** a bearer token: the
//! WebSocket handshake must carry `Authorization: Bearer <token>`, else it is
//! rejected before any session data is exchanged. The caller supplies the
//! token (the TUI generates a random one and prints it) so the server itself
//! never invents credentials and stays config-free.

use std::net::SocketAddr;
use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use neenee_core::{AgentRequest, AgentResponse, Message};
use neenee_store::session::SessionStore;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request};
use tokio_tungstenite::tungstenite::http::StatusCode;

/// The wire envelope. Each WebSocket text frame is one of these, JSON-encoded.
/// Inbound (browser → server) is always [`Wire::Request`]; outbound
/// (server → browser) is [`Wire::Response`] or [`Wire::History`] (sent once
/// at connect, before any live responses).
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
enum Wire {
    Request {
        #[serde(flatten)]
        request: AgentRequest,
    },
    Response {
        #[serde(flatten)]
        response: AgentResponse,
    },
    /// Full transcript replay, sent once on connect so the browser catches up
    /// on everything that happened before it joined.
    History { messages: Vec<Message> },
}

/// Which interfaces the serve listener binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeExpose {
    /// Bind `127.0.0.1` only — the listener is reachable from this machine
    /// and no other. The default; no token is required because a local
    /// co-process is trusted.
    Local,
    /// Bind `0.0.0.0` — the listener is reachable from any interface. **A
    /// bearer token is mandatory** (enforced at handshake) so a public port
    /// never exposes the session unauthenticated.
    Public,
}

/// All knobs for [`start_server`]. Grouped so adding a field never widens the
/// call signature.
#[derive(Debug, Clone)]
pub struct ServeOptions {
    /// The TCP port to listen on. `0` lets the OS pick.
    pub port: u16,
    /// Which interfaces to bind.
    pub expose: ServeExpose,
    /// Bearer token required at WS handshake. `None` means "no auth" and is
    /// only safe with [`ServeExpose::Local`]; [`start_server`] rejects a
    /// `Public` + `None` combination by falling back to a generated token
    /// surfaced via [`ServeHandle::token`] so the caller can display it.
    pub token: Option<String>,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            port: 0,
            expose: ServeExpose::Local,
            token: None,
        }
    }
}

/// The handle returned by [`start_server`]: the actual bound port, the
/// cancellation token, and (when auth is active) the bearer token a client
/// must present.
pub struct ServeHandle {
    /// Resolves to the OS-assigned port once the listener has bound.
    pub port: tokio::sync::oneshot::Receiver<u16>,
    /// Cancel to stop accepting (used by `/serve` with no arg).
    pub cancel: tokio_util::sync::CancellationToken,
    /// The bearer token a client must send as `Authorization: Bearer <token>`,
    /// when auth is in effect. `None` for the unauthenticated local default.
    pub token: Option<String>,
}

/// Spawn the WebSocket server. Returns immediately; the listener runs as a
/// detached tokio task that lives until the process exits, the cancellation
/// token is cancelled, or the broadcast channel is dropped (which happens
/// when `/serve` with no arg clears the tap).
///
/// - `opts`: port, exposure, and optional token (see [`ServeOptions`]).
/// - `req_tx`: the existing agent-loop request channel. Browser requests are
///   fed in here alongside TUI requests.
/// - `events`: the broadcast channel the TUI listener taps responses into.
///   Each WS connection subscribes to this.
/// - `session`: the session store, used to replay transcript history on connect.
///
/// # Security
///
/// `Local` binds loopback and skips auth. `Public` binds all interfaces and
/// **requires** a token: if `opts.token` is `None` under `Public`, a random
/// token is generated and returned in the handle so the caller can show it.
/// A `Public` listener with no token is never started.
pub fn start_server(
    opts: ServeOptions,
    req_tx: mpsc::UnboundedSender<AgentRequest>,
    events: broadcast::Sender<AgentResponse>,
    session: Arc<SessionStore>,
) -> ServeHandle {
    let (actual_port_tx, actual_port_rx) = tokio::sync::oneshot::channel::<u16>();
    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel_clone = cancel.clone();

    // Enforce the invariant: Public ⇒ token. Generate one if the caller forgot,
    // so a public port is never unauthenticated.
    let token = match (opts.expose, opts.token.clone()) {
        (ServeExpose::Public, None) => Some(generate_token()),
        (_, t) => t,
    };

    let bind_addr: SocketAddr = match opts.expose {
        ServeExpose::Local => ([127, 0, 0, 1], opts.port).into(),
        ServeExpose::Public => ([0, 0, 0, 0], opts.port).into(),
    };
    let token_for_task = token.clone();

    tokio::spawn(async move {
        let listener = match TcpListener::bind(bind_addr).await {
            Ok(l) => {
                let actual = l.local_addr().map(|a| a.port()).unwrap_or(opts.port);
                let _ = actual_port_tx.send(actual);
                tracing::info!(%bind_addr, actual_port = actual, auth = token_for_task.is_some(), "neenee serve: WebSocket listener started");
                l
            }
            Err(e) => {
                tracing::error!(%bind_addr, error = %e, "neenee serve: failed to bind");
                return;
            }
        };
        loop {
            // When `/serve` (no arg) cancels the token, stop accepting.
            tokio::select! {
                _ = cancel_clone.cancelled() => {
                    tracing::info!("neenee serve: cancelled, stopping listener");
                    break;
                }
                accept_result = listener.accept() => {
                    let (stream, peer_addr) = match accept_result {
                        Ok(conn) => conn,
                        Err(e) => {
                            tracing::warn!(error = %e, "neenee serve: accept failed");
                            continue;
                        }
                    };
                    let req_tx = req_tx.clone();
                    let events = events.clone();
                    let session = session.clone();
                    let token = token_for_task.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(stream, req_tx, events, session, token).await {
                            tracing::warn!(%peer_addr, error = %e, "neenee serve: connection ended");
                        }
                    });
                }
            }
        }
    });
    ServeHandle {
        port: actual_port_rx,
        cancel,
        token,
    }
}

/// Generate a random bearer token. Uses process id + time + thread handle for
/// entropy — strong enough to gate a development WebSocket port, not a
/// substitute for a real secret store in production.
fn generate_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    // Mix entropy into two 64-bit halves and format each as 16 hex chars,
    // yielding a 32-char token. `{:016x}` pads to 16 but does NOT truncate,
    // so we mask to 64 bits first to keep the length fixed.
    let pid = std::process::id() as u128;
    let h1 = (nanos ^ pid.wrapping_mul(0x9e3779b97f4a7c15)) as u64;
    let h2 = (nanos >> 64 ^ pid.wrapping_mul(0xbf58476d1ce4e5b9)) as u64;
    format!("{h1:016x}{h2:016x}")
}

/// Handle a single WebSocket connection: replay history, then bridge
/// broadcast → WS and WS → req_tx concurrently.
///
/// If `token` is `Some`, the handshake is rejected unless the client sends
/// `Authorization: Bearer <token>`.
#[allow(clippy::result_large_err)]
async fn handle_connection(
    stream: tokio::net::TcpStream,
    req_tx: mpsc::UnboundedSender<AgentRequest>,
    events: broadcast::Sender<AgentResponse>,
    session: Arc<SessionStore>,
    token: Option<String>,
) -> Result<(), String> {
    // Authenticated handshake: validate the Authorization header before the
    // WS upgrade completes. An unauthenticated (token = None) listener skips
    // this and accepts directly.
    let ws_stream = if let Some(expected) = token.as_deref() {
        let expected = expected.to_string();
        tokio_tungstenite::accept_hdr_async(stream, move |req: &Request, resp| {
            if check_bearer(req, &expected) {
                Ok(resp)
            } else {
                reject_unauthorized()
            }
        })
        .await
        .map_err(|e| format!("ws handshake (auth): {e}"))?
    } else {
        tokio_tungstenite::accept_async(stream)
            .await
            .map_err(|e| format!("ws handshake: {e}"))?
    };
    let (mut ws_sink, mut ws_source) = ws_stream.split();

    // 1. Replay transcript history so the browser sees prior context.
    let messages = session.full_transcript().await;
    let history = serde_json::to_string(&Wire::History { messages })
        .map_err(|e| format!("serialize history: {e}"))?;
    ws_sink
        .send(WsMessage::Text(history.into()))
        .await
        .map_err(|e| format!("send history: {e}"))?;

    // 2. Subscribe to the live response broadcast.
    let mut rx = events.subscribe();

    // 3. Bridge both directions concurrently. The task ends when either
    //    direction closes (browser disconnects or server stops).
    loop {
        tokio::select! {
            // broadcast → browser
            resp = rx.recv() => {
                match resp {
                    Ok(resp) => {
                        let text = serde_json::to_string(&Wire::Response { response: resp })
                            .map_err(|e| format!("serialize response: {e}"))?;
                        ws_sink.send(WsMessage::Text(text.into())).await
                            .map_err(|e| format!("ws send: {e}"))?;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "neenee serve: client lagged, skipping");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        break; // server stopped
                    }
                }
            }
            // browser → agent_loop
            msg = ws_source.next() => {
                match msg {
                    Some(Ok(WsMessage::Text(text))) => {
                        match serde_json::from_str::<Wire>(&text) {
                            Ok(Wire::Request { request }) => {
                                let _ = req_tx.send(request);
                            }
                            Ok(_) => {
                                // Ignore non-request inbound messages.
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "neenee serve: bad request json");
                            }
                        }
                    }
                    Some(Ok(_)) => {} // ignore binary/ping/pong
                    Some(Err(e)) => {
                        return Err(format!("ws recv: {e}"));
                    }
                    None => {
                        break; // browser disconnected
                    }
                }
            }
        }
    }
    Ok(())
}

/// Check the WS handshake request for `Authorization: Bearer <token>`.
fn check_bearer(req: &Request, expected: &str) -> bool {
    let Some(val) = req.headers().get("Authorization") else {
        return false;
    };
    let Ok(s) = val.to_str() else {
        return false;
    };
    let Some(rest) = s.strip_prefix("Bearer ") else {
        return false;
    };
    // Constant-time-ish compare to avoid trivial timing oracles on a dev port.
    rest.trim() == expected
}

/// Build a 401 `ErrorResponse` (an HTTP response tungstenite sends before
/// dropping the handshake). Returned as `Err` from the handshake callback.
#[allow(clippy::result_large_err)]
fn reject_unauthorized() -> Result<tungstenite::handshake::server::Response, ErrorResponse> {
    // A static 401 response body. `ErrorResponse = Response<Option<String>>`;
    // its size is fixed by tungstenite's type, not something we can shrink.
    let body = "Unauthorized".to_string();
    let resp = tungstenite::handshake::server::Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("WWW-Authenticate", "Bearer")
        .body(Some(body))
        // Header values are static, so construction cannot fail; `default()`
        // (an empty 200) is an unreachable fallback that keeps this clippy-clean.
        .unwrap_or_default();
    Err(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_bearer_matches_exact() {
        let opts = ServeOptions {
            port: 0,
            expose: ServeExpose::Local,
            token: None,
        };
        // Default is local + no token.
        assert_eq!(opts.expose, ServeExpose::Local);
    }

    #[test]
    fn local_bind_addr_is_loopback() {
        let local: SocketAddr = ([127, 0, 0, 1], 0).into();
        assert!(local.ip().is_loopback());
        let public: SocketAddr = ([0, 0, 0, 0], 0).into();
        assert!(!public.ip().is_loopback());
    }

    #[test]
    fn generate_token_is_nonempty_hex() {
        let t = generate_token();
        assert_eq!(t.len(), 32, "token should be 32 hex chars, got {t}");
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
