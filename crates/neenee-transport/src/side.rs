//! `/btw` side-conversation machinery (ADR-0017): the live side session, the
//! primary-status watcher that streams coarse updates to the side banner, and
//! the active-round router that directs a prompt at whichever session the user
//! is currently composing into. Extracted verbatim from `main.rs`.
//!
//! The primary round machinery is intentionally untouched here — a side session
//! peers the primary's per-round state with its own `Agent` + store + history +
//! [`RoundLifecycle`], so a side round runs concurrently with the primary round
//! without disturbing the primary's round lifecycle.

use neenee_agent::orchestration::{
    ContextProjectionSettings, InteractiveRoundContext, ProxyProvider, RoundInput, round_response,
    send_harness_state, start_interactive_round,
};
use neenee_agent::{Agent, AgentIdentity, NoProvider, RoundLifecycle};
use neenee_core::{AgentResponse, LoopStatus, ParentStatus, Provider, RoundEvent, Tool};
use neenee_persistence::config::Config;
use neenee_persistence::session::SessionStore;
use neenee_skills::SkillRegistry;
use std::sync::RwLock;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{RwLock as AsyncRwLock, mpsc};

use crate::agent_setup::active_context_window;

/// A live `/btw` side conversation (ADR-0017). Peers the primary session's
/// loose per-round state with its own [`Agent`], [`SessionStore`], and
/// [`RoundLifecycle`], so a side round runs concurrently with the primary round
/// without disturbing the primary's round lifecycle. The side store is the
/// single source of truth for the side's message list (ADR-0048); it is pinned
/// to a self-contained file written by [`SessionStore::fork_to_side`], and
/// only that file is mutated by side rounds.
pub struct SideSession {
    pub id: String,
    pub agent: Arc<Agent>,
    pub store: Arc<SessionStore>,
    pub lifecycle: Arc<RoundLifecycle>,
}

impl SideSession {
    /// Fork the primary into a self-contained side file and construct a fresh
    /// [`Agent`] + store bound to it. The primary's active pointer and
    /// in-flight round are left untouched. Returns [`None`] when the fork or
    /// side-store open fails; the caller surfaces the error.
    pub async fn build(
        primary: &SessionStore,
        base_tools: &[Arc<dyn Tool>],
        provider_holder: &Arc<RwLock<Arc<dyn Provider>>>,
        skills: SkillRegistry,
        project_root: &std::path::Path,
        identity: AgentIdentity,
    ) -> Result<Self, String> {
        let (side_id, _parent_id) = primary.fork_to_side().await?;
        let store = Arc::new(primary.open_side(&side_id).await?);

        // Fresh side agent. The provider is shared through the same
        // `ProxyProvider` holder as the primary, which clones the inner
        // `Arc<dyn Provider>` per call and is safe under concurrency
        // (ADR-0017 §2). Tools come from the cached static snapshot (no
        // `EnvoyTool` and no session-scoped dynamic connector sources), so a
        // side chat neither recurses nor implicitly acquires the principal's
        // external connections. Dynamic capability propagation must be an
        // explicit policy decision (ADR-0060).
        let side_provider: Arc<dyn Provider> =
            Arc::new(ProxyProvider::new(provider_holder.clone()));
        let agent = Arc::new(
            Agent::builder(side_provider, base_tools.to_vec(), identity)
                .with_skills(skills)
                .build(),
        );
        agent.set_thread_id(&side_id);
        agent.set_project_root(Some(project_root.to_path_buf()));
        // A side conversation is a quick aside; run it unattended — without
        // human intervention — so it never raises a permission modal whose
        // reply could not be routed back to the side `Agent` through the
        // shared permission channel. This mirrors the envoy policy
        // (`envoy_tool.rs` sets `unattended`).
        agent.set_unattended(true);

        Ok(Self {
            id: side_id,
            agent,
            store,
            lifecycle: Arc::new(RoundLifecycle::new()),
        })
    }
}

/// Coarse primary-session status, derived from the primary's [`RoundLifecycle`]
/// for the `/btw` parent-status watcher (ADR-0017 §5). No live uncancelled
/// round means the primary is idle; a running round means running.
pub async fn primary_status(primary_lifecycle: &Arc<RoundLifecycle>) -> ParentStatus {
    if primary_lifecycle.is_running().await {
        ParentStatus::Running
    } else {
        ParentStatus::Idle
    }
}

