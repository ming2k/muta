use futures::{SinkExt, StreamExt};
use muta_contracts::wire::{ERR_PROTOCOL_MISMATCH, ERR_VERSION_MISMATCH};
use muta_contracts::{
    AgentRequest, AgentResponse, MonitorAction, MonitorEvent, PROTOCOL_VERSION, protocol_accepts,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request};
use tokio_tungstenite::tungstenite::http::StatusCode;

use crate::shutdown::ShutdownGate;

// The transport envelope and its constants live in `muta-contracts` since
// ADR-0134 — one serde source of truth for the whole wire surface, next to
// the payload types. Re-exported here so every existing `serve::Wire` /
// `serve::AttachAction` path (tests, examples, the TUI, the CLI) keeps
// working unchanged.
pub use muta_contracts::wire::{AttachAction, ControlRequest, Wire};

/// How long a draining daemon waits, per connection, for the client to
/// complete the closing handshake after it is sent `Close(1001 GoingAway)`
/// (ADR-0101). Clients see a clean disconnect instead of a TCP reset when
/// the process exits.
const DRAIN_CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// Backoff ceiling for persistent accept errors (EMFILE and friends): each
/// consecutive failure doubles the wait, capped here, so a resource-exhausted
/// listener degrades to a slow poll instead of a hot spin. Any successful
/// accept resets it.
const ACCEPT_BACKOFF_CAP: std::time::Duration = std::time::Duration::from_secs(1);

/// Cap on the per-session attach-sync buffer (ADR-0096). Only a handful of
/// distinct state-sync events ever land here (the initial
/// `ContextTokens`/`HarnessState`/`ProviderKeys`/`ProviderPicker` set plus
/// one re-sync per mutation), so a small bound is plenty and a pathological
/// emitter cannot grow it without limit.
pub(crate) const ATTACH_SYNC_BUFFER_CAP: usize = 64;

/// WS keepalive cadence for attach connections (ADR-0113). The daemon pings
/// the peer on this interval; any inbound frame (pong included) refreshes
/// [`WS_PEER_SILENCE_LIMIT`]'s deadline.
const WS_PING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// How long an attach connection may stay completely silent (no inbound
/// frame) before the daemon drops it. Peers that die without a RST — laptop
/// sleep, NAT reaping, killed VM — otherwise park the read half until TCP's
/// own timeout, holding the session's broadcast receiver (and blocking the
/// idle-suspension reaper) for tens of minutes. Three missed pings is the
/// conventional dead-connection verdict.
const WS_PEER_SILENCE_LIMIT: std::time::Duration = std::time::Duration::from_secs(90);

/// Inform a newly attached client that project-authored assets remain
/// quarantined. Trust mutation is intentionally available only through the
/// canonical `/trust` command path, which also reloads every affected
/// consumer atomically.
fn workspace_trust_notice(session_id: &str, project_root: &std::path::Path) -> AgentResponse {
    let round = |event: muta_contracts::RoundEvent| AgentResponse::Round {
        session_id: session_id.to_string(),
        event,
    };
    round(muta_contracts::RoundEvent::Notice(
            muta_contracts::AgentNotice::new(
                muta_contracts::NoticeKind::ReviewAlert,
                muta_contracts::NoticeSeverity::Warning,
                "Workspace contributions unreviewed",
                muta_contracts::NoticeSource::Harness,
            )
            .with_surface(muta_contracts::NoticeSurface::Banner)
            .with_body(format!(
                "This workspace ({}) contains project-authored contributions (skills, MCP, hooks, AGENTS.md). \
                 Run `/trust` to trust all domains, or `/trust mcp` / `/trust skills` for a narrow grant.",
                project_root.display()
            )),
        ))
}


/// Whether `response` is an attach-time state-sync event: one of the
/// startup emissions a client that attaches *after* the session began can
/// never reconstruct (active context projection, harness snapshot, key
/// readiness, picker state). These are buffered per session and replayed to
/// a new client right after the welcome so the TUI hydrates immediately.
pub(crate) fn is_attach_sync_event(response: &AgentResponse) -> bool {
    match response {
        AgentResponse::ProviderKeys(_) | AgentResponse::ProviderPicker(_) => true,
        AgentResponse::Round { event, .. } => matches!(
            event,
            muta_contracts::RoundEvent::ContextTokens(_)
                | muta_contracts::RoundEvent::HarnessState(_)
        ),
        _ => false,
    }
}

/// Drain the attach-sync buffer, returning the buffered events in emission
/// order. Called by the WS attach path right after the welcome.
async fn drain_attach_sync(
    buffer: &tokio::sync::Mutex<std::collections::VecDeque<AgentResponse>>,
) -> Vec<AgentResponse> {
    buffer.lock().await.drain(..).collect()
}

/// Read-only snapshot of the attach-sync buffer for the Lagged resync path:
/// unlike [`drain_attach_sync`] (consumed once by a *new* client), a lagging
/// client's re-anchor must leave the buffer intact for the next attacher.
async fn snapshot_attach_sync(
    buffer: &tokio::sync::Mutex<std::collections::VecDeque<AgentResponse>>,
) -> Vec<AgentResponse> {
    buffer.lock().await.iter().cloned().collect()
}

/// Refuse a wire-protocol-skewed attach (ADR-0134) with an actionable
/// message naming both the protocol window and the product builds. Sent
/// before any session work. Directional: a too-old client hears "update",
/// a too-new client hears "restart the daemon".
fn protocol_mismatch_error(client: u32, client_version: Option<&str>) -> String {
    let window = format!(
        "{}..={}",
        muta_contracts::MIN_PROTOCOL_VERSION,
        PROTOCOL_VERSION
    );
    let builds = match client_version {
        Some(v) => format!(" (client build {v}, daemon build {})", daemon_version()),
        None => format!(" (daemon build {})", daemon_version()),
    };
    if client < muta_contracts::MIN_PROTOCOL_VERSION {
        format!(
            "client/daemon wire protocol mismatch: client protocol {client} is older \
             than the oldest this daemon serves ({window}). \
             Please update your muta client to build {daemon} or newer{builds}.",
            daemon = daemon_version()
        )
    } else {
        format!(
            "client/daemon wire protocol mismatch: client protocol {client} is newer \
             than this daemon's protocol {current}. \
             Stop the daemon and let it restart on demand: `muta daemon stop`, then rerun \
             this command (or `muta daemon start` to bring it up explicitly){builds}.",
            current = PROTOCOL_VERSION
        )
    }
}

/// Refuse a version-skewed attach with an actionable both-versions message
/// (ADR-0100 rule 4), with directional recommendations. Returned before any
/// session work happens. Applies only to clients that predate the protocol
/// field (ADR-0134) — a client that declares a protocol number is judged on
/// the window alone, its product version never enters the decision.
fn version_mismatch_error(client: &str, daemon: &str) -> String {
    use crate::client::{VersionRelation, compare_versions};
    match compare_versions(client, daemon) {
        VersionRelation::ClientOlder => format!(
            "client/daemon version mismatch: client ({client}) is older than daemon ({daemon}). \
             Please update your muta client to version {daemon} or newer."
        ),
        VersionRelation::ClientNewer => format!(
            "client/daemon version mismatch: daemon ({daemon}) is older than client ({client}). \
             Stop the daemon and let it restart on demand: `muta daemon stop`, then rerun \
             this command (or `muta daemon start` to bring it up explicitly)."
        ),
        VersionRelation::Equal | VersionRelation::Unknown => format!(
            "client/daemon version mismatch: client {client} vs daemon {daemon}. \
             Stop the daemon and let it restart on demand: `muta daemon stop`, then rerun \
             this command (or `muta daemon start` to bring it up explicitly)."
        ),
    }
}

