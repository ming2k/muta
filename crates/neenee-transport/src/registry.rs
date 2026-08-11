use crate::UiBridge;
use crate::bootstrap::{self, BootstrapParams};
use crate::monitor::MonitorTracker;
use crate::serve::{ATTACH_SYNC_BUFFER_CAP, AttachAction, is_attach_sync_event};
use neenee_agent::{AgentIdentity, PrincipalProfile};
use neenee_core::{
    AgentRequest, AgentResponse, MirrorHello, MonitorAction, MonitorEvent, MonitorSnapshot,
    MonitoredSession, PermissionDecision, SessionHosting, SessionOverview, SessionStatus,
    WipStatus,
};
use neenee_persistence::session::SessionStore;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio_util::sync::CancellationToken;
#[derive(Clone)]
pub struct HostParams {
    pub identity: AgentIdentity,
    pub principal: PrincipalProfile,
    pub ui: Arc<dyn UiBridge>,
}
pub struct HostedSession {
    /// The project this session belongs to (ADR-0096 two-level indexing:
    /// sessions are queryable per project, hosted by one global daemon).
    pub project_root: PathBuf,
    pub session: Arc<SessionStore>,
    pub req_tx: mpsc::UnboundedSender<AgentRequest>,
    pub events: broadcast::Sender<AgentResponse>,
    pub cancel: CancellationToken,
    /// The panel-facing tracker folding this session's event stream
    /// (ADR-0093). Owned here so the broadcast-tap task can fold events in
    /// and the registry can read rows out for snapshots.
    pub tracker: Arc<Mutex<MonitorTracker>>,
    /// Attach-time state-sync buffer: the startup events an attaching client
    /// cannot reconstruct (active provider/model, picker snapshot, key
    /// readiness). Filled by the broadcast-tap; drained into each new client
    /// after it subscribes so it hydrates immediately. Bounded; see
    /// [`ATTACH_SYNC_BUFFER_CAP`].
    pub sync_buffer: Arc<Mutex<VecDeque<AgentResponse>>>,
}
#[derive(Clone)]
pub struct BoundSession {
    pub session: Arc<SessionStore>,
    pub req_tx: mpsc::UnboundedSender<AgentRequest>,
    pub events: broadcast::Sender<AgentResponse>,
    /// Attach-time state-sync events buffered for this session (see
    /// [`HostedSession::sync_buffer`]). Drained by the WS layer into a new
    /// client right after it subscribes, before it joins the live broadcast.
    pub sync_buffer: Arc<Mutex<VecDeque<AgentResponse>>>,
}
pub enum ResolveOutcome {
    Welcome(BoundSession),
    Pick { sessions: Vec<SessionOverview> },
    Error(String),
}

/// The host-level observability topic (ADR-0093): one broadcast channel
/// carrying [`MonitorEvent`] diffs for every hosted session. The registry is
/// the only publisher; any number of monitor clients subscribe. A fresh
/// subscriber is expected to first take a [`SessionRegistry::monitor_snapshot`]
/// so events published before it subscribed are not lost.
pub type MonitorBus = broadcast::Sender<MonitorEvent>;

/// Daemon provenance for monitor snapshots: who the host is and when it
/// started. A `/serve` prehost (no host record) reports `started_at: 0`.
#[derive(Clone, Default)]
pub struct MonitorMeta {
    pub project_root: Option<String>,
    pub started_at: u64,
}