/// Watch the primary round while a `/btw` side session is live and stream
/// coarse [`ParentStatus`] updates to the TUI's side banner (ADR-0017 §5).
/// Self-terminates once the side session is torn down. Emits only on change
/// so a long-running primary round does not flood the channel.
pub fn spawn_parent_status_watcher(
    side: Arc<AsyncRwLock<Option<SideSession>>>,
    primary_lifecycle: Arc<RoundLifecycle>,
    tx: mpsc::UnboundedSender<AgentResponse>,
) {
    tokio::spawn(async move {
        let mut last: Option<ParentStatus> = None;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            if side.read().await.is_none() {
                break;
            }
            let status = primary_status(&primary_lifecycle).await;
            if last != Some(status) {
                last = Some(status);
                let _ = tx.send(AgentResponse::ParentStatus(status));
            }
        }
    });
}

/// Start an interactive round against whichever session the user is currently
/// composing into — the primary, or the live `/btw` side session when the
/// active-view flag is set (ADR-0017). A stale flag (side torn down
/// concurrently) falls back to the primary so the prompt is never silently
/// dropped. Compaction/retry knobs are resolved once from the primary agent +
/// config, which is correct because the side shares the same provider/model.
#[allow(clippy::too_many_arguments)]
pub async fn start_active_turn(
    active_view_side: &AtomicBool,
    side: &Arc<AsyncRwLock<Option<SideSession>>>,
    principal: &Arc<Agent>,
    primary_session: &Arc<SessionStore>,
    primary_lifecycle: &Arc<RoundLifecycle>,
    tx: &mpsc::UnboundedSender<AgentResponse>,
    config: &Config,
    input: RoundInput,
) {
    // Resolve which live session this round belongs to, cloning the per-session
    // Arcs out of the registry under a short-lived read lock. The guard drops
    // at the end of this statement, before the round starts.
    let (agent, session, lifecycle, session_id) = if active_view_side.load(Ordering::SeqCst) {
        let guard = side.read().await;
        if let Some(s) = guard.as_ref() {
            (
                s.agent.clone(),
                s.store.clone(),
                s.lifecycle.clone(),
                s.id.clone(),
            )
        } else {
            (
                principal.clone(),
                primary_session.clone(),
                primary_lifecycle.clone(),
                primary_session.id().await,
            )
        }
    } else {
        (
            principal.clone(),
            primary_session.clone(),
            primary_lifecycle.clone(),
            primary_session.id().await,
        )
    };

    // Refuse up-front when no real provider is configured. The TUI bumps its
    // activity-bar state optimistically (is_responding, activity_status, and
    // running_sessions) before sending `AgentRequest::Chat`; failing here
    // without emitting terminal events would leave that state stuck on
    // "queued". Emit a session-scoped `RoundEvent::Error` (resets the global
    // is_responding/activity cells) followed by an idle `HarnessState`
    // (drives the `OutboxSignal` that removes the session from
    // running_sessions) so the chrome collapses cleanly. Symmetric with
    // `start_session_turn`'s refusal path.
    if refuse_if_no_provider(tx, &agent, &session_id) {
        return;
    }

    start_resolved_turn(
        principal, tx, config, agent, session, lifecycle, session_id, input,
    )
    .await;
}

/// Resolve a live principal or side agent by its stable session id. Keeping
/// this lookup explicit prevents an outbox action from following a later view
/// switch into the wrong conversation.
pub async fn target_agent(
    side: &Arc<AsyncRwLock<Option<SideSession>>>,
    principal: &Arc<Agent>,
    primary_session: &Arc<SessionStore>,
    target_session_id: &str,
) -> Option<Arc<Agent>> {
    if primary_session.id().await == target_session_id {
        return Some(principal.clone());
    }
    side.read()
        .await
        .as_ref()
        .filter(|session| session.id == target_session_id)
        .map(|session| session.agent.clone())
}