/// Per-connection registry (ADR-0101): every accepted socket's task is
/// tracked here with its own cancel token, so a draining daemon can close
/// each live connection with a proper WebSocket `Close(1001 GoingAway)`
/// instead of vanishing under it. Connections remove themselves on exit.
#[derive(Default)]
pub struct ConnTable {
    inner: std::sync::Mutex<ConnTableInner>,
}

#[derive(Default)]
struct ConnTableInner {
    next_id: u64,
    conns: HashMap<u64, CancellationToken>,
}

impl ConnTable {
    pub fn new() -> Self {
        Self::default()
    }

    fn register(&self) -> (u64, CancellationToken) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.next_id += 1;
        let id = inner.next_id;
        let token = CancellationToken::new();
        inner.conns.insert(id, token.clone());
        (id, token)
    }

    fn unregister(&self, id: u64) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .conns
            .remove(&id);
    }

    /// Signal every live connection to close, and wait (bounded) for the
    /// connection tasks to observe it. Best-effort: a client that ignores
    /// the close frame is dropped when the process exits.
    pub async fn drain(&self) {
        let tokens: Vec<CancellationToken> = {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.conns.values().cloned().collect()
        };
        for token in &tokens {
            token.cancel();
        }
        let deadline = tokio::time::Instant::now() + DRAIN_CLOSE_TIMEOUT;
        while tokio::time::Instant::now() < deadline {
            let remaining = {
                let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                inner.conns.len()
            };
            if remaining == 0 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        tracing::warn!(
            connections = self
                .inner
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .conns
                .len(),
            "serve: connections did not close within the drain window"
        );
    }

    /// Number of live connections (the idle-exit probe counts these).
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .conns
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

use std::collections::HashMap;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeExpose {
    Local,
    Public,
}

#[derive(Debug, Clone)]
pub struct ServeOptions {
    pub port: u16,
    pub expose: ServeExpose,
    pub token: Option<String>,
    /// Require a bearer token even on loopback (ADR-0105): when `true` and no
    /// explicit `token` is set, the listener generates one and publishes it
    /// via the discovery record (owner-only, 0600). Defends the control plane
    /// against drive-by connections from other local processes and other
    /// users on a shared machine. The production daemon defaults this on via
    /// `[daemon] local_auth`; the field defaults off so existing
    /// `ServeOptions::default()` tests keep their unauthenticated loopback.
    pub local_auth: bool,
    /// When the requested `port` is taken, fall back to an OS-assigned
    /// ephemeral port (the discovery record then carries the real one) instead
    /// of failing startup. Used by the production daemon, whose CLI default
    /// port is fixed (9800); tests keep the strict default.
    pub port_fallback: bool,
    /// Native local control endpoint. Local IPC is exempt from the bearer
    /// token because its Unix permissions / Windows DACL are the auth boundary.
    pub local_endpoint: Option<muta_platform::ipc::LocalEndpoint>,
}
impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            port: 0,
            expose: ServeExpose::Local,
            token: None,
            local_auth: false,
            port_fallback: false,
            local_endpoint: None,
        }
    }
}

/// What the daemon's startup resolved: the bound endpoints on success, or
/// the failure that must stop the process. Distinct from the pre-0101
/// `oneshot<u16>` (whose drop surfaced as a useless `RecvError` at the top
/// level): the actual `io::Error` travels with the value.
pub struct Startup {
    /// TCP bind result: the bound port, or why the listener could not bind.
    pub port: Option<tokio::sync::oneshot::Receiver<Result<u16, std::io::Error>>>,
    /// Native local-IPC bind result when enabled.
    pub local_ready: Option<
        tokio::sync::oneshot::Receiver<
            Result<Option<muta_platform::ipc::LocalEndpoint>, std::io::Error>,
        >,
    >,
}

/// The destructured [`Startup`] receivers (`Startup::take`).
pub struct StartupParts {
    pub port_rx: tokio::sync::oneshot::Receiver<Result<u16, std::io::Error>>,
    pub local_rx: tokio::sync::oneshot::Receiver<
        Result<Option<muta_platform::ipc::LocalEndpoint>, std::io::Error>,
    >,
}

impl Startup {
    /// Take the receivers out (the run loop awaits them as locals so the
    /// containing `ServeHandle` stays intact for the drain phases).
    pub fn take(&mut self) -> StartupParts {
        StartupParts {
            port_rx: self.port.take().unwrap_or_else(|| {
                tokio::sync::oneshot::channel::<Result<u16, std::io::Error>>().1
            }),
            local_rx: self.local_ready.take().unwrap_or_else(|| {
                tokio::sync::oneshot::channel::<
                    Result<Option<muta_platform::ipc::LocalEndpoint>, std::io::Error>,
                >()
                .1
            }),
        }
    }
}

pub struct ServeHandle {
    pub startup: Startup,
    /// The listeners' cancellation token. Cancelling stops the accept loops
    /// (the loops then exit and are joined through `tasks`).
    pub cancel: CancellationToken,
    /// The connection table: every live connection's cancel token, for the
    /// drain phase (ADR-0101).
    pub conns: Arc<ConnTable>,
    /// Supervised accept tasks; `host::run` joins them during shutdown to
    /// *confirm* the loops exited (and to clean up native local-listener state
    /// deterministically, instead of racing the process end).
    pub tasks: Arc<crate::shutdown::TaskBook>,
    pub token: Option<String>,
    /// The daemon's shutdown gate: `ControlRequest::Shutdown` (the
    /// `muta daemon stop` verb) funnels into it like any other trigger.
    pub gate: Arc<ShutdownGate>,
    /// This daemon build's version, echoed to clients during handshake
    /// version negotiation (ADR-0100 rule 4).
    pub version: &'static str,
}

