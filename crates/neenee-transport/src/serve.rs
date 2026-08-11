use futures::{SinkExt, StreamExt};
use neenee_core::{
    AgentRequest, AgentResponse, MirrorHello, MonitorAction, MonitorEvent, MonitoredSession,
    SessionOverview,
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
            neenee_core::RoundEvent::ContextTokens(_) | neenee_core::RoundEvent::HarnessState(_)
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
    /// Report a session owned by THIS client process (a standalone `neenee`
    /// TUI) into the host's observability surface (ADR-0095). The next frame
    /// must be `Wire::Mirror(MirrorHello)`; after that the client streams
    /// `Wire::MirrorUpdate(MonitoredSession)` rows until it disconnects,
    /// which removes the row.
    Mirror,
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
        decision: neenee_core::PermissionDecision,
    },
    /// Tear down a hosted session.
    KillSession { session_id: String },
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum Wire {
    Select {
        action: AttachAction,
    },
    Welcome {
        session_id: String,
        round_counter: u64,
        messages: Vec<neenee_core::Message>,
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
    /// Mirror handshake header (ADR-0095). Client → server, exactly once as
    /// the first frame after `Select{action: Mirror}`.
    Mirror {
        #[serde(flatten)]
        hello: MirrorHello,
    },
    /// Mirrored status row (ADR-0095). Client → server, streamed while the
    /// mirror connection lives; the server re-publishes it onto the monitor
    /// topic with identity fields pinned to the adopted `MirrorHello`.
    MirrorUpdate {
        #[serde(flatten)]
        row: MonitoredSession,
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

pub struct ServeHandle {
    pub port: tokio::sync::oneshot::Receiver<u16>,
    /// Resolves to the bound UDS path, or `None` when UDS is disabled or the
    /// bind failed. Unix-only.
    #[cfg(unix)]
    pub uds_ready: tokio::sync::oneshot::Receiver<Option<std::path::PathBuf>>,
    pub cancel: tokio_util::sync::CancellationToken,
    pub token: Option<String>,
}

pub fn start_server(
    opts: ServeOptions,
    registry: Arc<crate::registry::SessionRegistry>,
) -> ServeHandle {
    let (actual_port_tx, actual_port_rx) = tokio::sync::oneshot::channel::<u16>();
    let cancel = tokio_util::sync::CancellationToken::new();
    let cc = cancel.clone();
    let token = match (opts.expose, opts.token.clone()) {
        (ServeExpose::Public, None) => Some(generate_token()),
        (_, t) => t,
    };
    let bind_addr: SocketAddr = match opts.expose {
        ServeExpose::Local => ([127, 0, 0, 1], opts.port).into(),
        ServeExpose::Public => ([0, 0, 0, 0], opts.port).into(),
    };
    let tf = token.clone();
    let tcp_registry = registry.clone();
    tokio::spawn(async move {
        let listener = match TcpListener::bind(bind_addr).await {
            Ok(l) => {
                let actual = l.local_addr().map(|a| a.port()).unwrap_or(opts.port);
                let _ = actual_port_tx.send(actual);
                tracing::info!(%bind_addr,actual_port=actual,auth=tf.is_some(),"neenee serve: listener started");
                l
            }
            Err(e) => {
                tracing::error!(%bind_addr,error=%e,"neenee serve: failed to bind");
                return;
            }
        };
        loop {
            tokio::select! {_=cc.cancelled()=>{tracing::info!("neenee serve: cancelled");break;}
            ac=listener.accept()=>{let(stream,peer)=match ac{Ok(c)=>c,Err(e)=>{tracing::warn!(error=%e,"neenee serve: accept failed");continue;}};
            let registry=tcp_registry.clone();let token=tf.clone();
            tokio::spawn(async move{if let Err(e)=handle_connection(stream,registry,token).await{tracing::warn!(%peer,error=%e,"neenee serve: connection ended");}});}}
        }
    });

    #[cfg(unix)]
    let uds_rx = {
        let (uds_tx, uds_rx) = tokio::sync::oneshot::channel::<Option<std::path::PathBuf>>();
        if let Some(path) = opts.uds_path.clone() {
            let cc = cancel.clone();
            tokio::spawn(async move {
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
                loop {
                    tokio::select! {_=cc.cancelled()=>{tracing::info!("neenee serve: uds cancelled");break;}
                    ac=listener.accept()=>{let(stream,_)=match ac{Ok(c)=>c,Err(e)=>{tracing::warn!(error=%e,"neenee serve: uds accept failed");continue;}};
                    let registry=registry.clone();
                    // UDS is the local control channel: the socket's 0600
                    // permissions are the auth boundary, so no bearer token.
                    tokio::spawn(async move{if let Err(e)=handle_connection(stream,registry,None).await{tracing::warn!(error=%e,"neenee serve: uds connection ended");}});}}
                }
                let _ = std::fs::remove_file(&path);
            });
        } else {
            let _ = uds_tx.send(None);
        }
        uds_rx
    };

    ServeHandle {
        port: actual_port_rx,
        #[cfg(unix)]
        uds_ready: uds_rx,
        cancel,
        token,
    }
}

/// Execute a session-management verb (ADR-0096) and reply once. The control
/// channel is request/response (unlike the streaming monitor/attach roles):
/// one `ControlRequest` in, one `ControlReply` out, then the connection may
/// close or issue another verb.
async fn run_control<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    mut ws_sink: futures::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, WsMessage>,
    registry: Arc<crate::registry::SessionRegistry>,
    request: ControlRequest,
) -> Result<(), String> {
    let (ok, session_id, error) = match request {
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

fn generate_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    let h1 = (nanos ^ pid.wrapping_mul(0x9e3779b97f4a7c15)) as u64;
    let h2 = (nanos >> 64 ^ pid.wrapping_mul(0xbf58476d1ce4e5b9)) as u64;
    format!("{h1:016x}{h2:016x}")
}

#[allow(clippy::result_large_err)]
async fn handle_connection<S>(
    stream: S,
    registry: Arc<crate::registry::SessionRegistry>,
    token: Option<String>,
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
    let action = loop {
        match ws_source.next().await {
            Some(Ok(WsMessage::Text(text))) => match serde_json::from_str::<Wire>(&text) {
                Ok(Wire::Select { action }) => break action,
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
    match action {
        AttachAction::Monitor(action) => return run_monitor(ws_sink, registry, action).await,
        AttachAction::Mirror => return run_mirror(ws_sink, ws_source, registry).await,
        AttachAction::Control(request) => return run_control(ws_sink, registry, request).await,
        AttachAction::New | AttachAction::Attach(_) => {}
    }
    // The caller's project scopes creation / lazy resume. Attach clients
    // declare it in their Select via the discovery record they used; for now
    // the registry treats the daemon's recorded project set as global and
    // uses the process cwd as the fallback scope.
    let caller_project = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
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
            event: neenee_core::RoundEvent::TodosUpdated(todos),
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
) -> Result<(), String> {
    let mut rx = registry.subscribe_monitor();
    let snapshot = registry.monitor_snapshot(action).await;
    send_monitor(&mut ws_sink, MonitorEvent::Snapshot(snapshot)).await?;
    if !action.watch {
        let _ = ws_sink.close().await;
        return Ok(());
    }
    loop {
        match rx.recv().await {
            Ok(event) => {
                let filtered = match &event {
                    MonitorEvent::SessionAdded(row) | MonitorEvent::SessionUpdated(row) => {
                        if action.include_idle || row.status.is_active() {
                            Some(event.clone())
                        } else {
                            None
                        }
                    }
                    MonitorEvent::Snapshot(_) | MonitorEvent::SessionRemoved { .. } => {
                        Some(event.clone())
                    }
                };
                if let Some(event) = filtered {
                    send_monitor(&mut ws_sink, event).await?;
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                // The client fell behind; rows are whole-row replacements, so
                // the safest resync is a fresh snapshot.
                tracing::warn!(
                    skipped = n,
                    "neenee serve: monitor client lagged, resyncing"
                );
                let snapshot = registry.monitor_snapshot(action).await;
                send_monitor(&mut ws_sink, MonitorEvent::Snapshot(snapshot)).await?;
            }
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
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

/// Serve a mirror client (ADR-0095): a standalone `neenee` process reporting
/// a session it owns. The first frame after the handshake must be
/// `Wire::Mirror(MirrorHello)`; every `Wire::MirrorUpdate` after that is
/// re-published onto the monitor topic. The channel is client → server; the
/// server sends nothing back. When the connection ends the row is removed so
/// panels never display a silently-stale mirror.
async fn run_mirror<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    mut ws_sink: futures::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, WsMessage>,
    mut ws_source: futures::stream::SplitStream<tokio_tungstenite::WebSocketStream<S>>,
    registry: Arc<crate::registry::SessionRegistry>,
) -> Result<(), String> {
    let hello = loop {
        match ws_source.next().await {
            Some(Ok(WsMessage::Text(text))) => match serde_json::from_str::<Wire>(&text) {
                Ok(Wire::Mirror { hello }) => break hello,
                Ok(_) => {
                    send_error(&mut ws_sink, "expected Mirror header as the first frame").await?;
                    return Ok(());
                }
                Err(error) => {
                    send_error(&mut ws_sink, &format!("bad mirror header: {error}")).await?;
                    return Ok(());
                }
            },
            Some(Ok(_)) => continue,
            Some(Err(error)) => return Err(format!("ws recv before mirror header: {error}")),
            None => return Ok(()),
        }
    };
    let session_id = hello.session_id.clone();
    registry.mirror_adopt(hello).await;
    // From here on, every exit path must remove the row.
    let mut current_id = session_id;
    loop {
        match ws_source.next().await {
            Some(Ok(WsMessage::Text(text))) => match serde_json::from_str::<Wire>(&text) {
                Ok(Wire::Mirror { hello }) => {
                    // Re-adoption: the owning TUI switched which session it
                    // drives (`/session open`, `/clear`, fork). Drop the old
                    // row, adopt the new identity (ADR-0095 §3).
                    registry.mirror_remove(&current_id).await;
                    current_id = hello.session_id.clone();
                    registry.mirror_adopt(hello).await;
                }
                Ok(Wire::MirrorUpdate { row }) => {
                    // The connection is bound to the adopted session: a row
                    // naming another id is a protocol violation, not an
                    // identity takeover.
                    if row.id == current_id {
                        registry.mirror_upsert(row).await;
                    } else {
                        tracing::warn!(session = %current_id, claimed = %row.id, "neenee serve: mirror update id mismatch dropped");
                    }
                }
                Ok(_) => {
                    tracing::warn!("neenee serve: unexpected frame on mirror channel, ignored")
                }
                Err(error) => tracing::warn!(%error, "neenee serve: bad mirror frame"),
            },
            Some(Ok(_)) => {}
            Some(Err(error)) => {
                tracing::warn!(%error, "neenee serve: mirror recv failed");
                break;
            }
            None => break,
        }
    }
    registry.mirror_remove(&current_id).await;
    Ok(())
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
    rest.trim() == expected
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
        assert_eq!(t.len(), 32);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
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