#[derive(Clone)]
pub struct SessionRegistry {
    params: Option<HostParams>,
    sessions: Arc<Mutex<HashMap<String, Arc<HostedSession>>>>,
    /// Mirrored sessions owned by standalone `neenee` processes (ADR-0095):
    /// status rows arriving over `Wire::Mirror` connections, keyed by session
    /// id. Hosted sessions always win a collision — a mirrored row is a
    /// report, a hosted row is the thing itself.
    mirrors: Arc<Mutex<HashMap<String, MonitoredSession>>>,
    monitor: MonitorBus,
    meta: Arc<Mutex<MonitorMeta>>,
    /// Declared work-in-progress per session id (ADR-0097 §5): the
    /// coordination registry sessions consult via `check_wip`. In-memory —
    /// advisory by design, so a restart simply drops declarations until
    /// peers re-declare (never a correctness hazard).
    wip: Arc<Mutex<HashMap<String, WipStatus>>>,
}
impl SessionRegistry {
    pub fn new(params: HostParams) -> Self {
        Self::with_meta(Some(params))
    }
    pub fn prehost_only() -> Self {
        Self::with_meta(None)
    }
    fn with_meta(params: Option<HostParams>) -> Self {
        let (monitor, _) = broadcast::channel::<MonitorEvent>(256);
        Self {
            params,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            mirrors: Arc::new(Mutex::new(HashMap::new())),
            monitor,
            meta: Arc::new(Mutex::new(MonitorMeta::default())),
            wip: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    /// Record the host's provenance (project root + start time) so monitor
    /// snapshots can identify the host. Called once by `host::run` before
    /// the listener starts accepting monitor clients.
    pub async fn set_monitor_meta(&self, project_root: String, started_at: u64) {
        *self.meta.lock().await = MonitorMeta {
            project_root: Some(project_root),
            started_at,
        };
    }
    /// Resolve an attach. `caller_project` scopes creation and lazy resume
    /// (ADR-0096): `New` / `Attach(None)` create in the caller's project;
    /// `Attach(Some(id))` first looks up every hosted session globally, then
    /// lazy-resumes from the caller's project on disk.
    pub async fn resolve(
        &self,
        action: AttachAction,
        caller_project: &std::path::Path,
    ) -> ResolveOutcome {
        match action {
            AttachAction::New => {
                self.create_session_outcome(caller_project.to_path_buf())
                    .await
            }
            AttachAction::Attach(None) => self.resolve_auto(caller_project).await,
            AttachAction::Attach(Some(id)) => self.resolve_id(&id, caller_project).await,
            // Monitor handshakes are intercepted by the WS layer
            // (`serve::handle_connection`) and never reach `resolve`.
            AttachAction::Monitor(_) => {
                ResolveOutcome::Error("monitor connections are served directly".into())
            }
            // Mirror connections are likewise handled by the WS layer
            // (`serve::run_mirror`) and never reach `resolve`.
            AttachAction::Mirror => {
                ResolveOutcome::Error("mirror connections are served directly".into())
            }
            AttachAction::Control(_) => {
                ResolveOutcome::Error("control connections are served directly".into())
            }
        }
    }

    /// Control plane (ADR-0096): create a session for `project` and return
    /// its id. The session is daemon-held; no client is attached yet.
    pub async fn create_session(&self, project: PathBuf) -> Result<String, String> {
        let bound = self
            .assemble_hosted(crate::startup::StartupMode::Fresh, project)
            .await
            .map_err(|e| match e {
                AssembleErr::NoHost => "this host cannot create sessions".to_string(),
                AssembleErr::AssembleFailed(e) => format!("could not start a new session: {e}"),
            })?;
        Ok(bound.session.id().await)
    }

    async fn create_session_outcome(&self, project: PathBuf) -> ResolveOutcome {
        match self
            .assemble_hosted(crate::startup::StartupMode::Fresh, project)
            .await
        {
            Ok(b) => ResolveOutcome::Welcome(b),
            Err(AssembleErr::NoHost) => {
                ResolveOutcome::Error("this host cannot create sessions".into())
            }
            Err(AssembleErr::AssembleFailed(e)) => {
                ResolveOutcome::Error(format!("could not start a new session: {e}"))
            }
        }
    }
    /// Control plane (ADR-0096): send an interrupt to a hosted session.
    pub async fn interrupt(&self, session_id: &str) -> Result<(), String> {
        let map = self.sessions.lock().await;
        let Some(e) = map.get(session_id) else {
            return Err(format!(
                "session '{session_id}' is not hosted on this server"
            ));
        };
        let _ = e.req_tx.send(AgentRequest::Interrupt);
        Ok(())
    }

    /// Control plane (ADR-0096): answer a pending permission request.
    pub async fn resolve_permission(
        &self,
        session_id: &str,
        request_id: String,
        decision: PermissionDecision,
    ) -> Result<(), String> {
        let map = self.sessions.lock().await;
        let Some(e) = map.get(session_id) else {
            return Err(format!(
                "session '{session_id}' is not hosted on this server"
            ));
        };
        let _ = e.req_tx.send(AgentRequest::PermissionReply {
            request_id,
            decision,
            parent_call_id: None,
        });
        Ok(())
    }

    /// Control plane (ADR-0096): send a prompt to a hosted session as a new
    /// round (how a panel or web UI "starts a task" on an existing session).
    pub async fn send_prompt(&self, session_id: &str, text: String) -> Result<(), String> {
        let map = self.sessions.lock().await;
        let Some(e) = map.get(session_id) else {
            return Err(format!(
                "session '{session_id}' is not hosted on this server"
            ));
        };
        let _ = e.req_tx.send(AgentRequest::Chat {
            text,
            images: Vec::new(),
            sent_at_ms: None,
        });
        Ok(())
    }

    /// Control plane (ADR-0096): tear down a hosted session — cancel its
    /// driver, drop it from the registry, and tell monitors it is gone.
    pub async fn kill_session(&self, session_id: &str) -> Result<(), String> {
        let removed = self.sessions.lock().await.remove(session_id);
        let Some(e) = removed else {
            return Err(format!(
                "session '{session_id}' is not hosted on this server"
            ));
        };
        e.cancel.cancel();
        // A killed session's declared WIP goes with it (ADR-0097 §5 cleanup).
        self.clear_wip(session_id).await;
        self.publish(MonitorEvent::SessionRemoved {
            session_id: session_id.to_string(),
        })
        .await;
        Ok(())
    }

    // ── WIP coordination (ADR-0097 §5) ────────────────────────────────────

    /// Register (or replace) a session's declared WIP and project it onto the
    /// session's monitor row so peers and the dashboard see it.
    pub async fn declare_wip(&self, session_id: &str, paths: Vec<String>, summary: String) {
        let status = WipStatus { paths, summary };
        self.wip
            .lock()
            .await
            .insert(session_id.to_string(), status.clone());
        let hosted = self.sessions.lock().await.get(session_id).cloned();
        if let Some(e) = hosted {
            e.tracker.lock().await.set_wip(Some(status));
        }
    }

    /// Clear a session's declared WIP (on `wip_done`, kill, or natural
    /// settle) and remove it from the monitor row.
    pub async fn clear_wip(&self, session_id: &str) {
        self.wip.lock().await.remove(session_id);
        let hosted = self.sessions.lock().await.get(session_id).cloned();
        if let Some(e) = hosted {
            e.tracker.lock().await.set_wip(None);
        }
    }

    /// Answer a session's `check_wip`: the conflicting declared-WIPs of
    /// *other* sessions in the same workspace, plus the advice the session
    /// should act on (ADR-0097 §5). Advisory, never a lock.
    pub async fn check_wip(
        &self,
        session_id: &str,
        query_paths: &[String],
        concern: Option<&str>,
    ) -> (Vec<neenee_core::WipConflict>, neenee_core::WipAdvice) {
        let workspace = self
            .sessions
            .lock()
            .await
            .get(session_id)
            .map(|e| e.project_root.clone());
        let Some(workspace) = workspace else {
            // Unknown session: no coordination data — proceed as today.
            return (Vec::new(), neenee_core::WipAdvice::Proceed);
        };

        // Peer sessions in the same workspace (by registry index), minus the
        // asker itself. Collect ids up front (session.id() is async).
        let peers: Vec<String> = {
            let map = self.sessions.lock().await;
            let mut ids = Vec::new();
            for e in map.values() {
                if e.project_root == workspace {
                    let id = e.session.id().await;
                    if id != session_id {
                        ids.push(id);
                    }
                }
            }
            ids
        };

        let declared = self.wip.lock().await;
        let mut conflicts = Vec::new();
        for peer in peers {
            let Some(wip) = declared.get(&peer) else { continue };
            let overlap = overlap_paths(query_paths, &wip.paths);
            // A peer whose WIP doesn't intersect the query at all is not a
            // conflict for this concern (when the query named paths).
            if !query_paths.is_empty() && overlap.is_empty() && concern.is_none() {
                continue;
            }
            conflicts.push(neenee_core::WipConflict {
                session: peer,
                paths: wip.paths.clone(),
                summary: wip.summary.clone(),
                overlap,
            });
        }

        let advice = if conflicts.is_empty() {
            neenee_core::WipAdvice::Proceed
        } else if query_paths.is_empty() {
            // Whole-workspace concern (e.g. "run the full suite") with any
            // WIP present: narrow / skip global verification.
            neenee_core::WipAdvice::ProceedScoped
        } else if conflicts.iter().any(|c| !c.overlap.is_empty()) {
            neenee_core::WipAdvice::Defer
        } else {
            neenee_core::WipAdvice::ProceedScoped
        };
        (conflicts, advice)
    }

    pub async fn host(&self, entry: HostedSession) -> BoundSession {
        let id = entry.session.id().await;
        let b = BoundSession {
            session: entry.session.clone(),
            req_tx: entry.req_tx.clone(),
            events: entry.events.clone(),
            sync_buffer: entry.sync_buffer.clone(),
        };
        let tracker = entry.tracker.clone();
        self.publish(MonitorEvent::SessionAdded(tracker.lock().await.row()))
            .await;
        self.sessions.lock().await.insert(id, Arc::new(entry));
        b
    }
    async fn resolve_auto(&self, caller_project: &std::path::Path) -> ResolveOutcome {
        // Prefer the caller's own project's sessions; only when that project
        // has none do we consider the global set, and only a single global
        // session auto-binds (otherwise the client must choose).
        let map = self.sessions.lock().await;
        let mine: Vec<&Arc<HostedSession>> = map
            .values()
            .filter(|e| e.project_root == caller_project)
            .collect();
        match mine.len() {
            1 => return ResolveOutcome::Welcome(self.bound_from(mine[0])),
            n if n > 1 => {
                return ResolveOutcome::Pick {
                    sessions: self.overview_subset(&mine).await,
                };
            }
            _ => {}
        }
        match map.len() {
            0 => {
                drop(map);
                self.create_session_outcome(caller_project.to_path_buf())
                    .await
            }
            1 => {
                if let Some(e) = map.values().next() {
                    ResolveOutcome::Welcome(self.bound_from(e))
                } else {
                    unreachable!("len==1")
                }
            }
            _ => ResolveOutcome::Pick {
                sessions: self.overview(&map).await,
            },
        }
    }
    async fn resolve_id(&self, id: &str, caller_project: &std::path::Path) -> ResolveOutcome {
        {
            let map = self.sessions.lock().await;
            if let Some(e) = map.get(id) {
                return ResolveOutcome::Welcome(self.bound_from(e));
            }
        }
        if self.params.is_none() {
            return ResolveOutcome::Error(format!("session '{id}' is not hosted on this server"));
        }
        // Lazy resume searches the caller's project first, then every other
        // project the daemon knows about on disk.
        if !session_exists_on_disk(caller_project, id).await {
            return ResolveOutcome::Error(format!("unknown session id '{id}'"));
        }
        match self
            .assemble_hosted(
                crate::startup::StartupMode::Resume(Some(id.to_string())),
                caller_project.to_path_buf(),
            )
            .await
        {
            Ok(b) => ResolveOutcome::Welcome(b),
            Err(AssembleErr::NoHost) => {
                ResolveOutcome::Error(format!("session '{id}' is not hosted on this server"))
            }
            Err(AssembleErr::AssembleFailed(e)) => {
                ResolveOutcome::Error(format!("could not resume session {id}: {e}"))
            }
        }
    }
    async fn assemble_hosted(
        &self,
        startup: crate::startup::StartupMode,
        project_root: PathBuf,
    ) -> Result<BoundSession, AssembleErr> {
        let HostParams {
            identity,
            principal,
            ui,
        } = self.params.as_ref().ok_or(AssembleErr::NoHost)?.clone();
        let boot = bootstrap::assemble(BootstrapParams {
            identity,
            principal,
            ui,
            startup,
            project_root: Some(project_root.clone()),
            autopilot: false,
            single_instance: false,
        })
        .await
        .map_err(AssembleErr::AssembleFailed)?;
        let session = boot.session.clone();
        let req_tx = boot.req_tx.clone();
        let (events_tx, _) = broadcast::channel::<AgentResponse>(1024);
        let tap = events_tx.clone();
        let mut rr = boot.resp_rx;

        // Monitor wiring (ADR-0093): seed the tracker from the session header,
        // then fold every broadcast response into the row and publish diffs.
        // A lazily-resumed session whose round was cut off mid-flight will
        // re-drive from the first `TurnStarted` the driver emits, so the
        // tracker needs no transcript-tail heuristic — it starts Idle and
        // catches up from the live stream.
        let base = overview_of(&session, true).await;
        let tracker = Arc::new(Mutex::new(MonitorTracker::bootstrap(
            base_row(base, &project_root),
            SessionStatus::Idle,
        )));
        let tracker_for_tap = tracker.clone();
        let monitor_bus = self.monitor.clone();
        let sync_buffer = Arc::new(Mutex::new(VecDeque::<AgentResponse>::new()));
        let sync_buffer_for_tap = sync_buffer.clone();
        tokio::spawn(async move {
            while let Some(r) = rr.recv().await {
                {
                    let mut guard = tracker_for_tap.lock().await;
                    guard.observe(&r);
                    let row = guard.row();
                    drop(guard);
                    let _ = monitor_bus.send(MonitorEvent::SessionUpdated(row));
                }
                // Buffer the attach-sync events before broadcasting so a
                // client that attaches later still hydrates. Order within the
                // buffer matches emission order, so draining reproduces the
                // startup sync faithfully.
                if is_attach_sync_event(&r) {
                    let mut buf = sync_buffer_for_tap.lock().await;
                    if buf.len() >= ATTACH_SYNC_BUFFER_CAP {
                        buf.pop_front();
                    }
                    buf.push_back(r.clone());
                }
                let _ = tap.send(r);
            }
        });
        let cancel = CancellationToken::new();
        let cd = cancel.clone();
        let driver = boot.driver;
        tokio::spawn(async move {
            tokio::select! {_=cd.cancelled()=>tracing::info!("registry: driver cancelled"),_=driver.run()=>tracing::info!("registry: driver exited"),}
        });
        let id = session.id().await;
        let bound = BoundSession {
            session: session.clone(),
            req_tx: req_tx.clone(),
            events: events_tx.clone(),
            sync_buffer: sync_buffer.clone(),
        };
        let hosted = Arc::new(HostedSession {
            project_root,
            session,
            req_tx,
            events: events_tx,
            cancel,
            tracker,
            sync_buffer,
        });
        self.publish(MonitorEvent::SessionAdded(
            hosted.tracker.lock().await.row(),
        ))
        .await;
        self.sessions.lock().await.insert(id, hosted);
        Ok(bound)
    }
    fn bound_from(&self, e: &Arc<HostedSession>) -> BoundSession {
        BoundSession {
            session: e.session.clone(),
            req_tx: e.req_tx.clone(),
            events: e.events.clone(),
            sync_buffer: e.sync_buffer.clone(),
        }
    }

    /// Subscribe to the host-level monitor topic (ADR-0093 §4).
    pub fn subscribe_monitor(&self) -> broadcast::Receiver<MonitorEvent> {
        self.monitor.subscribe()
    }

    /// The current monitor snapshot: every hosted session's row plus every
    /// mirrored row (ADR-0095), newest activity first, honouring the client's
    /// `include_idle` filter. Hosted rows win id collisions.
    pub async fn monitor_snapshot(&self, action: MonitorAction) -> MonitorSnapshot {
        let mut sessions = Vec::new();
        let all_hosted: std::collections::HashSet<String> = {
            let map = self.sessions.lock().await;
            for hosted in map.values() {
                let row = hosted.tracker.lock().await.row();
                if action.include_idle || row.status != SessionStatus::Idle {
                    sessions.push(row);
                }
            }
            // The collision set is built from *all* hosted ids — including
            // ones the idle filter dropped — so a stale mirror row can never
            // shadow a hosted session.
            map.keys().cloned().collect()
        };
        {
            let mirrors = self.mirrors.lock().await;
            for row in mirrors.values() {
                if all_hosted.contains(&row.id) {
                    continue;
                }
                if action.include_idle || row.status != SessionStatus::Idle {
                    sessions.push(row.clone());
                }
            }
        }
        sessions.sort_by_key(|row| std::cmp::Reverse(row.updated_at));
        let meta = self.meta.lock().await.clone();
        MonitorSnapshot {
            project_root: meta.project_root.unwrap_or_default(),
            daemon_started_at: meta.started_at,
            sessions,
        }
    }

    /// Adopt a mirror connection's identity header (ADR-0095): seeds the
    /// mirrored row and announces it. A mirrored row for a session this host
    /// already serves is accepted but never surfaces (hosted wins).
    pub async fn mirror_adopt(&self, hello: MirrorHello) {
        let row = MonitoredSession {
            id: hello.session_id,
            overview: hello.overview,
            created_at: hello.created_at,
            updated_at: now_secs(),
            // A mirror hello carries no project path; the row's workspace
            // shows as unknown until a hosted twin wins the id.
            project_root: String::new(),
            // A mirror carries no declared WIP.
            wip: None,
            message_count: hello.message_count,
            hosting: SessionHosting::Mirrored,
            status: SessionStatus::Idle,
            round: 0,
            turn: None,
            output_tokens: 0,
            elapsed_ms: 0,
            current_tool: None,
            activity: None,
            context_tokens: None,
            note: None,
        };
        self.mirrors
            .lock()
            .await
            .insert(row.id.clone(), row.clone());
        self.publish(MonitorEvent::SessionAdded(row)).await;
    }

    /// Apply a mirrored status update. The row's identity fields are pinned
    /// to the adopted header so the wire copy stays truthful about *which*
    /// session it describes; the incoming `hosting` is forced to `Mirrored`.
    pub async fn mirror_upsert(&self, mut row: MonitoredSession) {
        let mut mirrors = self.mirrors.lock().await;
        let Some(existing) = mirrors.get_mut(&row.id) else {
            // An update before any hello (or after a removal) is a protocol
            // violation; drop it rather than invent an identity.
            tracing::warn!(session = %row.id, "registry: mirror update for unknown session dropped");
            return;
        };
        row.overview = existing.overview.clone();
        row.created_at = existing.created_at;
        row.hosting = SessionHosting::Mirrored;
        row.updated_at = now_secs();
        *existing = row.clone();
        drop(mirrors);
        self.publish(MonitorEvent::SessionUpdated(row)).await;
    }

    /// A mirror connection closed: the owning process may still be alive,
    /// but without its stream the row would go silently stale, so the panel
    /// drops it (ADR-0095 §—liveness).
    pub async fn mirror_remove(&self, session_id: &str) {
        if self.mirrors.lock().await.remove(session_id).is_some() {
            self.publish(MonitorEvent::SessionRemoved {
                session_id: session_id.to_string(),
            })
            .await;
        }
    }

    /// Publish a host-level diff. Best-effort: with no subscribers the
    /// broadcast send fails, which is fine — a fresh subscriber always takes
    /// a snapshot first (ADR-0093 §4).
    async fn publish(&self, event: MonitorEvent) {
        let _ = self.monitor.send(event);
    }

    /// Test-only hook: integration tests construct `HostedSession` by hand
    /// (no driver), so they emulate the registry's broadcast-tap by folding
    /// responses into the tracker themselves and publishing the diff through
    /// this narrow seam. Not part of the production surface. Best-effort:
    /// with no subscribers the send is dropped, exactly like production.
    #[doc(hidden)]
    pub fn publish_for_test(&self, event: MonitorEvent) {
        let _ = self.monitor.send(event);
    }

    async fn overview(&self, map: &HashMap<String, Arc<HostedSession>>) -> Vec<SessionOverview> {
        let hosted: Vec<&Arc<HostedSession>> = map.values().collect();
        self.overview_subset(&hosted).await
    }
    async fn overview_subset(&self, entries: &[&Arc<HostedSession>]) -> Vec<SessionOverview> {
        let mut out = Vec::new();
        for e in entries {
            out.push(overview_of(&e.session, true).await);
        }
        out.sort_by_key(|i| std::cmp::Reverse(i.updated_at));
        out
    }
}
enum AssembleErr {
    NoHost,
    AssembleFailed(Box<dyn std::error::Error>),
}

/// Project the picker's cheap header row into the monitor base row; every
/// status/accounting field starts at its zero value and is folded from the
/// live event stream by the [`MonitorTracker`]. `project_root` rides along
/// from the registry's two-level index so clients can name the workspace
/// without a second lookup.
fn base_row(overview: SessionOverview, project_root: &std::path::Path) -> MonitoredSession {
    MonitoredSession {
        id: overview.id,
        overview: overview.overview,
        created_at: overview.created_at,
        updated_at: overview.updated_at,
        message_count: overview.message_count,
        hosting: SessionHosting::Hosted,
        status: SessionStatus::Idle,
        round: 0,
        turn: None,
        output_tokens: 0,
        elapsed_ms: 0,
        current_tool: None,
        activity: None,
        context_tokens: None,
        note: None,
        project_root: project_root.display().to_string(),
        wip: None,
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[allow(clippy::collapsible_if)]
async fn overview_of(session: &SessionStore, active: bool) -> SessionOverview {
    let id = session.id().await;
    if let Ok(items) = session.list().await {
        if let Some(item) = items.into_iter().find(|i| i.id == id) {
            return SessionOverview {
                id: item.id,
                overview: item.overview,
                created_at: item.created_at,
                updated_at: item.updated_at,
                message_count: item.message_count,
                active,
            };
        }
    }
    let mc = session.full_transcript().await.len();
    SessionOverview {
        id,
        overview: String::new(),
        created_at: 0,
        updated_at: 0,
        message_count: mc,
        active,
    }
}
async fn session_exists_on_disk(project_root: &std::path::Path, id: &str) -> bool {
    SessionStore::load_for_project(project_root.to_path_buf())
        .list()
        .await
        .map(|items| items.iter().any(|i| i.id == id))
        .unwrap_or(false)
}

/// The subset of `wip` paths overlapping the query's `query` paths. Paths are
/// compared normalized (separators unified, trailing slashes stripped, `.`
/// resolved); a declared path overlaps a queried path when either is a prefix
/// of the other (a directory WIP covers files beneath it, and vice versa).
/// Empty when the query named no paths.
fn overlap_paths(query: &[String], wip: &[String]) -> Vec<String> {
    if query.is_empty() {
        return Vec::new();
    }
    let norm = |p: &str| -> String {
        let p = p.replace('\\', "/");
        let mut p = p.trim_end_matches('/').to_string();
        // Resolve leading "./" so workspace-relative forms compare equal.
        while let Some(rest) = p.strip_prefix("./") {
            p = rest.to_string();
        }
        p
    };
    let query_norm: Vec<String> = query.iter().map(|p| norm(p)).collect();
    wip.iter()
        .filter(|w| {
            let w = norm(w);
            query_norm.iter().any(|q| {
                w == *q
                    || w.starts_with(&format!("{q}/"))
                    || q.starts_with(&format!("{w}/"))
            })
        })
        .cloned()
        .collect()
}