/// The daemon's own `CARGO_PKG_VERSION`, shared by the discovery record and
/// the handshake refusal message. Every daemon launch path embeds the same
/// workspace version.
pub fn daemon_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn start_server(
    opts: ServeOptions,
    registry: Arc<crate::registry::SessionRegistry>,
) -> ServeHandle {
    let (actual_port_tx, actual_port_rx) =
        tokio::sync::oneshot::channel::<Result<u16, std::io::Error>>();
    let cancel = CancellationToken::new();
    let conns = Arc::new(ConnTable::new());
    let tasks = Arc::new(crate::shutdown::TaskBook::new());
    // The serve gate carries the daemon's version from the start so the
    // handshake refusal names it even when the serve layer runs standalone
    // (host::run attaches its own run-loop gate on top of this one).
    let gate = Arc::new(ShutdownGate::new().with_version(daemon_version()));
    let token = opts.token.clone().or_else(|| match opts.expose {
        ServeExpose::Public => Some(generate_token()),
        // Loopback auth (ADR-0105): generated per start, published owner-only
        // in the discovery record; Rust clients read and present it, browser
        // clients use the `bearer.` subprotocol.
        ServeExpose::Local if opts.local_auth => Some(generate_token()),
        ServeExpose::Local => None,
    });
    let bind_addr: SocketAddr = match opts.expose {
        ServeExpose::Local => ([127, 0, 0, 1], opts.port).into(),
        ServeExpose::Public => ([0, 0, 0, 0], opts.port).into(),
    };
    let port_fallback = opts.port_fallback;
    let expose = opts.expose;
    {
        let cc = cancel.clone();
        let tf = token.clone();
        let registry = registry.clone();
        let conns = conns.clone();
        let tasks = tasks.clone();
        let gate = gate.clone();
        let handle = tokio::spawn(async move {
            let listener = match bind_tcp(bind_addr, port_fallback).await {
                Ok((l, actual)) => {
                    let _ = actual_port_tx.send(Ok(actual));
                    tracing::info!(%bind_addr,actual_port=actual,auth=tf.is_some(),"muta daemon: listener started");
                    l
                }
                Err(e) => {
                    // Surface the real io::Error: the run loop turns it into
                    // a fatal shutdown (readable at the top level), not a
                    // bare RecvError.
                    let _ = actual_port_tx.send(Err(std::io::Error::new(
                        e.kind(),
                        format!("could not bind {bind_addr}: {e}"),
                    )));
                    return;
                }
            };
            // Exponential backoff across *consecutive* accept failures so a
            // resource-exhausted listener (EMFILE) degrades to a slow poll
            // instead of a hot spin; any success resets it.
            let mut backoff = std::time::Duration::from_millis(5);
            loop {
                tokio::select! {_=cc.cancelled()=>{tracing::info!("muta daemon: cancelled");break;}
                ac=listener.accept()=>{let(stream,peer)=match ac{Ok(c)=>c,Err(e)=>{tracing::warn!(error=%e,backoff_ms=backoff.as_millis() as u64,"muta daemon: accept failed");tokio::time::sleep(backoff).await;backoff=(backoff*2).min(ACCEPT_BACKOFF_CAP);continue;}};
                backoff=std::time::Duration::from_millis(5);
                spawn_tcp_connection(stream, registry.clone(), tf.clone(), expose, conns.clone(), gate.clone(), cc.clone(), peer.to_string());}}
            }
        });
        tasks.track("tcp-accept", handle);
    }

    let local_rx = {
        let (local_tx, local_rx) = tokio::sync::oneshot::channel::<
            Result<Option<muta_platform::ipc::LocalEndpoint>, std::io::Error>,
        >();
        if let Some(endpoint) = opts.local_endpoint.clone() {
            let cc = cancel.clone();
            let registry = registry.clone();
            let conns = conns.clone();
            let tasks = tasks.clone();
            let gate = gate.clone();
            let handle = tokio::spawn(async move {
                let mut listener = match muta_platform::ipc::LocalListener::bind(&endpoint) {
                    Ok(l) => {
                        let _ = local_tx.send(Ok(Some(endpoint.clone())));
                        l
                    }
                    Err(e) => {
                        tracing::error!(endpoint=?endpoint,error=%e,"muta daemon: local IPC bind failed");
                        let _ = local_tx.send(Err(e));
                        return;
                    }
                };
                tracing::info!(endpoint=?endpoint,"muta daemon: local IPC listener started");
                let mut backoff = std::time::Duration::from_millis(5);
                loop {
                    tokio::select! {_=cc.cancelled()=>{tracing::info!("muta daemon: local IPC cancelled");break;}
                    ac=listener.accept()=>{let stream=match ac{Ok(c)=>c,Err(e)=>{tracing::warn!(error=%e,backoff_ms=backoff.as_millis() as u64,"muta daemon: local IPC accept failed");tokio::time::sleep(backoff).await;backoff=(backoff*2).min(ACCEPT_BACKOFF_CAP);continue;}};
                    backoff=std::time::Duration::from_millis(5);
                    spawn_connection(stream, registry.clone(), None, ServeExpose::Local, conns.clone(), gate.clone(), cc.clone(), format!("local:{endpoint:?}"));}}
                }
            });
            tasks.track("local-ipc-accept", handle);
        } else {
            let _ = local_tx.send(Ok(None));
        }
        local_rx
    };

    ServeHandle {
        startup: Startup {
            port: Some(actual_port_rx),
            local_ready: Some(local_rx),
        },
        cancel,
        conns,
        tasks,
        token,
        gate,
        version: daemon_version(),
    }
}

/// Bind the TCP listener, falling back to an OS-assigned port when the
/// requested one is taken and `port_fallback` is on (the discovery record
/// carries the actual port either way). Returns the listener and the port.
async fn bind_tcp(addr: SocketAddr, port_fallback: bool) -> std::io::Result<(TcpListener, u16)> {
    match TcpListener::bind(addr).await {
        Ok(l) => {
            let actual = l.local_addr().map(|a| a.port()).unwrap_or(addr.port());
            Ok((l, actual))
        }
        Err(e)
            if port_fallback && addr.port() != 0 && e.kind() == std::io::ErrorKind::AddrInUse =>
        {
            tracing::warn!(%addr, "muta daemon: requested port in use; falling back to an ephemeral port");
            let fallback: SocketAddr = (addr.ip(), 0).into();
            let l = TcpListener::bind(fallback).await?;
            let actual = l.local_addr().map(|a| a.port()).unwrap_or(0);
            Ok((l, actual))
        }
        Err(e) => Err(e),
    }
}

/// Spawn one accepted-TCP task: peek at the request head, then dispatch to
/// the health responder or the WebSocket control plane. The classification
/// runs inside the per-connection
/// task, never inline in the accept loop, so a slowloris peer cannot stall
/// accepts.
#[allow(clippy::too_many_arguments)]
fn spawn_tcp_connection(
    stream: tokio::net::TcpStream,
    registry: Arc<crate::registry::SessionRegistry>,
    token: Option<String>,
    expose: ServeExpose,
    conns: Arc<ConnTable>,
    gate: Arc<ShutdownGate>,
    listeners: CancellationToken,
    peer: String,
) {
    tokio::spawn(async move {
        let expected_token = token.as_deref();
        match classify(&stream).await {
            Ok(TcpTransport::Http) => {
                // Health responses are momentary and stateless; they stay out
                // of the drain-tracked connection table by design.
                if let Err(e) =
                    crate::health_http::serve(stream, gate.version_of_daemon(), expected_token)
                        .await
                {
                    tracing::debug!(%peer, error=%e, "muta daemon: http error");
                }
            }
            Ok(TcpTransport::WebSocket) => {
                spawn_connection(
                    stream, registry, token, expose, conns, gate, listeners, peer,
                );
            }
            Err(e) => {
                tracing::debug!(%peer, error=%e, "muta daemon: transport classify failed");
            }
        }
    });
}

/// Spawn one connection task, registered in the connection table for the
/// drain phase. Every accepted socket funnels through here so the table can
/// never miss one; the guard unregisters on every exit path.
#[allow(clippy::too_many_arguments)]
fn spawn_connection<S>(
    stream: S,
    registry: Arc<crate::registry::SessionRegistry>,
    token: Option<String>,
    expose: ServeExpose,
    conns: Arc<ConnTable>,
    gate: Arc<ShutdownGate>,
    listeners: CancellationToken,
    peer: String,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (id, conn_cancel) = conns.register();
    let conns_for_guard = conns.clone();
    tokio::spawn(async move {
        // RAII deregistration: even a panic in handle_connection unregisters.
        let _guard = ConnGuard {
            conns: conns_for_guard,
            id,
        };
        let result = tokio::select! {
            r = handle_connection(stream, registry, token, expose, gate, listeners) => r,
            // Draining daemon (ADR-0101): cancel the connection's future.
            // The socket drops with it, closing the TCP stream; clients
            // treat the disconnect exactly like a Close frame — reconnect
            // with backoff. (Sending a graceful Close frame from *here* is
            // not possible: the WS sink is owned by the inner future.)
            _ = conn_cancel.cancelled() => {
                tracing::debug!(%peer, "muta daemon: closing connection for drain");
                Ok(())
            }
        };
        if let Err(e) = result {
            tracing::warn!(%peer, error=%e, "muta daemon: connection ended");
        }
    });
}

