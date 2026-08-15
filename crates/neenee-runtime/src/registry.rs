use crate::UiBridge;
use crate::bootstrap::{self, BootstrapParams};
use crate::monitor::MonitorTracker;
use crate::serve::{ATTACH_SYNC_BUFFER_CAP, AttachAction, is_attach_sync_event};
use neenee_agent::{Agent, AgentIdentity, PrincipalProfile};
use neenee_contracts::{
    AgentRequest, AgentResponse, MonitorAction, MonitorEvent, MonitorSnapshot, MonitoredSession,
    PermissionDecision, SessionHosting, SessionOverview, SessionStatus, WipStatus,
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
    /// `ATTACH_SYNC_BUFFER_CAP`.
    pub sync_buffer: Arc<Mutex<VecDeque<AgentResponse>>>,
    /// When this hosted session was created (wall-clock, monotonic). Drives
    /// the idle reaper: a session that stays `is_empty_unpersisted` (no real
    /// content, never written to disk) past the idle TTL is reclaimed so
    /// abandoned empty sessions cannot accumulate in memory. `Instant` is
    /// process-local and never persisted, matching the in-memory-only nature
    /// of the sessions it guards.
    pub created_at: std::time::Instant,
    /// Handle on the session's primary agent (the same `Arc` the bootstrap
    /// hands out as `agent_for_session_end`) so the registry can fire
    /// SessionEnd hooks (ADR-0025) when the session ends — killed over the
    /// control plane, reaped, or torn down on daemon shutdown. The driver
    /// task owns the agent otherwise. `None` only for hand-built test
    /// entries, which carry no agent and fire nothing.
    pub agent_for_session_end: Option<Arc<Agent>>,
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
    monitor: MonitorBus,
    meta: Arc<Mutex<MonitorMeta>>,
    /// Declared work-in-progress per session id (ADR-0097 §5): the
    /// coordination registry sessions consult via `check_wip`. In-memory —
    /// advisory by design, so a restart simply drops declarations until
    /// peers re-declare (never a correctness hazard).
    wip: Arc<Mutex<HashMap<String, WipStatus>>>,
}

/// Shared handle on the WIP-coordination registry (ADR-0097 §5), injected
/// into the per-session `declare_wip`/`wip_done` tools so they mutate the
/// daemon's coordination state without holding the whole registry.
pub type WipRegistry = Arc<Mutex<HashMap<String, WipStatus>>>;

