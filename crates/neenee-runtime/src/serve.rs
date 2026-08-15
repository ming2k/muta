use futures::{SinkExt, StreamExt};
use neenee_contracts::{AgentRequest, AgentResponse, MonitorAction, MonitorEvent, SessionOverview};
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
            neenee_contracts::RoundEvent::ContextTokens(_)
                | neenee_contracts::RoundEvent::HarnessState(_)
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttachAction {
    New,
    Attach(Option<String>),
    /// Observe the whole host instead of attaching to one session
    /// (ADR-0093): the server answers with a snapshot frame and, when
    /// `watch` is set, streams diffs until the client disconnects.
    Monitor(MonitorAction),
    /// Issue a session-management verb (ADR-0096): create, prompt, interrupt,
    /// answer a permission, or kill — without attaching as a session client.
    Control(ControlRequest),
}

/// Session-management verbs for the control plane (ADR-0096). Each maps to a
/// registry operation; the reply is `Wire::ControlReply`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(tag = "verb", rename_all = "snake_case")]
pub enum ControlRequest {
    /// Create a session for a project; optionally send an opening prompt.
    CreateSession {
        project: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
    /// Send a prompt to a hosted session as a new round.
    SendPrompt { session_id: String, text: String },
    /// Interrupt the current round of a hosted session.
    Interrupt { session_id: String },
    /// Answer a pending permission request on a hosted session.
    ResolvePermission {
        session_id: String,
        request_id: String,
        decision: neenee_contracts::PermissionDecision,
    },
    /// Tear down a hosted session.
    KillSession { session_id: String },
    /// Stop the daemon itself (ADR-0100): stop accepting new attaches, drain
    /// live connections, tear every hosted session down through the same
    /// graceful path as SIGINT/SIGTERM, and exit 0. Gives scripts, the TUI,
    /// and the upgrade flow a clean remote stop that previously required
    /// `kill <pid>`. There is deliberately no force flag: a second `neenee
    /// stop` (or any signal) escalates naturally through the same gate.
    Shutdown,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum Wire {
    Select {
        action: AttachAction,
        /// The attaching client's working directory — the project scope for
        /// `New` creation, auto-attach, and lazy resume (ADR-0096). Optional
        /// for wire compatibility: a client predating the field sends none
        /// and the daemon falls back to its own process cwd, its behavior
        /// before the field existed. Ignored for Monitor / Control
        /// actions, which the daemon serves without consulting a project
        /// scope.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<std::path::PathBuf>,
        /// The client's build version (ADR-0100 rule 4). The daemon refuses
        /// the attach with a both-versions error before any session work
        /// when it differs from its own — the wire protocol is pre-1.0 and
        /// evolves every release, so exact equality is deliberate. Absent on
        /// frames from clients predating the field; the daemon tolerates
        /// them the same way it tolerates any unknown sender: by serving
        /// them (a same-build client always sends it; only a genuinely old
        /// client omits it, and refusing on absence would brick
        /// version-pinned clients against their own daemon).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
    },
    Welcome {
        session_id: String,
        round_counter: u64,
        messages: Vec<neenee_contracts::Message>,
        /// The provider instance id the session is currently serving (its
        /// own pin when set, else the config default at bind time). Drives
        /// the TUI hint-bar's `@<instance>` suffix and the picker's active
        /// highlight. Empty when no provider is configured.
        #[serde(default)]
        provider: String,
        /// The wire model id the session is currently serving. Empty when no
        /// model resolves.
        #[serde(default)]
        model: String,
    },
    Pick {
        sessions: Vec<SessionOverview>,
    },
    Error {
        message: String,
    },
    Request {
        #[serde(flatten)]
        request: AgentRequest,
    },
    Response {
        #[serde(flatten)]
        response: AgentResponse,
    },
    /// Daemon-observability stream frame (ADR-0093). Server → client only;
    /// the first frame after a `Select{Monitor}` handshake is always
    /// `MonitorEvent::Snapshot`, followed by diffs while `watch` holds.
    Monitor {
        #[serde(flatten)]
        event: MonitorEvent,
    },
    /// Reply to a `Select{action: Control(..)}` verb (ADR-0096): either the
    /// created/confirmed session id or an error message.
    ControlReply {
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

/// Refuse a version-skewed attach with an actionable both-versions message
/// (ADR-0100 rule 4). Returned before any session work happens.
fn version_mismatch_error(client: &str, daemon: &str) -> String {
    format!(
        "client/daemon version mismatch: client {client} vs daemon {daemon}. \
         Stop the daemon and let it restart on demand: `neenee stop`, then rerun \
         this command (or `neenee serve --detach` to bring it up explicitly)."
    )
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
    /// `Some(path)` additionally serves the same protocol over a Unix domain
    /// socket (ADR-0096). UDS connections are exempt from the bearer token —
    /// the socket's filesystem permissions are the auth boundary (0600 in a
    /// 0700 runtime dir). Unix-only; ignored elsewhere.
    #[cfg(unix)]
    pub uds_path: Option<std::path::PathBuf>,
}
impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            port: 0,
            expose: ServeExpose::Local,
            token: None,
            #[cfg(unix)]
            uds_path: None,
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
    /// UDS bind result: the bound path when enabled and successful.
    /// Unix-only.
    #[cfg(unix)]
    pub uds_ready: Option<tokio::sync::oneshot::Receiver<Option<std::path::PathBuf>>>,
}

/// The destructured [`Startup`] receivers (`Startup::take`).
pub struct StartupParts {
    pub port_rx: tokio::sync::oneshot::Receiver<Result<u16, std::io::Error>>,
    #[cfg(unix)]
    pub uds_rx: tokio::sync::oneshot::Receiver<Option<std::path::PathBuf>>,
    #[cfg(not(unix))]
    pub uds_rx: std::marker::PhantomData<()>,
}

impl Startup {
    /// Take the receivers out (the run loop awaits them as locals so the
    /// containing `ServeHandle` stays intact for the drain phases).
    pub fn take(&mut self) -> StartupParts {
        StartupParts {
            port_rx: self.port.take().unwrap_or_else(|| {
                tokio::sync::oneshot::channel::<Result<u16, std::io::Error>>().1
            }),
            #[cfg(unix)]
            uds_rx: self
                .uds_ready
                .take()
                .unwrap_or_else(|| tokio::sync::oneshot::channel::<Option<std::path::PathBuf>>().1),
            #[cfg(not(unix))]
            uds_rx: std::marker::PhantomData,
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
    /// *confirm* the loops exited (and to clean up the UDS socket file
    /// deterministically, instead of racing the process end).
    pub tasks: Arc<crate::shutdown::TaskBook>,
    pub token: Option<String>,
    /// The daemon's shutdown gate: `ControlRequest::Shutdown` (the `neenee
    /// stop` verb) funnels into it like any other trigger (ADR-0100).
    pub gate: Arc<ShutdownGate>,
    /// This daemon build's version, echoed to clients during handshake
    /// version negotiation (ADR-0100 rule 4).
    pub version: &'static str,
}

/// The daemon's own `CARGO_PKG_VERSION`, shared by the discovery record and
/// the handshake refusal message. Both `neenee-server` and `neenee serve`
/// embed the same workspace version.
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
    let token = match (opts.expose, opts.token.clone()) {
        (ServeExpose::Public, None) => Some(generate_token()),
        (_, t) => t,
    };
    let bind_addr: SocketAddr = match opts.expose {
        ServeExpose::Local => ([127, 0, 0, 1], opts.port).into(),
        ServeExpose::Public => ([0, 0, 0, 0], opts.port).into(),
    };
    {
        let cc = cancel.clone();
        let tf = token.clone();
        let registry = registry.clone();
        let conns = conns.clone();
        let tasks = tasks.clone();
        let gate = gate.clone();
        let handle = tokio::spawn(async move {
            let listener = match TcpListener::bind(bind_addr).await {
                Ok(l) => {
                    let actual = l.local_addr().map(|a| a.port()).unwrap_or(opts.port);
                    let _ = actual_port_tx.send(Ok(actual));
                    tracing::info!(%bind_addr,actual_port=actual,auth=tf.is_some(),"neenee serve: listener started");
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
                tokio::select! {_=cc.cancelled()=>{tracing::info!("neenee serve: cancelled");break;}
                ac=listener.accept()=>{let(stream,peer)=match ac{Ok(c)=>c,Err(e)=>{tracing::warn!(error=%e,backoff_ms=backoff.as_millis() as u64,"neenee serve: accept failed");tokio::time::sleep(backoff).await;backoff=(backoff*2).min(ACCEPT_BACKOFF_CAP);continue;}};
                backoff=std::time::Duration::from_millis(5);
                spawn_connection(stream, registry.clone(), tf.clone(), conns.clone(), gate.clone(), cc.clone(), peer.to_string());}}
            }
        });
        tasks.track("tcp-accept", handle);
    }

    #[cfg(unix)]
    let uds_rx = {
        let (uds_tx, uds_rx) = tokio::sync::oneshot::channel::<Option<std::path::PathBuf>>();
        if let Some(path) = opts.uds_path.clone() {
            let cc = cancel.clone();
            let registry = registry.clone();
            let conns = conns.clone();
            let tasks = tasks.clone();
            let gate = gate.clone();
            let handle = tokio::spawn(async move {
                let listener = match bind_uds(&path).await {
                    Ok(l) => {
                        let _ = uds_tx.send(Some(path.clone()));
                        l
                    }
                    Err(e) => {
                        tracing::error!(path=%path.display(),error=%e,"neenee serve: uds bind failed");
                        let _ = uds_tx.send(None);
                        return;
                    }
                };
                tracing::info!(path=%path.display(),"neenee serve: uds listener started");
                let mut backoff = std::time::Duration::from_millis(5);
                loop {
                    tokio::select! {_=cc.cancelled()=>{tracing::info!("neenee serve: uds cancelled");break;}
                    ac=listener.accept()=>{let(stream,_peer)=match ac{Ok(c)=>c,Err(e)=>{tracing::warn!(error=%e,backoff_ms=backoff.as_millis() as u64,"neenee serve: uds accept failed");tokio::time::sleep(backoff).await;backoff=(backoff*2).min(ACCEPT_BACKOFF_CAP);continue;}};
                    backoff=std::time::Duration::from_millis(5);
                    // UDS is the local control channel: the socket's 0600
                    // permissions are the auth boundary, so no bearer token.
                    spawn_connection(stream, registry.clone(), None, conns.clone(), gate.clone(), cc.clone(), format!("uds:{}", path.display()));}}
                }
                // Deterministic socket-file cleanup: this runs *inside* the
                // supervised task, and `host::run` joins the task before
                // exiting, so the file is gone before the process is — no
                // more racing the runtime drop.
                let _ = std::fs::remove_file(&path);
            });
            tasks.track("uds-accept", handle);
        } else {
            let _ = uds_tx.send(None);
        }
        uds_rx
    };

    ServeHandle {
        startup: Startup {
            port: Some(actual_port_rx),
            #[cfg(unix)]
            uds_ready: Some(uds_rx),
        },
        cancel,
        conns,
        tasks,
        token,
        gate,
        version: daemon_version(),
    }
}

/// Spawn one connection task, registered in the connection table for the
/// drain phase. Every accepted socket funnels through here so the table can
/// never miss one; the guard unregisters on every exit path.
fn spawn_connection<S>(
    stream: S,
    registry: Arc<crate::registry::SessionRegistry>,
    token: Option<String>,
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
            r = handle_connection(stream, registry, token, gate, listeners) => r,
            // Draining daemon (ADR-0101): cancel the connection's future.
            // The socket drops with it, closing the TCP stream; clients
            // treat the disconnect exactly like a Close frame — reconnect
            // with backoff. (Sending a graceful Close frame from *here* is
            // not possible: the WS sink is owned by the inner future.)
            _ = conn_cancel.cancelled() => {
                tracing::debug!(%peer, "neenee serve: closing connection for drain");
                Ok(())
            }
        };
        if let Err(e) = result {
            tracing::warn!(%peer, error=%e, "neenee serve: connection ended");
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
                        tracing::warn!(session=%id,error=%e,"neenee serve: create-session prompt failed");
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

#[cfg(unix)]
/// Bind a Unix domain socket, removing any stale socket file first and
/// tightening permissions to 0600 inside a 0700 runtime dir (ADR-0096).
async fn bind_uds(path: &std::path::Path) -> std::io::Result<tokio::net::UnixListener> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        use std::os::unix::fs::FileTypeExt as _;
        if meta.file_type().is_socket() {
            std::fs::remove_file(path)?;
        }
    }
    let listener = tokio::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
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
    gate: Arc<ShutdownGate>,
    listeners: CancellationToken,
) -> Result<(), String>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
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
    let (action, project, client_version) = loop {
        match ws_source.next().await {
            Some(Ok(WsMessage::Text(text))) => match serde_json::from_str::<Wire>(&text) {
                Ok(Wire::Select {
                    action,
                    project,
                    version,
                }) => break (action, project, version),
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
    // Version negotiation (ADR-0100 rule 4): exact equality, enforced before
    // any session work. The wire protocol is pre-1.0 and changes every
    // release, so a skew is a real hazard, not a hypothetical. An absent
    // version is served (see the field docs for why pinning-safe).
    if let Some(client) = client_version
        && client != gate.version_of_daemon()
    {
        send_error(
            &mut ws_sink,
            &version_mismatch_error(&client, gate.version_of_daemon()),
        )
        .await?;
        return Ok(());
    }
    match action {
        AttachAction::Monitor(action) => return run_monitor(ws_sink, registry, action, gate).await,
        AttachAction::Control(request) => {
            return run_control(ws_sink, registry, gate, listeners, request).await;
        }
        AttachAction::New | AttachAction::Attach(_) => {}
    }
    // The caller's project scopes creation / lazy resume (ADR-0096). Attach
    // clients declare their working directory in the Select frame's optional
    // `project`; a client predating that field sends none and the daemon
    // falls back to its own process cwd — which is whatever the first client
    // that spawned the daemon happened to use, so it is only correct by
    // coincidence.
    let caller_project = project.unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    });
    let bound = match registry.resolve(action, &caller_project).await {
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
            let config = neenee_persistence::config::Config::load();
            let provider = neenee_agent::catalog::default_provider_id(&config).to_string();
            let model =
                neenee_agent::catalog::resolved_model_name(&config, &provider).unwrap_or_default();
            (provider, model)
        }
    };
    let welcome = serde_json::to_string(&Wire::Welcome {
        session_id,
        round_counter,
        messages,
        provider,
        model,
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
            event: neenee_contracts::RoundEvent::TodosUpdated(todos),
        },
    })
    .map_err(|e| format!("serialize todos restore: {e}"))?;
    ws_sink
        .send(WsMessage::Text(todos_frame.into()))
        .await
        .map_err(|e| format!("send todos restore: {e}"))?;
    let req_tx = bound.req_tx.clone();
    let mut rx = bound.events.subscribe();
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
    loop {
        tokio::select! {resp=rx.recv()=>{match resp{Ok(resp)=>{let text=serde_json::to_string(&Wire::Response{response:resp}).map_err(|e|format!("serialize response: {e}"))?;if let Err(e)=ws_sink.send(WsMessage::Text(text.into())).await{return Err(format!("ws send: {e}"));}},Err(broadcast::error::RecvError::Lagged(n))=>{tracing::warn!(skipped=n,"neenee serve: client lagged");continue;},Err(broadcast::error::RecvError::Closed)=>break,}},
        msg=ws_source.next()=>{match msg{Some(Ok(WsMessage::Text(text)))=>match serde_json::from_str::<Wire>(&text){Ok(Wire::Request{request})=>{let _=req_tx.send(request);},Ok(_)=>{},Err(e)=>tracing::warn!(error=%e,"neenee serve: bad request json"),},Some(Ok(_))=>{},Some(Err(e))=>return Err(format!("ws recv: {e}")),None=>break,}}}
    }
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
                            "neenee serve: monitor client lagged, resyncing"
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
    let text = serde_json::to_string(&Wire::Error {
        message: message.to_string(),
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

#[allow(clippy::result_large_err)]
fn reject_unauthorized() -> Result<tungstenite::handshake::server::Response, ErrorResponse> {
    let body = "Unauthorized".to_string();
    let resp = tungstenite::handshake::server::Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("WWW-Authenticate", "Bearer")
        .body(Some(body))
        .unwrap_or_default();
    Err(resp)
}

#[cfg(test)]
mod tests {
    use super::*;
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
    }
}