/// Unregisters a connection id on drop.
struct ConnGuard {
    conns: Arc<ConnTable>,
    id: u64,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.conns.unregister(self.id);
    }
}

/// Execute a session-management verb (ADR-0096) and reply once. The control
/// channel is request/response (unlike the streaming monitor/attach roles):
/// one `ControlRequest` in, one `ControlReply` out, then the connection may
/// close or issue another verb.
async fn run_control<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    mut ws_sink: futures::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, WsMessage>,
    registry: Arc<crate::registry::SessionRegistry>,
    gate: Arc<ShutdownGate>,
    listeners: CancellationToken,
    request: ControlRequest,
) -> Result<(), String> {
    let (ok, session_id, error) = match request {
        ControlRequest::Shutdown => {
            // The remote stop verb (ADR-0100): acknowledge on the wire first
            // — the gate fires the same drain as any signal, which would
            // otherwise cancel this very connection before the reply lands.
            let reply = serde_json::to_string(&Wire::ControlReply {
                ok: true,
                session_id: None,
                error: None,
            })
            .map_err(|e| format!("serialize control reply: {e}"))?;
            ws_sink
                .send(WsMessage::Text(reply.into()))
                .await
                .map_err(|e| format!("send control reply: {e}"))?;
            let _ = ws_sink.close().await;
            // The verb is the same trigger as a signal: latch the gate (so
            // monitors stream DaemonDraining and the run loop drains), and
            // stop the accept loops immediately — even when the serve layer
            // is driven standalone (no host::run), the listeners must stop.
            gate.request(crate::shutdown::ShutdownReason::ControlVerb, false);
            listeners.cancel();
            return Ok(());
        }
        ControlRequest::CreateSession { project, prompt } => {
            match registry
                .create_session(std::path::PathBuf::from(&project))
                .await
            {
                Ok(id) => {
                    if let Some(text) = prompt
                        && let Err(e) = registry.send_prompt(&id, text).await
                    {
                        tracing::warn!(session=%id,error=%e,"muta daemon: create-session prompt failed");
                    }
                    (true, Some(id), None)
                }
                Err(e) => (false, None, Some(e)),
            }
        }
        ControlRequest::SendPrompt { session_id, text } => {
            match registry.send_prompt(&session_id, text).await {
                Ok(()) => (true, Some(session_id), None),
                Err(e) => (false, None, Some(e)),
            }
        }
        ControlRequest::Interrupt { session_id } => match registry.interrupt(&session_id).await {
            Ok(()) => (true, Some(session_id), None),
            Err(e) => (false, None, Some(e)),
        },
        ControlRequest::ResolvePermission {
            session_id,
            request_id,
            decision,
        } => match registry
            .resolve_permission(&session_id, request_id, decision)
            .await
        {
            Ok(()) => (true, Some(session_id), None),
            Err(e) => (false, None, Some(e)),
        },
        ControlRequest::KillSession { session_id } => {
            match registry.kill_session(&session_id).await {
                Ok(()) => (true, Some(session_id), None),
                Err(e) => (false, None, Some(e)),
            }
        }
        ControlRequest::SuspendSession { session_id } => {
            match registry.suspend_session_control(&session_id).await {
                Ok(()) => (true, Some(session_id), None),
                Err(e) => (false, None, Some(e)),
            }
        }
    };
    let reply = serde_json::to_string(&Wire::ControlReply {
        ok,
        session_id,
        error,
    })
    .map_err(|e| format!("serialize control reply: {e}"))?;
    ws_sink
        .send(WsMessage::Text(reply.into()))
        .await
        .map_err(|e| format!("send control reply: {e}"))?;
    let _ = ws_sink.close().await;
    Ok(())
}

/// Generate the bearer token a `--public` listener requires. Two UUIDv4s —
/// 256 bits from the OS CSPRNG (uuid v4 reads `getrandom`), hex-encoded so
/// the token is URL-safe. The previous time^pid hash let a LAN attacker
/// shrink the brute-force space to almost nothing; this does not.
fn generate_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