/// How long a never-persisted (empty) hosted session may sit idle before the
/// reaper reclaims it. Five minutes is comfortably longer than any legitimate
/// create→attach→first-prompt gap, so an empty session a user is about to
/// type into is never swept from under them.
const IDLE_EMPTY_SESSION_TTL: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// How often the idle-empty reaper sweeps. One minute keeps abandoned empty
/// sessions bounded without meaningfully waking the daemon.
const IDLE_REAPER_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Per-session budget for `SessionEnd` hooks during teardown (ADR-0101).
/// A user hook runs an external process; a hung one must not pin the daemon
/// (this bound applies to single-session kills; daemon shutdown sizes the
/// same budget against its remaining grace).
const DEFAULT_SESSION_END_HOOK_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);
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
    /// driver, fire its SessionEnd hooks (ADR-0025: a killed session is a
    /// session that ended), drop it from the registry, and tell monitors it
    /// is gone.
    ///
    /// `hook_budget` bounds each `fire_session_end` call. A user-configured
    /// SessionEnd hook runs an external process; a hung one must never pin
    /// the daemon's shutdown (or this verb) open. On timeout the remaining
    /// hook work is abandoned — the hook's side effects are best-effort by
    /// design (ADR-0025) — and the teardown continues.
    pub async fn kill_session(&self, session_id: &str) -> Result<(), String> {
        self.kill_session_with_hook_budget(session_id, DEFAULT_SESSION_END_HOOK_BUDGET)
            .await
    }

    /// [`Self::kill_session`] with an explicit hook budget, so daemon
    /// shutdown can size it against its own remaining grace (ADR-0101).
    pub async fn kill_session_with_hook_budget(
        &self,
        session_id: &str,
        hook_budget: std::time::Duration,
    ) -> Result<(), String> {
        let removed = self.sessions.lock().await.remove(session_id);
        let Some(e) = removed else {
            return Err(format!(
                "session '{session_id}' is not hosted on this server"
            ));
        };
        e.cancel.cancel();
        // SessionEnd observers fire best-effort after the driver is cancelled;
        // the hook context (session id + cwd) does not depend on it. Bounded:
        // an external-process hook that hangs cannot pin the daemon open.
        if let Some(agent) = &e.agent_for_session_end
            && tokio::time::timeout(hook_budget, agent.fire_session_end())
                .await
                .is_err()
        {
            tracing::warn!(
                session = %session_id,
                timeout_secs = hook_budget.as_secs_f32(),
                "registry: SessionEnd hook exceeded its budget; abandoning it"
            );
        }
        // A killed session's declared WIP goes with it (ADR-0097 §5 cleanup).
        self.clear_wip(session_id).await;
        self.publish(MonitorEvent::SessionRemoved {
            session_id: session_id.to_string(),
        })
        .await;
        Ok(())
    }

    /// Graceful daemon shutdown (ADR-0096): tear down every hosted session
    /// via [`Self::kill_session_with_hook_budget`], so each one's SessionEnd
    /// hooks (ADR-0025) fire before the process exits. `host::run` calls
    /// this after the listeners stop accepting and the connections drain
    /// (ADR-0101).
    ///
    /// Sessions are torn down **concurrently** and each hook is bounded by
    /// `hook_budget` individually: N sessions cost max(hook) — not the sum —
    /// so one slow hook cannot starve the others of their own budget.
    /// Best-effort per session: one failure does not skip the rest.
    pub async fn shutdown_all_sessions(&self) {
        self.shutdown_all_sessions_with_hook_budget(DEFAULT_SESSION_END_HOOK_BUDGET)
            .await;
    }

    pub async fn shutdown_all_sessions_with_hook_budget(&self, hook_budget: std::time::Duration) {
        let ids: Vec<String> = self.sessions.lock().await.keys().cloned().collect();
        let mut tears: Vec<std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>> =
            Vec::with_capacity(ids.len());
        for id in ids {
            let this = self.clone();
            tears.push(Box::pin(async move {
                if let Err(error) = this.kill_session_with_hook_budget(&id, hook_budget).await {
                    tracing::warn!(session = %id, %error, "registry: session teardown failed on shutdown");
                }
            }));
        }
        futures::future::join_all(tears).await;
    }

    /// One pass of the idle-empty reaper: remove every hosted session that is
    /// still `is_empty_unpersisted` (no user-facing content, never written to
    /// disk) **and** has been hosted longer than `IDLE_EMPTY_SESSION_TTL`
    /// **and** currently has no attached client. Returns the reclaimed ids.
    ///
    /// Only never-persisted sessions are eligible — a session that has real
    /// content (however brief) is user history and is never reaped. The
    /// `receiver_count() == 0` probe treats any live event subscriber as an
    /// attached client, so a session someone is actively watching is left
    /// alone even while still empty. This is the in-memory counterpart of the
    /// persistence layer's lazy-materialisation guard (ADR-0018): that guard
    /// keeps empty sessions off disk, this one keeps them from piling up in
    /// the daemon's memory when clients create-then-abandon them.
    pub async fn reap_idle_empty_sessions(&self) -> Vec<String> {
        self.reap_idle_empty_sessions_with(IDLE_EMPTY_SESSION_TTL)
            .await
    }

    /// TTL-parameterised core of [`Self::reap_idle_empty_sessions`], split out
    /// so tests can sweep immediately instead of waiting out the real TTL.
    #[doc(hidden)]
    pub async fn reap_idle_empty_sessions_with(&self, ttl: std::time::Duration) -> Vec<String> {
        // Snapshot candidates under the lock, then probe + kill outside it so
        // we never hold the map lock across an `await` on a session's state.
        let candidates: Vec<(String, Arc<HostedSession>)> = self
            .sessions
            .lock()
            .await
            .iter()
            .map(|(id, e)| (id.clone(), Arc::clone(e)))
            .collect();
        let mut reaped = Vec::new();
        for (id, entry) in candidates {
            let idle_long_enough = entry.created_at.elapsed() >= ttl;
            let no_clients = entry.events.receiver_count() == 0;
            if !idle_long_enough || !no_clients {
                continue;
            }
            if !entry.session.is_empty_unpersisted().await {
                continue;
            }
            // Guard against the id-key stability hazard: an in-session
            // `/session open` or `/new` may have repointed the store to a
            // *different*, now persisted session while the map key still holds
            // the original id.
            // Only kill when the store is still on the entry's original id —
            // otherwise the entry has been repurposed and must be left alone.
            if entry.session.id().await != id {
                continue;
            }
            if self.kill_session(&id).await.is_ok() {
                tracing::info!(session_id = %id, "reaped idle never-persisted session");
                reaped.push(id);
            }
        }
        reaped
    }

    /// Spawn the background idle-empty reaper, sweeping every
    /// `IDLE_REAPER_INTERVAL` until `cancel` fires. The daemon calls this
    /// once at startup; the task stops cleanly on shutdown.
    pub fn spawn_idle_reaper(self: &Arc<Self>, cancel: CancellationToken) {
        let registry = Arc::clone(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(IDLE_REAPER_INTERVAL);
            // `interval` fires immediately on creation; skip the first tick so
            // a just-started daemon does not reap sessions still being set up.
            tick.tick().await;
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        tracing::info!("idle-session reaper: shutdown");
                        break;
                    }
                    _ = tick.tick() => {
                        registry.reap_idle_empty_sessions().await;
                    }
                }
            }
        });
    }

    // ── WIP coordination (ADR-0097 §5) ────────────────────────────────────

    /// The shared WIP-coordination handle, for injecting into the session
    /// tools (ADR-0097 §5).
    pub fn wip_registry_handle(&self) -> WipRegistry {
        Arc::clone(&self.wip)
    }

    /// A `check_wip` closure bound to one session, for injecting into the
    /// `check_wip` tool. It clones the registry so the tool can query
    /// against the live sessions index without holding a registry borrow.
    fn check_wip_closure(&self, session_id: String) -> crate::wip_tools::CheckWipQuery {
        let registry = self.clone();
        Arc::new(move |paths: Vec<String>, concern: Option<String>| {
            let registry = registry.clone();
            let sid = session_id.clone();
            Box::pin(async move { registry.check_wip(&sid, &paths, concern.as_deref()).await })
        })
    }

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
    ) -> (
        Vec<neenee_contracts::WipConflict>,
        neenee_contracts::WipAdvice,
    ) {
        let workspace = self
            .sessions
            .lock()
            .await
            .get(session_id)
            .map(|e| e.project_root.clone());
        let Some(workspace) = workspace else {
            // Unknown session: no coordination data — proceed as today.
            return (Vec::new(), neenee_contracts::WipAdvice::Proceed);
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
            let Some(wip) = declared.get(&peer) else {
                continue;
            };
            let overlap = overlap_paths(query_paths, &wip.paths);
            // A peer whose WIP doesn't intersect the query at all is not a
            // conflict for this concern (when the query named paths).
            if !query_paths.is_empty() && overlap.is_empty() && concern.is_none() {
                continue;
            }
            conflicts.push(neenee_contracts::WipConflict {
                session: peer,
                paths: wip.paths.clone(),
                summary: wip.summary.clone(),
                overlap,
            });
        }

        let advice = if conflicts.is_empty() {
            neenee_contracts::WipAdvice::Proceed
        } else if query_paths.is_empty() {
            // Whole-workspace concern (e.g. "run the full suite") with any
            // WIP present: narrow / skip global verification.
            neenee_contracts::WipAdvice::ProceedScoped
        } else if conflicts.iter().any(|c| !c.overlap.is_empty()) {
            neenee_contracts::WipAdvice::Defer
        } else {
            neenee_contracts::WipAdvice::ProceedScoped
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
            extra_session_tools: None,
        })
        .await
        .map_err(AssembleErr::AssembleFailed)?;
        let session = boot.session.clone();
        let req_tx = boot.req_tx.clone();
        // WIP-coordination tools (ADR-0097 §5): build them now that the
        // session id is known, and publish them onto the agent the assemble
        // built so its model can declare/consult WIP against this daemon's
        // coordination registry. Done before the driver starts serving
        // requests so the tools are present from the first turn.
        let session_id = session.id().await;
        let wip_tools = crate::wip_tools::build_wip_tools(
            self.wip_registry_handle(),
            session_id.clone(),
            self.check_wip_closure(session_id),
        );
        boot.agent
            .dynamic_tool_sink()
            .replace("wip-coordination", wip_tools);
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
            created_at: std::time::Instant::now(),
            agent_for_session_end: Some(boot.agent_for_session_end),
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

    /// Number of currently hosted sessions (the idle-exit probe,
    /// ADR-0100 rule 3).
    pub async fn session_count(&self) -> usize {
        self.sessions.lock().await.len()
    }

    /// Subscribe to the host-level monitor topic (ADR-0093 §4).
    pub fn subscribe_monitor(&self) -> broadcast::Receiver<MonitorEvent> {
        self.monitor.subscribe()
    }

    /// The current monitor snapshot: every hosted session's row, newest
    /// activity first, honouring the client's `include_idle` filter.
    pub async fn monitor_snapshot(&self, action: MonitorAction) -> MonitorSnapshot {
        let mut sessions = Vec::new();
        {
            let map = self.sessions.lock().await;
            for hosted in map.values() {
                let row = hosted.tracker.lock().await.row();
                if action.include_idle || row.status != SessionStatus::Idle {
                    sessions.push(row);
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

    /// Publish a host-level diff. Best-effort: with no subscribers the
    /// broadcast send fails, which is fine — a fresh subscriber always takes
    /// a snapshot first (ADR-0093 §4).
    async fn publish(&self, event: MonitorEvent) {
        let _ = self.monitor.send(event);
    }

    /// Publish a host-level (daemon-scope) event on the monitor bus
    /// (ADR-0101): the run loop announces `DaemonDraining` when the graceful
    /// drain begins so every watch client learns the daemon is going away
    /// before its connection closes. Public and synchronous because the bus
    /// is a broadcast sender — sending never blocks on subscribers.
    pub fn publish_host_event(&self, event: MonitorEvent) {
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

/// Insert/replace a session's declared WIP into the shared coordination
/// registry (the half of `declare_wip` the session tools drive directly).
pub(crate) async fn declare_wip_on(registry: &WipRegistry, session_id: &str, status: WipStatus) {
    registry.lock().await.insert(session_id.to_string(), status);
}

/// Remove a session's declared WIP from the shared coordination registry.
pub(crate) async fn clear_wip_on(registry: &WipRegistry, session_id: &str) {
    registry.lock().await.remove(session_id);
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
                w == *q || w.starts_with(&format!("{q}/")) || q.starts_with(&format!("{w}/"))
            })
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod wip_tests {
    use super::overlap_paths;

    fn v(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn overlap_exact_and_prefix_both_directions() {
        // Exact match.
        assert_eq!(
            overlap_paths(&v(&["src/a.rs"]), &v(&["src/a.rs"])),
            v(&["src/a.rs"])
        );
        // WIP dir covers queried file beneath it.
        assert_eq!(overlap_paths(&v(&["src/a.rs"]), &v(&["src"])), v(&["src"]));
        // Queried dir covers WIP file beneath it.
        assert_eq!(
            overlap_paths(&v(&["src"]), &v(&["src/a.rs"])),
            v(&["src/a.rs"])
        );
        // Disjoint paths don't overlap.
        assert!(overlap_paths(&v(&["src/a.rs"]), &v(&["docs/b.md"])).is_empty());
        // Sibling names sharing a prefix string but not a path segment.
        assert!(overlap_paths(&v(&["src/app"]), &v(&["src/apple"])).is_empty());
    }

    #[test]
    fn overlap_normalizes_separators_and_dots() {
        assert_eq!(
            overlap_paths(&v(&["./src/a.rs"]), &v(&["src/a.rs"])),
            v(&["src/a.rs"])
        );
        assert_eq!(
            overlap_paths(&v(&["src\\a.rs"]), &v(&["src/a.rs"])),
            v(&["src/a.rs"])
        );
        assert_eq!(overlap_paths(&v(&["src/"]), &v(&["src"])), v(&["src"]));
    }

    #[test]
    fn overlap_empty_query_means_no_specific_paths() {
        assert!(overlap_paths(&[], &v(&["src"])).is_empty());
    }
}