/// Start a fresh round in one exact live session. Returns `false` when a side
/// conversation was closed after the message entered the frontend outbox.
#[allow(clippy::too_many_arguments)]
pub async fn start_session_turn(
    target_session_id: &str,
    side: &Arc<AsyncRwLock<Option<SideSession>>>,
    principal: &Arc<Agent>,
    primary_session: &Arc<SessionStore>,
    primary_lifecycle: &Arc<RoundLifecycle>,
    tx: &mpsc::UnboundedSender<AgentResponse>,
    config: &Config,
    input: RoundInput,
) -> bool {
    let primary_id = primary_session.id().await;
    let resolved = if primary_id == target_session_id {
        Some((
            principal.clone(),
            primary_session.clone(),
            primary_lifecycle.clone(),
            primary_id,
        ))
    } else {
        side.read().await.as_ref().and_then(|session| {
            (session.id == target_session_id).then(|| {
                (
                    session.agent.clone(),
                    session.store.clone(),
                    session.lifecycle.clone(),
                    session.id.clone(),
                )
            })
        })
    };
    let Some((agent, session, lifecycle, session_id)) = resolved else {
        return false;
    };

    // Same refusal contract as `start_active_turn`: a queued outbox item
    // cannot run without a real provider. Emitting the session-scoped error
    // + idle HarnessState here lets the frontend roll back its optimistically-
    // bumped state (running_sessions, the "queued" activity chip, the
    // spinner) and leave the outbox item in a recoverable state. Returning
    // `false` routes the caller through `RoundEvent::UserInputUnavailable`,
    // which promotes the dispatch item back to `Waiting` so the user can
    // recall or replay it once a real provider is configured.
    if refuse_if_no_provider(tx, &agent, &session_id) {
        return false;
    }

    start_resolved_turn(
        principal, tx, config, agent, session, lifecycle, session_id, input,
    )
    .await;
    true
}

#[allow(clippy::too_many_arguments)]
async fn start_resolved_turn(
    principal: &Arc<Agent>,
    tx: &mpsc::UnboundedSender<AgentResponse>,
    config: &Config,
    agent: Arc<Agent>,
    session: Arc<SessionStore>,
    lifecycle: Arc<RoundLifecycle>,
    session_id: String,
    input: RoundInput,
) {
    let projection =
        ContextProjectionSettings::from_config(config, active_context_window(principal));
    let retry_max_attempts = config.provider_retry_max_attempts;
    let retry_base_ms = config.provider_retry_base_ms;
    let retry_max_ms = config.provider_retry_max_ms;

    start_interactive_round(
        InteractiveRoundContext {
            agent,
            tx: tx.clone(),
            lifecycle,
            session,
            session_id,
            projection,
            retry_max_attempts,
            retry_base_ms,
            retry_max_ms,
        },
        input,
    )
    .await;
}

/// No-provider gate shared by [`start_active_turn`] and [`start_session_turn`].
///
/// When the resolved agent is parked on the `NoProvider` sentinel, emit the
/// session-scoped events the TUI needs to roll back the optimistic
/// "queued" bump it performed before dispatching the chat/outbox item:
///
/// - [`RoundEvent::Error`] surfaces the user-facing "add a provider" notice
///   and resets the global `is_responding` / `activity_status` cells in the
///   TUI listener.
/// - [`RoundEvent::HarnessState`] with `loop_status: LoopStatus::Idle` drives
///   the `OutboxSignal::HarnessState { idle: true }` path, which removes the
///   session from `running_sessions` so the composer treats the next send as
///   immediate instead of busy-queueing it.
///
/// Returns `true` when the refusal fired (caller returns early without
/// starting a round); `false` when the round should proceed normally.
fn refuse_if_no_provider(
    tx: &mpsc::UnboundedSender<AgentResponse>,
    agent: &Agent,
    session_id: &str,
) -> bool {
    if !NoProvider::is(agent.provider.as_ref()) {
        return false;
    }
    let _ = tx.send(round_response(
        session_id,
        RoundEvent::Error(
            "No provider configured. Add one with /connections before sending a message."
                .to_string(),
        ),
    ));
    send_harness_state(tx, session_id, agent, LoopStatus::Idle);
    true
}