#[allow(clippy::result_large_err)]
async fn handle_connection<S>(
    stream: S,
    registry: Arc<crate::registry::SessionRegistry>,
    token: Option<String>,
    expose: ServeExpose,
    gate: Arc<ShutdownGate>,
    listeners: CancellationToken,
) -> Result<(), String>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // One handshake path for every TCP connection (ADR-0105), token or not:
    // the callback is where browser-origin and Host validation live, and
    // those checks apply to unauthenticated loopback listeners too.
    let ws_stream = tokio_tungstenite::accept_hdr_async(
        stream,
        move |req: &Request, mut resp: tungstenite::handshake::server::Response| {
            validate_origin(req, expose)?;
            if let Some(expected) = token.as_deref() {
                match check_credentials(req, expected) {
                    Ok(Some(protocol)) => {
                        // The browser channel (ADR-0105): the token arrives as
                        // a `bearer.<token>` subprotocol offer; the handshake
                        // must echo it or the browser aborts the connection.
                        if let Ok(value) = tungstenite::http::HeaderValue::from_str(&protocol) {
                            resp.headers_mut()
                                .insert(tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL, value);
                        }
                    }
                    Ok(None) => {}
                    Err(rejection) => return Err(rejection),
                }
            }
            Ok(resp)
        },
    )
    .await
    .map_err(|e| format!("ws handshake: {e}"))?;
    let (mut ws_sink, mut ws_source) = ws_stream.split();
    let (action, project, client_posture, client_version, client_protocol) = loop {
        match ws_source.next().await {
            Some(Ok(WsMessage::Text(text))) => match serde_json::from_str::<Wire>(&text) {
                Ok(Wire::Select {
                    action,
                    project,
                    posture,
                    version,
                    protocol,
                }) => break (action, project, posture, version, protocol),
                Ok(_) => {
                    send_error(&mut ws_sink, "expected Select as the first frame").await?;
                    return Ok(());
                }
                Err(error) => {
                    send_error(&mut ws_sink, &format!("bad first frame: {error}")).await?;
                    return Ok(());
                }
            },
            Some(Ok(_)) => continue,
            Some(Err(error)) => return Err(format!("ws recv before select: {error}")),
            None => return Ok(()),
        }
    };
    // Protocol negotiation (ADR-0134), enforced before any session work.
    // The protocol number is the authority when present: the daemon serves
    // any number in [MIN_PROTOCOL_VERSION, PROTOCOL_VERSION] regardless of
    // the product build — this is what lets a pinned client keep talking to
    // a newer daemon across additive wire changes. A client that sends no
    // protocol number predates the field, so it is judged by ADR-0100
    // rule 4's exact product-version equality instead (unknown fields are
    // ignored by serde, so this daemon's newer frames never break it).
    if let Some(client) = client_protocol
        && !protocol_accepts(client)
    {
        send_error_with_code(
            &mut ws_sink,
            &protocol_mismatch_error(client, client_version.as_deref()),
            Some(ERR_PROTOCOL_MISMATCH),
        )
        .await?;
        return Ok(());
    }
    // Legacy product-version gate (ADR-0100 rule 4) for pre-protocol
    // clients only. A protocol-declaring client has already been judged on
    // the window; its product version is advisory identity, not a gate.
    if client_protocol.is_none()
        && let Some(client) = client_version
        && client != gate.version_of_daemon()
    {
        send_error_with_code(
            &mut ws_sink,
            &version_mismatch_error(&client, gate.version_of_daemon()),
            Some(ERR_VERSION_MISMATCH),
        )
        .await?;
        return Ok(());
    }
    match action {
        AttachAction::Monitor(action) => return run_monitor(ws_sink, registry, action, gate).await,
        AttachAction::Control(request) => {
            return run_control(ws_sink, registry, gate, listeners, request).await;
        }
        AttachAction::New | AttachAction::Attach(_) | AttachAction::Picker => {}
    }
    // The caller's project scopes creation / lazy resume (ADR-0096). Attach
    // clients declare their working directory in the Select frame's optional
    // `project`; a client predating that field sends none and the daemon
    // falls back to its own process cwd — which is whatever the first client
    // that spawned the daemon happened to use, so it is only correct by
    // coincidence.
    let caller_project = project.clone().unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    });
    // A modern client *declared* its project; a legacy client did not and the
    // daemon is guessing from its own cwd. Auto-binding a lone cross-project
    // session is the "launched in project A, working in project B" trap, but
    // only the declared case can know it is cross-project — so the guard
    // applies there, and the legacy path keeps its historical behaviour.
    let declared_project = project.is_some();
    let bound = match registry
        .resolve_with_declaration(action, &caller_project, declared_project)
        .await
    {
        crate::registry::ResolveOutcome::Welcome(s) => s,
        crate::registry::ResolveOutcome::Pick { sessions } => {
            let text = serde_json::to_string(&Wire::Pick { sessions })
                .map_err(|e| format!("serialize pick: {e}"))?;
            ws_sink
                .send(WsMessage::Text(text.into()))
                .await
                .map_err(|e| format!("send pick: {e}"))?;
            return Ok(());
        }
        crate::registry::ResolveOutcome::Error(message) => {
            send_error(&mut ws_sink, &message).await?;
            return Ok(());
        }
    };
    let messages = bound.session.full_transcript().await;
    let round_counter = bound.session.round_counter().await;
    let session_id = bound.session.id().await;
    // The session's own provider/model pin when set (C6), otherwise the
    // config default — the pair the picker and hint bar should boot into.
    let (provider, model) = match bound.session.provider_selection().await {
        Some(sel) => (sel.provider, sel.model.unwrap_or_default()),
        None => {
            let config = muta_persistence::config::Config::load();
            let provider = muta_agent::catalog::default_provider_id(&config).to_string();
            let model =
                muta_agent::catalog::resolved_model_name(&config, &provider).unwrap_or_default();
            (provider, model)
        }
    };
    let welcome = serde_json::to_string(&Wire::Welcome {
        session_id,
        round_counter,
        messages,
        provider,
        model,
        round_interrupts: bound.session.round_interrupts().await,
        command_catalog: bound.command_catalog.clone(),
    })
    .map_err(|e| format!("serialize welcome: {e}"))?;
    ws_sink
        .send(WsMessage::Text(welcome.into()))
        .await
        .map_err(|e| format!("send welcome: {e}"))?;
    // Attach-time state sync: the restored session's task list lives on the
    // session store, but the TUI's todo panel only fills from a live
    // `TodosUpdated` event — which a client that was not connected when the
    // session was (lazily) resumed never saw. Push one right after the
    // welcome so the sticky panel restores immediately (mirroring what
    // `restore_session_runtime` does for in-process session switches). An
    // empty list is the "no active task list" state and clears any stale
    // panel.
    let todos = bound.session.todos().await;
    let todos_frame = serde_json::to_string(&Wire::Response {
        response: AgentResponse::Round {
            session_id: bound.session.id().await,
            event: muta_contracts::RoundEvent::TodosUpdated(todos),
        },
    })
    .map_err(|e| format!("serialize todos restore: {e}"))?;
    ws_sink
        .send(WsMessage::Text(todos_frame.into()))
        .await
        .map_err(|e| format!("send todos restore: {e}"))?;
    // Attach-time `/retry` affordance (ADR-0128): a session re-hosted after
    // its round stopped (daemon restart, lazy resume) carries the durable
    // resume point in its store, but the attaching client never saw the
    // idle `HarnessState` that would have published it. Push one now so the
    // hint bar offers `/retry` from the very first frame — exactly as if the
    // client had been attached when the round stopped.
    {
        // The agent handle does not ride on `BoundSession`, so read the
        // posture straight from the session store (ADR-0132: the store is
        // the source of truth for the persisted posture). A session that
        // died unattended re-attaches with `autopilot: true` in this very
        // first snapshot — the badge paints immediately instead of waiting
        // for the next periodic `HarnessState`.
        let snapshot = muta_contracts::HarnessSnapshot {
            loop_status: muta_contracts::LoopStatus::Idle,
            round_counter: bound.session.round_counter().await,
            yolo: bound.session.yolo().await,
            workspace_security: muta_contracts::WorkspaceSecuritySnapshot::default(),
            retry_pending: bound.session.retry_pending().await.is_some(),
        };
        let frame = serde_json::to_string(&Wire::Response {
            response: AgentResponse::Round {
                session_id: bound.session.id().await,
                event: muta_contracts::RoundEvent::HarnessState(snapshot),
            },
        })
        .map_err(|e| format!("serialize retry-pending restore: {e}"))?;
        ws_sink
            .send(WsMessage::Text(frame.into()))
            .await
            .map_err(|e| format!("send retry-pending restore: {e}"))?;
    }
    let req_tx = bound.req_tx.clone();
    let mut rx = bound.events.subscribe();
    // ADR-0141: fold this client's declared posture into the session's
    // human channel. First interactive attach flips the session interactive;
    // this never *removes* interactivity (only a detach can). The trust
    // prompt below is additionally gated on the client itself being
    // interactive — pushing a question to a pipe that cannot answer would
    // park the security decision forever.
    let after_attach = bound.human_channel.attach(client_posture);
    let attached_session_id = bound.session.id().await;
    tracing::info!(
        session_id = %attached_session_id,
        effective = ?after_attach,
        "muta daemon: human channel accounted"
    );
    // Replay the buffered attach-sync events (ADR-0096) so a client that
    // attached after the session began hydrates its picker/key/context
    // state immediately, before joining the live broadcast.
    for event in drain_attach_sync(&bound.sync_buffer).await {
        let text = serde_json::to_string(&Wire::Response { response: event })
            .map_err(|e| format!("serialize attach sync: {e}"))?;
        ws_sink
            .send(WsMessage::Text(text.into()))
            .await
            .map_err(|e| format!("ws send: {e}"))?;
    }
    // Surface quarantine after attach. This is a notice, not an alternate
    // mutation path: `/trust` owns persistence and live reload.
    let snap = bound.security.snapshot(bound.project_root());
    if matches!(
            snap.aggregate(),
            muta_contracts::WorkspaceTrustState::Quarantined
                | muta_contracts::WorkspaceTrustState::Changed
        ) {
        let session_id = bound.session.id().await;
        let response = workspace_trust_notice(&session_id, bound.project_root());
        let text = serde_json::to_string(&Wire::Response { response })
            .map_err(|e| format!("serialize trust notice: {e}"))?;
        ws_sink
            .send(WsMessage::Text(text.into()))
            .await
            .map_err(|e| format!("ws send: {e}"))?;
    }
    // Liveness bookkeeping: a WS peer that dies without a RST (laptop
    // sleep, NAT drop, killed VM) leaves the select below parked on
    // `ws_source.next()` until TCP's own timeout (tens of minutes). A
    // periodic ping keeps the socket honest — the peer's pong (or any
    // inbound frame) refreshes the deadline; a full silence window with
    // nothing inbound tears the connection down so the session's broadcast
    // receiver is released (which also unblocks the idle suspension reaper,
    // ADR-0113).
    let mut last_inbound = tokio::time::Instant::now();
    let mut ping_interval = tokio::time::interval(WS_PING_INTERVAL);
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping_interval.tick().await; // skip the immediate first tick
    loop {
        tokio::select! {
            _ = ping_interval.tick() => {
                if last_inbound.elapsed() > WS_PEER_SILENCE_LIMIT {
                    tracing::warn!(
                        silent_for_secs = last_inbound.elapsed().as_secs(),
                        "muta daemon: peer silent past the limit; dropping connection"
                    );
                    return Err("peer silent past keepalive limit".to_string());
                }
                if let Err(e) = ws_sink.send(WsMessage::Ping(vec![].into())).await {
                    return Err(format!("ws ping: {e}"));
                }
            }
            resp = rx.recv() => match resp {
                Ok(resp) => {
                    let text = serde_json::to_string(&Wire::Response { response: resp })
                        .map_err(|e| format!("serialize response: {e}"))?;
                    if let Err(e) = ws_sink.send(WsMessage::Text(text.into())).await {
                        return Err(format!("ws send: {e}"));
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    // A slow attach client fell behind the session bus and
                    // the channel dropped `n` events. Skipping them silently
                    // used to leave the client's view permanently stale
                    // (missing transcript deltas read as "the agent hung").
                    // Re-anchor instead: replay the attach-sync buffer —
                    // the same idempotent startup state a fresh attach gets
                    // — so the client resynchronizes instead of drifting.
                    tracing::warn!(skipped = n, "muta daemon: client lagged; resyncing from attach-sync buffer");
                    for event in snapshot_attach_sync(&bound.sync_buffer).await {
                        let text = serde_json::to_string(&Wire::Response { response: event })
                            .map_err(|e| format!("serialize resync: {e}"))?;
                        if let Err(e) = ws_sink.send(WsMessage::Text(text.into())).await {
                            return Err(format!("ws send: {e}"));
                        }
                    }
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            msg = ws_source.next() => match msg {
                Some(Ok(WsMessage::Text(text))) => match serde_json::from_str::<Wire>(&text) {
                    Ok(Wire::Request { request: AgentRequest::EndSession }) => {
                        // Client-declared session end (ADR-0112): the
                        // operator said "I am done with this session", so
                        // tear it down now through the same path as
                        // `ControlRequest::KillSession` — cancel the driver,
                        // fire SessionEnd hooks, drop it from the registry,
                        // publish `SessionRemoved` — instead of leaving the
                        // session hosted until an idle reaper notices it.
                        // This never reaches the driver queue: the driver is
                        // about to be cancelled, so queueing would race the
                        // teardown.
                        let session_id = bound.session.id().await;
                        match registry.kill_session(&session_id).await {
                            Ok(()) => tracing::info!(
                                session = %session_id,
                                "muta daemon: client declared session end"
                            ),
                            // Already gone — which is exactly what the
                            // client asked for (e.g. another client ended it
                            // first). Nothing to report.
                            Err(e) => tracing::debug!(
                                session = %session_id,
                                error = %e,
                                "muta daemon: session already gone on client end"
                            ),
                        }
                        // `kill_session` broadcast the terminal
                        // `AgentResponse::Exit` on the session bus before
                        // returning; it is already sitting in this
                        // connection's buffer. Flush it (and anything
                        // queued behind it) so the client observes the
                        // graceful end marker instead of a bare socket
                        // close. Best-effort and bounded by what is
                        // buffered right now.
                        while let Ok(event) = rx.try_recv() {
                            let text = serde_json::to_string(&Wire::Response { response: event })
                                .map_err(|e| format!("serialize response: {e}"))?;
                            if ws_sink.send(WsMessage::Text(text.into())).await.is_err() {
                                break;
                            }
                        }
                        return Ok(());
                    }
                    Ok(Wire::Request { request }) => {
                        let _ = req_tx.send(request);
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "muta daemon: bad request json"),
                },
                Some(Ok(_)) => {
                    // Any inbound frame (pong, binary, text we ignore)
                    // proves the peer is alive: refresh the keepalive
                    // deadline.
                    last_inbound = tokio::time::Instant::now();
                }
                Some(Err(e)) => return Err(format!("ws recv: {e}")),
                None => break,
            },
        }
    }
    // ADR-0141: release this connection's channel hold. When the last
    // interactive watcher leaves, the session drops to Autonomous and any
    // request parked since resolves by labeled policy instead of hanging.
    let after_detach = bound.human_channel.detach();
    let detached_session_id = bound.session.id().await;
    tracing::info!(
        session_id = %detached_session_id,
        effective = ?after_detach,
        "muta daemon: human channel released"
    );
    Ok(())
}

/// Serve a host-observability client (ADR-0093): send one snapshot, then —
/// while `watch` holds — stream diffs until the client hangs up. The channel
/// is strictly server → client; monitor clients never steer sessions, so any
/// inbound frame other than a close is ignored. The subscription is taken
/// *before* the snapshot is composed so an event published between the two
/// cannot be lost (it arrives as a redundant diff, which consumers tolerate —
/// updates are idempotent whole-row replacements).
async fn run_monitor<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    mut ws_sink: futures::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, WsMessage>,
    registry: Arc<crate::registry::SessionRegistry>,
    action: MonitorAction,
    gate: Arc<ShutdownGate>,
) -> Result<(), String> {
    let mut rx = registry.subscribe_monitor();
    let snapshot = registry.monitor_snapshot(action).await;
    send_monitor(&mut ws_sink, MonitorEvent::Snapshot(snapshot)).await?;
    if !action.watch {
        let _ = ws_sink.close().await;
        return Ok(());
    }
    let drain = gate.triggered();
    tokio::pin!(drain);
    loop {
        tokio::select! {
            // Daemon draining (ADR-0101): say so on the stream, then close.
            // Watch clients get an explicit terminal signal instead of an
            // abrupt disconnect they might misread as a network fault.
            _ = &mut drain => {
                send_monitor(&mut ws_sink, MonitorEvent::DaemonDraining).await?;
                let _ = ws_sink.close().await;
                return Ok(());
            }
            received = rx.recv() => {
                let event = match received {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // The client fell behind; rows are whole-row replacements, so
                        // the safest resync is a fresh snapshot.
                        tracing::warn!(
                            skipped = n,
                            "muta daemon: monitor client lagged, resyncing"
                        );
                        let snapshot = registry.monitor_snapshot(action).await;
                        send_monitor(&mut ws_sink, MonitorEvent::Snapshot(snapshot)).await?;
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                };
                let filtered = match &event {
                    MonitorEvent::SessionAdded(row) | MonitorEvent::SessionUpdated(row) => {
                        if action.include_idle || row.status.is_active() {
                            Some(event.clone())
                        } else {
                            None
                        }
                    }
                    MonitorEvent::Snapshot(_)
                    | MonitorEvent::SessionRemoved { .. }
                    | MonitorEvent::DaemonDraining => Some(event.clone()),
                };
                if let Some(event) = filtered {
                    send_monitor(&mut ws_sink, event).await?;
                }
            }
        }
    }
}

/// Send one monitor stream frame. Frames ride the `Wire::Monitor` envelope
/// (ADR-0093 §4), so the client sees the same wire shape for the initial
/// snapshot and every diff.
async fn send_monitor<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    ws_sink: &mut futures::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, WsMessage>,
    event: MonitorEvent,
) -> Result<(), String> {
    let text = serde_json::to_string(&Wire::Monitor { event })
        .map_err(|e| format!("serialize monitor event: {e}"))?;
    ws_sink
        .send(WsMessage::Text(text.into()))
        .await
        .map_err(|e| format!("send monitor event: {e}"))
}

async fn send_error<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    ws_sink: &mut futures::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, WsMessage>,
    message: &str,
) -> Result<(), String> {
    send_error_with_code(ws_sink, message, None).await
}

/// `send_error` with a stable machine-readable `code` (ADR-0105) so clients
/// can branch on the reason instead of string-sniffing the message.
async fn send_error_with_code<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    ws_sink: &mut futures::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, WsMessage>,
    message: &str,
    code: Option<&str>,
) -> Result<(), String> {
    let text = serde_json::to_string(&Wire::Error {
        message: message.to_string(),
        code: code.map(str::to_string),
    })
    .map_err(|e| format!("serialize error: {e}"))?;
    ws_sink
        .send(WsMessage::Text(text.into()))
        .await
        .map_err(|e| format!("send error: {e}"))
}

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
    constant_time_eq(rest.trim().as_bytes(), expected.as_bytes())
}

/// Constant-time token comparison. Always scans the full `expected` length
/// and folds every byte into one accumulator, so timing reveals neither the
/// first mismatching position nor the expected length (a shorter `given`
/// reads as 0 bytes, a longer one is caught by the length fold — both still
/// compare every expected byte).
fn constant_time_eq(given: &[u8], expected: &[u8]) -> bool {
    let mut diff = given.len() ^ expected.len();
    for (i, &b) in expected.iter().enumerate() {
        diff |= usize::from(given.get(i).copied().unwrap_or(0) ^ b);
    }
    diff == 0
}

fn reject_unauthorized() -> ErrorResponse {
    let body = "Unauthorized".to_string();
    tungstenite::handshake::server::Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("WWW-Authenticate", "Bearer")
        .body(Some(body))
        .unwrap_or_default()
}

/// 403 for a handshake that fails the origin policy (distinct from 401: the
/// client presented no credential problem — its *provenance* is disallowed).
#[allow(clippy::result_large_err)]
fn reject_forbidden(reason: &str) -> ErrorResponse {
    tungstenite::handshake::server::Response::builder()
        .status(StatusCode::FORBIDDEN)
        .body(Some(reason.to_string()))
        .unwrap_or_default()
}

/// The host part of a `Host` header or origin URL, lowercased, port stripped.
fn host_part(authority: &str) -> String {
    let authority = authority.trim();
    let host = if let Some(rest) = authority.strip_prefix('[') {
        // Bracketed IPv6: [::1]:9800 → ::1
        rest.split(']').next().unwrap_or(rest)
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    host.to_ascii_lowercase()
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1") || host.strip_suffix(".localhost").is_some()
}

/// Browser drive-by defense (ADR-0105). WebSocket handshakes are not subject
/// to the same-origin policy: any page the user visits can open
/// `ws://127.0.0.1:<port>` and drive the daemon — *unless* the server checks.
/// Browsers always send `Origin` on a WebSocket upgrade, so on a loopback
/// listener we refuse handshakes whose origin is an http(s) page not itself
/// served from loopback (the panel served by this daemon, or a local dev
/// server, qualifies). Non-browser clients (TUI, CLI) send no `Origin` and
/// are governed by the bearer token instead. On a `--public` listener the
/// token is mandatory and the origin check is moot — remote origins are
/// legitimate there.
#[allow(clippy::result_large_err)]
fn validate_origin(req: &Request, expose: ServeExpose) -> Result<(), ErrorResponse> {
    if expose == ServeExpose::Public {
        return Ok(());
    }
    let Some(origin) = req.headers().get("Origin").and_then(|v| v.to_str().ok()) else {
        return Ok(());
    };
    let without_scheme = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"));
    let allowed = without_scheme
        .map(|rest| is_loopback_host(&host_part(rest.split('/').next().unwrap_or(rest))))
        .unwrap_or(false);
    if allowed {
        Ok(())
    } else {
        tracing::warn!(%origin, "muta daemon: refused WebSocket handshake from foreign browser origin");
        Err(reject_forbidden(
            "browser origin not allowed: the muta control plane only serves pages hosted on loopback",
        ))
    }
}

/// Subprotocol carrying a bearer token from clients that cannot set headers
/// (browsers): `Sec-WebSocket-Protocol: bearer.<token>` (ADR-0105).
const BEARER_SUBPROTOCOL_PREFIX: &str = "bearer.";

/// Credential check for the handshake: `Authorization: Bearer` first, then
/// the `bearer.<token>` subprotocol. `Ok(Some(protocol))` means the
/// subprotocol channel was used and the handshake response must echo it.
#[allow(clippy::result_large_err)]
fn check_credentials(req: &Request, expected: &str) -> Result<Option<String>, ErrorResponse> {
    if check_bearer(req, expected) {
        return Ok(None);
    }
    if let Some(offers) = req
        .headers()
        .get("Sec-WebSocket-Protocol")
        .and_then(|v| v.to_str().ok())
    {
        for offer in offers.split(',').map(str::trim) {
            if let Some(token) = offer.strip_prefix(BEARER_SUBPROTOCOL_PREFIX)
                && constant_time_eq(token.as_bytes(), expected.as_bytes())
            {
                return Ok(Some(offer.to_string()));
            }
        }
    }
    Err(reject_unauthorized())
}

/// What the first bytes of an accepted TCP connection ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TcpTransport {
    /// `Upgrade: websocket` — the control plane.
    WebSocket,
    /// Plain HTTP — the generic health endpoint.
    Http,
}

/// Cap on header bytes examined when splitting HTTP from WebSocket on the
/// single port. A handshake that does not terminate its headers within this
/// budget is handed to the WebSocket path to reject.
const PEEK_CAP: usize = 16 * 1024;

/// Split an accepted TCP connection into the WebSocket control plane or the
/// plain-HTTP health path by peeking at (not consuming) the request head.
async fn classify(stream: &tokio::net::TcpStream) -> std::io::Result<TcpTransport> {
    let mut head = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.peek(&mut chunk).await?;
        if n == 0 {
            // Peer connected and sent nothing: let the WS handshake path own
            // the (failing) parse so behavior matches a direct WS client.
            return Ok(TcpTransport::WebSocket);
        }
        head.extend_from_slice(&chunk[..n]);
        if head.windows(4).any(|w| w == b"\r\n\r\n") {
            return Ok(classify_head(&head));
        }
        if head.len() >= PEEK_CAP {
            return Ok(TcpTransport::WebSocket);
        }
    }
}

fn classify_head(head: &[u8]) -> TcpTransport {
    let text = String::from_utf8_lossy(head);
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let looks_http = request_line.ends_with("HTTP/1.1") || request_line.ends_with("HTTP/1.0");
    let ws_upgrade = lines.any(|line| {
        let line = line.trim_start();
        line.len() >= "upgrade:".len()
            && line[.."upgrade:".len()].eq_ignore_ascii_case("upgrade:")
            && line["upgrade:".len()..]
                .trim()
                .eq_ignore_ascii_case("websocket")
    });
    if looks_http && !ws_upgrade {
        TcpTransport::Http
    } else {
        TcpTransport::WebSocket
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn end_session_serializes_as_flattened_null_unit_variant() {
        // ADR-0112: `EndSession` is a unit variant flattened into the
        // `Wire::Request` envelope, so it must appear as a key with a `null`
        // value next to `"type"` — the same shape `Interrupt` uses, and what
        // the Web app's `requestFrame({ EndSession: null })` sends.
        let text = serde_json::to_string(&Wire::Request {
            request: AgentRequest::EndSession,
        })
        .expect("serialize EndSession");
        assert_eq!(text, r#"{"type":"Request","EndSession":null}"#);
    }
    #[test]
    fn generate_token_is_nonempty_hex() {
        let t = generate_token();
        // Two UUIDv4 simple encodings: 2 × 32 hex chars.
        assert_eq!(t.len(), 64);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }
    #[test]
    fn generate_token_never_repeats() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1024 {
            let t = generate_token();
            assert_eq!(t.len(), 64);
            assert!(seen.insert(t), "token collision");
        }
    }
    #[test]
    fn constant_time_eq_matches_only_exact_equality() {
        assert!(constant_time_eq(b"", b""));
        assert!(constant_time_eq(b"sekret", b"sekret"));
        // Same length, one byte off.
        assert!(!constant_time_eq(b"sekret", b"sekreT"));
        // Prefix / extension / empty must not match (no length leak via an
        // early return: the fold covers length too).
        assert!(!constant_time_eq(b"sek", b"sekret"));
        assert!(!constant_time_eq(b"sekrets", b"sekret"));
        assert!(!constant_time_eq(b"", b"sekret"));
    }
    #[test]
    fn check_bearer_accepts_only_the_exact_token() {
        fn req(auth: Option<&str>) -> Request {
            let mut builder = tungstenite::http::Request::builder();
            if let Some(value) = auth {
                builder = builder.header("Authorization", value);
            }
            builder.body(()).unwrap()
        }
        let expected = "0123456789abcdef";
        assert!(check_bearer(
            &req(Some(&format!("Bearer {expected}"))),
            expected
        ));
        // Surrounding whitespace on the header value is tolerated.
        assert!(check_bearer(
            &req(Some(&format!("Bearer  {expected} "))),
            expected
        ));
        assert!(!check_bearer(
            &req(Some("Bearer 0123456789abcde")),
            expected
        ));
        assert!(!check_bearer(
            &req(Some("Bearer 0123456789abcdef0")),
            expected
        ));
        assert!(!check_bearer(
            &req(Some("Basic 0123456789abcdef")),
            expected
        ));
        assert!(!check_bearer(&req(None), expected));
    }
    #[test]
    fn attach_action_roundtrips() {
        assert_eq!(
            serde_json::to_string(&AttachAction::New).unwrap(),
            "\"new\""
        );
        assert_eq!(
            serde_json::to_string(&AttachAction::Attach(Some("abc".into()))).unwrap(),
            r#"{"attach":"abc"}"#
        );
        let back: AttachAction = serde_json::from_str(r#"{"attach":"abc"}"#).unwrap();
        assert_eq!(back, AttachAction::Attach(Some("abc".into())));
        // The picker action (ADR-0116) serializes as a bare unit variant;
        // an older daemon that does not know it fails the Select frame with
        // a clear deserialize error instead of a mid-handshake protocol
        // fault.
        assert_eq!(
            serde_json::to_string(&AttachAction::Picker).unwrap(),
            "\"picker\""
        );
        let back: AttachAction = serde_json::from_str("\"picker\"").unwrap();
        assert_eq!(back, AttachAction::Picker);
    }

    fn handshake_req(headers: &[(&str, &str)]) -> Request {
        let mut builder = tungstenite::http::Request::builder();
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(()).unwrap()
    }

    #[test]
    fn loopback_host_detection() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("foo.localhost"));
        assert!(is_loopback_host("::1"));
        assert!(!is_loopback_host("example.com"));
        assert!(!is_loopback_host("127.0.0.1.evil.com"));
        assert_eq!(host_part("127.0.0.1:9800"), "127.0.0.1");
        assert_eq!(host_part("[::1]:9800"), "::1");
        assert_eq!(host_part("Example.COM:443"), "example.com");
    }

    #[test]
    fn origin_policy_refuses_foreign_browser_pages_on_loopback() {
        // A page served from an external site: rejected.
        let foreign = handshake_req(&[("Origin", "https://evil.example")]);
        assert!(validate_origin(&foreign, ServeExpose::Local).is_err());
        // The panel served by this daemon (or a local dev server): allowed.
        let local = handshake_req(&[("Origin", "http://127.0.0.1:9800")]);
        assert!(validate_origin(&local, ServeExpose::Local).is_ok());
        let localhost = handshake_req(&[("Origin", "http://localhost:5173")]);
        assert!(validate_origin(&localhost, ServeExpose::Local).is_ok());
        // Non-browser clients send no Origin: governed by the token instead.
        let none = handshake_req(&[]);
        assert!(validate_origin(&none, ServeExpose::Local).is_ok());
        // A sandboxed/file page ("null" origin) is foreign.
        let null = handshake_req(&[("Origin", "null")]);
        assert!(validate_origin(&null, ServeExpose::Local).is_err());
        // Public listeners skip the origin policy (token is the boundary).
        assert!(validate_origin(&foreign, ServeExpose::Public).is_ok());
    }

    #[test]
    fn credentials_accept_bearer_or_subprotocol() {
        let expected = "0123456789abcdef";
        // Authorization header wins.
        let bearer = handshake_req(&[("Authorization", &format!("Bearer {expected}"))]);
        assert_eq!(check_credentials(&bearer, expected).unwrap(), None);
        // The browser channel: bearer.<token> subprotocol offer is accepted
        // and must be echoed (Ok(Some(..))).
        let sub = handshake_req(&[("Sec-WebSocket-Protocol", &format!("bearer.{expected}"))]);
        assert_eq!(
            check_credentials(&sub, expected).unwrap(),
            Some(format!("bearer.{expected}"))
        );
        // Among several offers, the matching one is picked.
        let multi = handshake_req(&[(
            "Sec-WebSocket-Protocol",
            &format!("chat, bearer.{expected}, other"),
        )]);
        assert!(check_credentials(&multi, expected).unwrap().is_some());
        // Wrong token / no credentials: rejected.
        let wrong = handshake_req(&[("Sec-WebSocket-Protocol", "bearer.nope")]);
        assert!(check_credentials(&wrong, expected).is_err());
        let none = handshake_req(&[]);
        assert!(check_credentials(&none, expected).is_err());
    }

    #[test]
    fn classify_splits_websocket_from_plain_http() {
        let ws = b"GET / HTTP/1.1\r\nHost: 127.0.0.1:9800\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n";
        assert_eq!(classify_head(ws), TcpTransport::WebSocket);
        let ws_lower = b"GET / HTTP/1.1\r\nupgrade: WebSocket\r\n\r\n";
        assert_eq!(classify_head(ws_lower), TcpTransport::WebSocket);
        let http = b"GET /index.html HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
        assert_eq!(classify_head(http), TcpTransport::Http);
        let head = b"HEAD /healthz HTTP/1.0\r\n\r\n";
        assert_eq!(classify_head(head), TcpTransport::Http);
        // Not HTTP at all: handed to the WS path to reject properly.
        let garbage = b"nonsense\r\n\r\n";
        assert_eq!(classify_head(garbage), TcpTransport::WebSocket);
    }

    #[test]
    fn error_frame_carries_an_optional_code() {
        let with_code = serde_json::to_string(&Wire::Error {
            message: "m".to_string(),
            code: Some(ERR_VERSION_MISMATCH.to_string()),
        })
        .unwrap();
        assert_eq!(
            with_code,
            r#"{"type":"Error","message":"m","code":"version_mismatch"}"#
        );
        // Absent code stays absent on the wire (older clients unaffected).
        let without = serde_json::to_string(&Wire::Error {
            message: "m".to_string(),
            code: None,
        })
        .unwrap();
        assert_eq!(without, r#"{"type":"Error","message":"m"}"#);
        // And a frame from an older daemon (no code) still parses.
        let back: Wire = serde_json::from_str(r#"{"type":"Error","message":"m"}"#).unwrap();
        assert!(matches!(back, Wire::Error { code: None, .. }));
    }
}
