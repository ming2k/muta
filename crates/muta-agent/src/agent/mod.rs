//! The [`Agent`] orchestration type and its supporting machinery.
//!
//! The type definition, builder, runner handle, queue plumbing, the free
//! helper functions shared across the split, and the embedded tests. The
//! `impl Agent` blocks are split by concern into sibling modules:
//! `state` (configuration/identity), `steering` (rounds/queues/interrupts),
//! `tools_admin` (permissions/catalog), `rounds` (streaming loop), and
//! `execution` (tool tail + hooks).

use super::*;
use muta_contracts::human_request::{
    AutonomousFallbackPolicy, HumanChannelPosture, HumanReply, HumanRequestKind,
};

use futures::future::BoxFuture;

/// Role-reanchoring note appended to a successful runner's tool-result text in
/// the master's transcript. Counters "role bleed": after a run of read-only
/// delegations the model may over-generalize the runner's read-only framing onto
/// the master itself. The note pins the boundary explicitly and
/// unconditionally — it does not rely on a `[hooks]` entry, so the guarantee is
/// structural.
const RUNNER_REANCHOR_OK: &str = "\
[system] The read-only / toolset-scoped framing above applies to the runner only. \
You (the master agent) retain your full toolset — including write and edit tools \
and the shell — across this delegation. Perform any edits or writes yourself; the \
runner cannot.";

/// Same role-reanchoring for a *failed* runner. Reaffirms the boundary and nudges
/// the master toward acting directly rather than re-delegating a failing
/// sub-task.
const RUNNER_REANCHOR_FAILED: &str = "\
[system] That runner could not complete its sub-task. Its read-only / toolset-scoped \
framing does not transfer to you: you (the master agent) retain your full toolset \
— including write and edit tools and the shell. Act directly on the findings above, \
or re-delegate with a narrower scope.";

/// Same role-reanchoring for an *interrupted* runner: stopped by the user, not
/// failed. The partial findings above are real work; the master may continue
/// them directly or re-delegate, and stays accountable for the outcome.
const RUNNER_REANCHOR_INTERRUPTED: &str = "\
[system] That runner was interrupted mid-task (stopped by the user before it finished). \
Its partial findings above are real work, and its read-only / toolset-scoped framing \
does not transfer to you: you (the master agent) retain your full toolset — \
including write and edit tools and the shell. Continue the work directly from where \
it stopped, or re-delegate with a narrower scope.";

/// Build the model-visible text for an runner tool result: the runner's summary
/// wrapped in the standard `[<tool> result]:` header, followed by a
/// deterministic role-reanchoring note (`RUNNER_REANCHOR_OK` on success,
/// `RUNNER_REANCHOR_FAILED` on `failed`, `RUNNER_REANCHOR_INTERRUPTED` on
/// `interrupted`). This is the single choke point where
/// an runner's read-only framing enters the master's transcript, so the
/// re-anchor is applied here unconditionally — it cannot be forgotten by a
/// missing `[hooks]` config. Extracted from [`Agent::record_tool_result`] so the
/// contract (the anchor is present, and its tone tracks the failure flag) is
/// unit-testable without a full `Agent` fixture.
pub(crate) fn runner_result_text(
    name: &str,
    summary: &str,
    failed: bool,
    interrupted: bool,
) -> String {
    let reanchor = if interrupted {
        RUNNER_REANCHOR_INTERRUPTED
    } else if failed {
        RUNNER_REANCHOR_FAILED
    } else {
        RUNNER_REANCHOR_OK
    };
    format!("[{name} result]:\n{summary}\n\n{reanchor}")
}

/// In-memory only mask of tools a hook has temporarily disabled via a
/// [`muta_contracts::HookOutcome::ScopeTools`] outcome, partitioned by the
/// [`muta_contracts::RestorePoint`] at which each should come back.
///
/// Deliberately **separate** from the session-level, persisted
/// [`Agent::disabled_tools`]: scoped disables never reach the session store
/// (the snapshot path only clones the persisted mask), so they never survive a
/// restart and never collide with a user's manual `/tools` toggles. Each bucket
/// is a reference count (`HashMap<String, u32>`) rather than a flat set so two
/// hooks disabling the same tool at different restore points don't fight: the
/// earlier restore only decrements, the tool stays hidden until its last
/// refcount reaches zero.
#[derive(Default, Clone)]
pub(crate) struct ScopedToolDisable {
    round_end: HashMap<String, u32>,
    turn_end: HashMap<String, u32>,
}

impl ScopedToolDisable {
    /// Record a hook-fired disable for `tool` at `restore`. Increments the
    /// refcount so nested disables compose.
    fn disable(&mut self, tool: &str, restore: muta_contracts::RestorePoint) {
        let bucket = match restore {
            muta_contracts::RestorePoint::TurnEnd => &mut self.turn_end,
            muta_contracts::RestorePoint::RoundEnd => &mut self.round_end,
        };
        *bucket.entry(tool.to_string()).or_insert(0) += 1;
    }

    /// Whether `tool` is currently scoped-disabled (hidden from the model and
    /// rejected at dispatch) under any restore point.
    pub(crate) fn contains(&self, tool: &str) -> bool {
        self.round_end.contains_key(tool) || self.turn_end.contains_key(tool)
    }

    /// Drop every `TurnEnd` disable at the ReAct-turn boundary. `RoundEnd`
    /// disables survive until the user round ends.
    fn restore_turn_end(&mut self) {
        self.turn_end.clear();
    }

    /// Drop every disable (both buckets). Called at user-round end so the
    /// toolset is fresh for the next user request.
    fn restore_round_end(&mut self) {
        self.round_end.clear();
        self.turn_end.clear();
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.round_end.is_empty() && self.turn_end.is_empty()
    }
}

/// Mid-turn save-point closure installed by orchestration (ADR-0048).
///
/// Invoked at each ReAct-turn boundary with the current full round history.
/// The implementation diffs against its own durable baseline and appends only
/// the new tail to the session event log (see `SessionStore::append_turn`).
/// Errors are surfaced back to the ReAct loop, which treats a persist failure
/// as a round-ending error (better to stop than to keep mutating state that may
/// not be recoverable).
pub(crate) type TurnPersistFn =
    Arc<dyn Fn(&[Message]) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

pub use muta_contracts::RequestTokenEstimate;

// `AgentIdentity` now lives in `muta-contracts` (`identity.rs`) as pure domain
// vocabulary, alongside the role profiles. It is re-exported by name at the
// crate root below and via `pub use muta_contracts::*`, so all existing
// `muta_agent::AgentIdentity` / `crate::AgentIdentity` references keep
// resolving unchanged.

/// Parked oneshots for in-flight interactive-input requests (L3.5 β): a
/// `bash` command classified interactive blocks here until the operator's
/// [`InputReply`] arrives (or `None` on cancel/turn-end).
pub struct Agent {
    pub provider: Arc<dyn Provider>,
    /// Archetype / kind of this agent (Master, Runner).
    kind: std::sync::RwLock<muta_contracts::AgentKind>,
    /// Global/Session tool pool for declarative tool resolution.
    pool: Arc<std::sync::RwLock<muta_contracts::ToolPool>>,
    /// The full capability set: every tool keyed by capability, with all its
    /// variants. The single source of truth from which the model-visible
    /// [`resolved_tools`](Self::resolved_tools) view is derived for the active
    /// [`variant_selection`](Self::variant_selection).
    pub(crate) toolset: muta_contracts::ToolSet,

    /// The active resolved view: exactly one variant per capability, for the
    /// current model's [`variant_selection`](Self::variant_selection). Both request
    /// assembly (`visible_tools` → `ModelRequest`) and dispatch (`find` by name)
    /// read this, so re-resolving it on a model/selection switch makes *both* the
    /// schema and the executed implementation track the chosen variant. Held
    /// behind a `RwLock` because it is swapped wholesale on selection change.
    resolved_tools: Arc<std::sync::RwLock<Vec<Arc<dyn Tool>>>>,
    /// Tools published by dynamically changing external sources. MCP and
    /// future connectors replace their own named snapshots through the core
    /// [`DynamicToolSink`] port; the agent owns synchronization, provenance,
    /// collision policy, advertisement, and dispatch.
    dynamic_tools: Arc<crate::dynamic_tools::DynamicToolRegistry>,
    /// Session-level disabled-tool mask. Names here are hidden from the model
    /// (their schemas are omitted from `ModelRequest`) and rejected at
    /// dispatch, but the tool stays installed so it can be re-enabled without
    /// rebuilding the agent. Toggled from the session modal via
    /// `set_tool_enabled` / `ToggleTool`.
    disabled_tools: Arc<std::sync::Mutex<HashSet<String>>>,
    /// Hook-installed *temporary* disable mask ([`HookOutcome::ScopeTools`]).
    /// Not persisted: excluded from `disabled_tools_snapshot()` so it never
    /// reaches the session store. Auto-restored at the configured
    /// [`muta_contracts::RestorePoint`]. See [`ScopedToolDisable`].
    scoped_disabled_tools: Arc<std::sync::Mutex<ScopedToolDisable>>,
    /// The unified three-bucket tool manager (kimi-code port). The single
    /// authority for classification, per-turn schema (`loop_tools`), and
    /// dispatch lookup. Shares storage Arcs with the agent's own fields so
    /// both see the same live state; it also solely owns the `user` bucket
    /// (SDK/RPC-injected tools — empty today, wired so the classification
    /// and name-clash policy are stable from day one). See
    /// [`crate::tool_manager`].
    tool_manager: crate::tool_manager::ToolManager,
    /// Unified task list, the single source of truth for "what is left to
    /// do." Drives the sticky panel and persists across restarts. Shared
    /// with the concrete `todo` / `todo_update` tools installed by
    /// [`crate::tool_integration`].
    todos: Arc<std::sync::Mutex<muta_contracts::TodoList>>,
    /// Harness round counter, bumped at the start of every `execute_round`.
    /// Shared with the todo tools so they can stamp
    /// `updated_at_round` for the TUI stale detector.
    round_counter: Arc<std::sync::Mutex<u64>>,
    permissions: crate::permission_store::PermissionStore,
    /// Canonicalized additional workspace roots (ADR-0142), set once by the
    /// assembling bootstrap. Kept as an owned copy so system-prompt assembly
    /// never re-reads the project config mid-session.
    additional_workspace_roots: Vec<std::path::PathBuf>,
    /// Workspace authority is orthogonal to interaction posture. Shared with
    /// spawned runners so delegation cannot silently widen the parent's grant.
    workspace_security: Arc<std::sync::Mutex<muta_contracts::WorkspaceSecuritySnapshot>>,
    /// Session-scoped workspace confinement bypass handle.
    unconfined: muta_contracts::SharedUnconfined,
    /// Content-attested project instructions from the Rules asset domain.
    /// Replaced live when `/trust` or `/untrust` changes admission.
    project_rules: Arc<std::sync::RwLock<String>>,
    /// Parked interactive-input requests (L3.5 β). Mirrors `ask_user`.
    pub(crate) skills_registry: skills::SkillRegistry,
    thread_id: Arc<std::sync::Mutex<Option<String>>>,
    accounting_actor_id: std::sync::Mutex<String>,
    /// Context-pressure threshold (in tokens) above which the harness asks the
    /// [`ContextProjectionGate`] to project the model-visible window between
    /// ReAct turns. `0` disables mid-round projection. Derived from the active
    /// model's context window.
    context_prune_threshold_tokens: Arc<std::sync::Mutex<usize>>,
    /// Optional mid-turn model-context projection gate.
    context_projection_gate: Arc<std::sync::Mutex<Option<Arc<dyn ContextProjectionGate>>>>,
    /// Opt-in hard-stop budget (ADR-0018): abort a round after this many ReAct
    /// turns. Seeded from `Config::master.hard_stop_turns` (default `0`
    /// = uncapped, matching ADR-0009) and mutated at runtime via
    /// `set_hard_stop_turns`. This is the sole execution cap; session review
    /// is on-demand (`/review`) and never aborts a round.
    hard_stop_turns: Arc<std::sync::Mutex<usize>>,
    /// Advanced pre-dispatch doom-loop guard configuration. Default
    /// **enabled** (`window: 16`, `threshold: 3` — ADR-0113 §5 flipped it
    /// on, ADR-0148 relaxed the trip point); seeded from
    /// `[master.doom_guard]` in `config.toml` and forced to
    /// [`muta_contracts::DoomGuardConfig::disabled`] for runners and the review
    /// diagnostic. Held behind an `Arc<RwLock>` because master-profile
    /// overlays can replace the configuration atomically; the per-round guard
    /// reads it when `RoundState` is constructed.
    doom_guard_config: Arc<std::sync::RwLock<muta_contracts::DoomGuardConfig>>,
    /// Unified interaction controller governing human posture, stdin policy,
    /// and autonomous fallback behaviors.
    pub(crate) interaction: Arc<crate::interaction::InteractionController>,
    /// ADR-0141: the single owner of parked human-decision oneshots
    /// (permission / ask_user / interactive input).
    human_broker: crate::human_broker::HumanRequestBroker,

    /// Command-aware safety policy for `bash`. This sits above the ordinary
    /// permission broker so broad approvals such as `bash *` cannot silently
    /// authorize destructive commands like `git reset --hard`.
    bash_policy: std::sync::RwLock<crate::bash_policy::BashPolicy>,
    /// Runtime operation boundary for this agent (ADR-0028). The main agent is
    /// unrestricted ([`muta_contracts::OperationScope::unrestricted`]); an runner
    /// carries the scope resolved from its profile's `write_paths` and
    /// `command_allowlist` grants. Enforced at the `execute_tool` funnel for
    /// every admitted tool whose [`muta_contracts::ScopeTarget`] falls outside the
    /// granted scope, before the permission broker — a hard boundary, not a
    /// prompt.
    operation_scope: std::sync::Mutex<muta_contracts::OperationScope>,
    /// Lifecycle event hooks (ADR-0025). Installed once at startup from the
    /// `[hooks]` config by the CLI; empty by default (runners, tests). Read
    /// at the PreToolUse / PostToolUse / Stop insertion points. Held as a
    /// swappable `Arc` behind a `Mutex` so [`Agent::set_hooks`] can replace the
    /// whole registry without the insertion points holding the lock across the
    /// async `fire` — they clone the `Arc` and drop the guard first.
    hooks: crate::hook_runner::HookRunner,
    /// Inbound steering inbox — the down-direction of full-duplex (ADR-0029).
    /// `None` for agents that were never given a handle (the top-level agent
    /// driven directly by the harness, legacy tests); lazily created by
    /// [`Agent::install_inbox`], which a spawned runner's dispatcher
    /// (`RunnerTool`) calls so the parent can steer it mid-round. The driver loop
    /// `take`s the receiver at round entry and drains it at every ReAct-turn
    /// boundary (see [`Agent::drain_inbox`]). Carries only the
    /// "new-input / control" class ([`AgentOp`]); the request/reply class
    /// (permission / ask_user) bypasses this queue and resolves the parked
    /// oneshot directly via `reply_permission` / `reply_user_question`, since a
    /// reply must unblock a tool parked mid-round and cannot wait for the loop.
    inbox_tx: std::sync::Mutex<Option<mpsc::UnboundedSender<AgentOp>>>,
    inbox_rx: std::sync::Mutex<Option<mpsc::UnboundedReceiver<AgentOp>>>,
    /// Inbound steering and follow-up queues for the currently running master/side round.
    /// This is deliberately separate from the runner `AgentOp` inbox: submit,
    /// cancel, and boundary admission all take this one mutex, which gives the
    /// UI an exact answer in the cancellation-vs-admission race. `None` means
    /// the round is not accepting queued messages.
    session_queues: std::sync::Mutex<Option<SessionQueues>>,
    steering_mode: std::sync::RwLock<muta_contracts::QueueMode>,
    follow_up_mode: std::sync::RwLock<muta_contracts::QueueMode>,
    /// Cumulative milliseconds the current round has spent parked on a human
    /// decision (permission prompt or `ask_user`). Reset to 0 at the start of
    /// each user round and added to at every permission/ask_user `await`. Read
    /// at the round exit gate to derive "active" generation time
    /// (`duration_ms - paused_ms`) for an honest tokens/sec that excludes the
    /// human-thinking pause. `AtomicU64` because the parking sites
    /// (`execute_tool`, the bash policy path, `execute_ask_user`) take `&self`,
    /// not `&mut state`.
    round_paused_ms: std::sync::atomic::AtomicU64,
    /// Who this agent is and what it is for. The single string the system
    /// prompt opens with — supplied by the *embedding* (e.g. the CLI), so this
    /// crate stays identity-agnostic and can be reused by frontends that are
    /// not "muta". See [`AgentIdentity`].
    ///
    /// Behind a `RwLock` so a master-role switch ([`Self::set_identity`],
    /// driven by `/master` / `@master:`) can replace it live and the next
    /// request's system prompt reflects the new preamble without rebuilding the
    /// agent. Readers ([`Self::identity`], system-prompt assembly) take a read
    /// lock and clone; writers take a write lock. Contention is negligible —
    /// identity changes at most once per user command, reads once per request.
    pub(crate) identity: std::sync::RwLock<AgentIdentity>,
    /// Optional mid-round save point invoked at every ReAct-turn boundary
    /// (ADR-0048). The embedding (orchestration) installs a closure that
    /// durably appends the round's new messages to the session log so a crash
    /// after a side-effecting tool call leaves the transcript in sync with the
    /// filesystem instead of rewinding to the previous turn. `None` for
    /// runners, the review diagnostic, and tests — they have no session of
    /// their own to persist, so the turn boundary is a plain no-op there.
    turn_persist: std::sync::Mutex<Option<TurnPersistFn>>,
    /// Request-scoped projector. The agent owns its lifecycle and supplies live
    /// state snapshots; the assembler owns the pure window-to-request transform.
    model_request_assembler: crate::model_request::ModelRequestAssembler,
    /// Per-model tool-variant selection (the **override** axis) for the
    /// *current* model: a `capability → variant_id` map. Seeded from
    /// `[tool_variants."<model-id>"]` config via
    /// [`Agent::set_variant_selection`] and re-seeded on model switch so the
    /// resolved toolset always tracks the live model. Held behind an `Arc` so a
    /// spawned runner — which is an agent on the *same* model — can inherit
    /// the same overrides by sharing this handle (see
    /// [`Agent::variant_selection_handle`]); the agent decides scope, the
    /// model decides variant.
    variant_selection: Arc<std::sync::Mutex<muta_contracts::VariantSelection>>,
    /// This agent's **identity-side selection** of the pool (the agent half of
    /// the two-selector model): the capability scope it admits plus any variant
    /// pins it forces. The master agent is
    /// [`ToolSelection::unrestricted`](muta_contracts::ToolSelection::unrestricted)
    /// — every capability, model-chosen variants. A scoped agent (or a future
    /// role-bound master) narrows this. Composed with the live model's
    /// selection by [`muta_contracts::ToolSet::resolve_for`] every time the toolset
    /// is re-resolved: scope by intersection, variants by agent-over-model
    /// precedence, model capability limits applied hard.
    agent_selection: std::sync::Mutex<muta_contracts::ToolSelection>,
    /// Token-source accounting: running tally of how many tokens each
    /// provider+model reported authoritatively (upstream `usage`) vs. how many
    /// were filled in by the local estimator. Shared with the TUI so the
    /// token-source report modal renders live. `None` for runners/tests that
    /// don't surface the report.
    token_ledger: std::sync::Mutex<Option<Arc<muta_contracts::TokenSourceLedger>>>,
    /// Content-addressed per-message token weights (see
    /// [`muta_contracts::MessageTokenWeights`]). Every estimate path consults
    /// this, so BPE tokenization cost collapses from O(total session bytes)
    /// per pass to O(new bytes since the last pass). Messages are immutable
    /// once written, so the cache never needs invalidation: identical bytes
    /// always yield identical weights. Held behind an `Arc` so off-executor
    /// estimate tasks (spawn_blocking) and the context-projection gates can
    /// share the same cache without borrowing the agent.
    token_weights: std::sync::Arc<muta_contracts::MessageTokenWeights>,
}

/// Capability handle for steering a running agent from the outside — the
/// parent's down-direction of full-duplex (ADR-0029). Cheap to clone (one
/// `Weak` + one `mpsc::Sender`); obtained from [`Agent::install_inbox`] on an
/// `Arc<Agent>` (a spawned runner) and typically lodged in a
/// [`crate::runner_tool::RunnerRegistry`] keyed by the parent tool-call id so
/// the harness can look it up when a request surfaces.
///
/// Two classes of operation, deliberately split:
///
/// - **Steering** ([`AgentOp`], via [`RunnerHandle::submit`]): inject a new
///   user message, a hidden inter-agent note, or interrupt/shutdown. Routed
///   through the agent's inbox and applied at the next ReAct-turn boundary —
///   safe to defer because nothing is blocked on it.
/// - **Request/reply** ([`RunnerHandle::reply_permission`] /
///   [`RunnerHandle::reply_user_question`]): resolve a permission broker or
///   `ask_user` oneshot the runner is parked on **right now**, mid-tool.
///   These bypass the inbox and call the agent's shared-state resolvers
///   directly — a queued reply would deadlock the parked tool.
///
/// The `Weak<Agent>` means the handle observes the agent's lifetime: once the
/// runner's round ends and the dispatcher drops its `Arc`, every method
/// returns `false` / `None` instead of erroring, so a late reply from the UI
/// after the runner finished degrades gracefully.
#[derive(Clone)]
pub struct RunnerHandle {
    weak: std::sync::Weak<Agent>,
    ops: mpsc::UnboundedSender<AgentOp>,
}

impl RunnerHandle {
    /// Submit a steering [`AgentOp`] into the agent's inbox. Returns `false`
    /// if the agent has been dropped (receiver gone) — the op is discarded.
    pub fn submit(&self, op: AgentOp) -> bool {
        self.ops.send(op).is_ok()
    }

    /// Resolve a permission broker request the runner is parked on. Returns
    /// `false` if the agent was dropped or no matching pending request exists.
    /// This is the down-direction counterpart to an up-going
    /// [`AgentEvent::PermissionRequest`] / [`RunnerEvent::PermissionRequest`].
    pub fn reply_permission(&self, request_id: &str, decision: PermissionDecision) -> bool {
        if let Some(agent) = self.weak.upgrade() {
            agent.reply_permission(request_id, decision)
        } else {
            false
        }
    }

    /// Resolve an `ask_user` request the runner is parked on. Returns
    /// `false` if the agent was dropped or no matching pending request exists.
    /// Down-direction counterpart to an up-going
    /// [`AgentEvent::UserQuestionRequest`] / [`RunnerEvent::UserQuestionRequest`].
    /// An empty outer answer vector means the operator cancelled.
    pub fn reply_user_question(&self, request_id: &str, answers: Vec<Vec<String>>) -> bool {
        if let Some(agent) = self.weak.upgrade() {
            agent.reply_user_question(request_id, answers)
        } else {
            false
        }
    }

    /// Resolve an interactive-input request the runner's `bash` is parked on
    /// (L3.5 β). Down-direction counterpart to an up-going
    /// [`AgentEvent::StdinRequest`] / [`RunnerEvent::StdinRequest`].
    pub fn reply_input(&self, request_id: &str, text: String) -> bool {
        if let Some(agent) = self.weak.upgrade() {
            agent.reply_input(request_id, text)
        } else {
            false
        }
    }

    /// Whether the underlying agent is still alive (its dispatcher still holds
    /// the `Arc`). Lets a caller drop a stale handle instead of no-op-ing.
    pub fn is_alive(&self) -> bool {
        self.weak.upgrade().is_some()
    }
}

/// Mutable bookkeeping threaded through one user round's ReAct turns.
///
#[derive(Default)]
pub(crate) struct RoundState {
    token_usage: TokenUsage,
    /// Accumulated provider *generation* time across every completed request
    /// in this round (sum of each `RequestAccountingGuard`'s sealed span).
    /// Excludes tool execution, hooks, and human-decision pauses — it is the
    /// honest denominator for tokens/sec. Folded into `RoundOutcome` at the
    /// round-exit gate.
    generation_ms: u64,
    /// Consecutive ReAct turns whose tool calls were all `Read`-tier. Surfaced
    /// to user-configured `Turn` hooks so a hook can act on "exploration
    /// without progress". Reset to 0 by any turn containing an
    /// `Execute`/`Write` call.
    pub(crate) consecutive_readonly_turns: u32,
    /// The round-scoped guard registry: holds one or more `RoundGuard`s (e.g.
    /// `ReadLoopGuard`) and tool-call data for the ReAct turn just dispatched.
    /// It lives and dies with this `RoundState`, so loop
    /// state never crosses user rounds.
    pub(crate) guards: crate::loop_guard::RoundGuardState,
    /// Exact tool calls that reached a terminal result in this round. The set
    /// becomes an idempotency fence only after a transient provider retry;
    /// normal ReAct turns retain their existing repeat-call behavior.
    completed_tool_calls: HashSet<String>,
    /// Snapshot of calls completed before a transient provider failure. Only
    /// this frozen subset is protected: calls first executed after a retry keep
    /// normal same-round semantics unless a later provider failure checkpoints
    /// them too.
    retry_protected_tool_calls: HashSet<String>,
}

impl RoundState {
    /// Build a fresh per-round guard state with the standard guard set, tuned
    /// by `config`. Whether the guard is *enabled* (allowed to inject) is
    /// controlled by `config.enabled`, checked at the turn boundary in
    /// `Agent::apply_guard_actions` — so the guard state is always present
    /// even when disabled (it just never fires). It lives and dies with this
    /// `RoundState`, so loop state never crosses user rounds.
    fn guards_default(
        config: muta_contracts::DoomGuardConfig,
    ) -> crate::loop_guard::RoundGuardState {
        crate::loop_guard::RoundGuardState::new()
            .with_doom(crate::doom_guard::DoomLoopGuard::new(config))
    }

    pub(crate) fn remember_completed_tool(&mut self, call: &ToolCall) {
        self.completed_tool_calls
            .insert(checkpoint_tool_signature(call));
    }

    fn protect_completed_tools_for_retry(&mut self) {
        self.retry_protected_tool_calls
            .extend(self.completed_tool_calls.iter().cloned());
    }

    pub(crate) fn is_checkpoint_replay(&self, call: &ToolCall) -> bool {
        self.retry_protected_tool_calls
            .contains(&checkpoint_tool_signature(call))
    }
}

/// Exact, stable identity for retry idempotency. JSON arguments are parsed and
/// serialized once so insignificant object-key ordering does not turn the same
/// call into a different identity; malformed argument blobs fall back to their
/// trimmed wire form.
fn checkpoint_tool_signature(call: &ToolCall) -> String {
    let arguments = serde_json::from_str::<serde_json::Value>(&call.arguments)
        .map(|value| value.to_string())
        .unwrap_or_else(|_| call.arguments.trim().to_string());
    format!("{}\u{0}{arguments}", call.name)
}

/// Live state for one streaming user round.
///
/// Orchestration keeps this value across transient provider retries. A retry
/// therefore resumes the exact provider request that failed while preserving
/// completed tool results, loop-guard state, hook scope, accounting, and the
/// steering inbox. `pending_request` stays set until a complete, valid
/// assistant response has been accepted; re-entry while it is set skips
/// request preparation and turn-start hooks so retrying cannot replay work
/// that already happened at the request boundary.
pub(crate) struct StreamingRoundState {
    state: RoundState,
    turn_index: usize,
    /// Number of in-flight stream-loop recoveries already attempted in this
    /// user round. The first detected loop gets one guided retry; a recurrence
    /// is a hard stop. This state survives transient provider retries with the
    /// rest of the round checkpoint.
    stream_loop_recoveries: u8,
    inbox_rx: Option<mpsc::UnboundedReceiver<AgentOp>>,
    started_at: std::time::Instant,
    pending_request: Option<muta_contracts::ModelRequest>,
    session_queue_generation: Option<u64>,
}

impl StreamingRoundState {
    /// How many complete ReAct turns this round has committed — the ordinal
    /// the *next* turn would take (0-based `turn_index`). `/retry` captures
    /// this into a [`muta_contracts::RetryPoint`] so the resumed round
    /// keeps numbering turns contiguously instead of restarting at 0.
    pub(crate) fn committed_turns(&self) -> usize {
        self.turn_index
    }
}

/// A queue of pending messages controlled by a [`muta_contracts::QueueMode`].
#[derive(Debug, Clone)]
pub struct PendingMessageQueue {
    messages: std::collections::VecDeque<muta_contracts::QueuedMessage>,
    pub mode: muta_contracts::QueueMode,
}

impl PendingMessageQueue {
    pub fn new(mode: muta_contracts::QueueMode) -> Self {
        Self {
            messages: std::collections::VecDeque::new(),
            mode,
        }
    }

    pub fn enqueue(&mut self, message: muta_contracts::QueuedMessage) {
        self.messages.push_back(message);
    }

    pub fn has_items(&self) -> bool {
        !self.messages.is_empty()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn drain(&mut self) -> Vec<muta_contracts::QueuedMessage> {
        match self.mode {
            muta_contracts::QueueMode::All => self.messages.drain(..).collect(),
            muta_contracts::QueueMode::OneAtATime => {
                self.messages.pop_front().into_iter().collect()
            }
        }
    }

    pub fn drain_all(&mut self) -> Vec<muta_contracts::QueuedMessage> {
        self.messages.drain(..).collect()
    }

    pub fn cancel(&mut self, input_id: &str) -> Option<muta_contracts::QueuedMessage> {
        let position = self
            .messages
            .iter()
            .position(|input| input.id == input_id)?;
        self.messages.remove(position)
    }

    pub fn clear(&mut self) {
        self.messages.clear();
    }
}

struct SessionQueues {
    session_id: String,
    generation: u64,
    steering: PendingMessageQueue,
    follow_up: PendingMessageQueue,
}

/// Result of one tool-execution phase, returned by the cancellation-aware
/// executors ([`Agent::schedule_tool_calls`] and
/// [`Agent::execute_tool_evented`]). The executors never return
/// `Err(HarnessError::Interrupted)` themselves anymore: when the user
/// interrupts a turn they signal cooperatively-cancellable in-flight calls
/// (runners), drain them within a bounded grace period, and report
/// `interrupted: true` with whatever results were recovered. The caller
/// ([`Agent::dispatch_finalize`]) records the recovered results, then
/// propagates the interruption itself — so an interrupted runner's partial
/// transcript survives into the persisted transcript even though the round
/// ends as interrupted.
pub(crate) struct ConcurrentOutcome {
    /// Per-input results in input order. A `None` slot means the call was
    /// dropped by the cancel grace deadline (no result recovered); the
    /// executor already paired it with a terminal [`AgentEvent::ToolCancelled`].
    pub(crate) results: Vec<Option<(ToolOutput, u64)>>,
    /// Whether the cancellation token fired during execution.
    pub(crate) interrupted: bool,
}

/// Single-call counterpart of [`ConcurrentOutcome`] for
/// [`Agent::execute_tool_evented`]. `result` is `Some` when the call reached a
/// terminal result — normally, or after a graceful drain on interrupt.
pub(crate) struct SingleToolOutcome {
    pub(crate) result: Option<ToolOutput>,
    pub(crate) interrupted: bool,
}

/// RAII settlement for one concrete provider request. Any early-return path
/// (interrupt, timeout, provider error, invalid response) still terminally
/// records the attempt; normal completion explicitly settles it with the
/// provider usage or the local fallback estimate.
struct RequestAccountingGuard {
    ledger: Option<Arc<muta_contracts::TokenSourceLedger>>,
    key: Option<muta_contracts::RequestUsageKey>,
    round: u64,
    turn: u32,
    attempt: u32,
    cancel: CancellationToken,
    projected_prompt_tokens: i64,
    observed_completion_tokens: i64,
    /// Incremental BPE counter for streamed deltas (exact across delta
    /// boundaries; a per-delta sum would over-count merges that span them).
    output_counter: muta_contracts::tokenizer::StreamingCounter,
    observed_usage: Option<TokenUsage>,
    error: Option<String>,
    settled: bool,
    /// Monotonic performance anchors. The request clock starts at the actual
    /// provider-call boundary, after local projection/events. Stream events
    /// are sampled as the provider stream yields them; missing stages remain
    /// `None` rather than becoming fabricated zero-duration measurements.
    started_at: Option<std::time::Instant>,
    stream_ready_at: Option<std::time::Instant>,
    first_output_at: Option<std::time::Instant>,
    last_output_at: Option<std::time::Instant>,
    stream_end_at: Option<std::time::Instant>,
    validated_at: Option<std::time::Instant>,
    first_output_fragment: String,
    output_events: u32,
    generation_ms: u64,
}

impl RequestAccountingGuard {
    fn begin(
        agent: &Agent,
        cancel: &CancellationToken,
        provider: &str,
        model: &str,
        turn_index: usize,
        projected_prompt_tokens: usize,
    ) -> Self {
        let ledger = agent.token_ledger();
        let key = ledger.as_ref().map(|ledger| {
            let thread_id = agent.thread_id().unwrap_or_default();
            let actor_id = agent.accounting_actor_id();
            ledger.begin_request_for_actor(muta_contracts::BeginRequestParams {
                session_id: &thread_id,
                actor_id: &actor_id,
                provider,
                model,
                round: agent.round_count(),
                turn: turn_index.saturating_add(1) as u32,
                projected_prompt_tokens: projected_prompt_tokens as i64,
            })
        });
        let round = agent.round_count();
        let turn = turn_index.saturating_add(1) as u32;
        let attempt = key.as_ref().map_or(1, |key| key.attempt);
        Self {
            ledger,
            key,
            round,
            turn,
            attempt,
            cancel: cancel.clone(),
            projected_prompt_tokens: projected_prompt_tokens as i64,
            observed_completion_tokens: 0,
            output_counter: muta_contracts::tokenizer::StreamingCounter::new(),
            observed_usage: None,
            error: None,
            settled: false,
            started_at: None,
            stream_ready_at: None,
            first_output_at: None,
            last_output_at: None,
            stream_end_at: None,
            validated_at: None,
            first_output_fragment: String::new(),
            output_events: 0,
            generation_ms: 0,
        }
    }

    /// Start the monotonic request clock at the provider-call boundary.
    fn start_request(&mut self) {
        self.started_at.get_or_insert_with(std::time::Instant::now);
    }

    /// The provider returned a live response stream (normally after response
    /// headers were received).
    fn mark_stream_ready(&mut self) {
        self.stream_ready_at
            .get_or_insert_with(std::time::Instant::now);
    }

    fn mark_stream_end(&mut self) {
        self.stream_end_at
            .get_or_insert_with(std::time::Instant::now);
    }

    fn record_error(&mut self, err: impl Into<String>) {
        self.error = Some(err.into());
    }

    fn observe_output(&mut self, text: &str) {
        // Streamed deltas feed an exact incremental BPE counter: BPE is not
        // additive across delta boundaries (merges span them), so summing
        // per-delta counts overestimates by 2–100% depending on chunk size.
        // `push` returns the counter's *running* total — not a per-delta
        // increment — so the count is read off the counter afterwards rather
        // than summed per call (summing would re-count every early token once
        // per later delta; a real interrupted 4 000-delta stream booked 14.7M
        // "completion tokens" and a 130 050 tok/s rate from exactly that).
        self.output_counter.push(text);
        self.observed_completion_tokens = self.output_counter.tokens() as i64;
    }

    /// Observe one provider stream event at a single monotonic instant. One
    /// tool-call event may carry both a name and arguments; they share the
    /// same event timestamp and first-event token bucket.
    fn observe_stream_event(
        &mut self,
        event: &muta_contracts::ProviderStreamEvent,
        received_at: std::time::Instant,
    ) {
        let mut fragments: Vec<&str> = Vec::new();
        match event {
            muta_contracts::ProviderStreamEvent::ModelCatalogEtag(_) => return,
            muta_contracts::ProviderStreamEvent::TextDelta(delta)
            | muta_contracts::ProviderStreamEvent::ReasoningDelta(delta) => {
                if !delta.is_empty() {
                    fragments.push(delta);
                }
            }
            muta_contracts::ProviderStreamEvent::ToolCallDelta {
                name, arguments, ..
            } => {
                if let Some(name) = name.as_deref().filter(|name| !name.is_empty()) {
                    fragments.push(name);
                }
                if !arguments.is_empty() {
                    fragments.push(arguments);
                }
            }
            muta_contracts::ProviderStreamEvent::Usage(usage) => {
                self.observe_usage(*usage);
                return;
            }
            muta_contracts::ProviderStreamEvent::Completed(meta) => {
                if let Some(usage) = meta.usage {
                    self.observe_usage(usage);
                }
                return;
            }
        }

        if fragments.is_empty() {
            return;
        }
        let first_event = self.first_output_at.is_none();
        if first_event {
            self.first_output_at = Some(received_at);
        }
        self.last_output_at = Some(received_at);
        self.output_events = self.output_events.saturating_add(1);
        for fragment in fragments {
            if first_event {
                self.first_output_fragment.push_str(fragment);
            }
            self.observe_output(fragment);
        }
    }

    /// Close the stream counter (finalizing the unfinished trailing pretoken)
    /// so the observed count equals a whole-text tokenization of everything
    /// the attempt streamed. Idempotent.
    fn finish_output(&mut self) {
        let finished = self.output_counter.finish() as i64;
        if finished > self.observed_completion_tokens {
            self.observed_completion_tokens = finished;
        }
    }

    fn observe_usage(&mut self, usage: TokenUsage) {
        self.observed_usage = Some(usage);
    }

    /// Freeze the generation clock at the point a validated assistant response
    /// is available — *before* tool calls are dispatched, so their execution
    /// time never inflates the measured generation span. Safe to call more
    /// than once within one guard; only the first call records a span.
    fn seal_generation(&mut self) {
        if self.validated_at.is_some() {
            return;
        }
        let end = std::time::Instant::now();
        self.validated_at = Some(end);
        self.stream_end_at.get_or_insert(end);
        if let Some(start) = self.started_at {
            self.generation_ms = end.saturating_duration_since(start).as_millis() as u64;
        }
    }

    fn performance(&self) -> muta_contracts::RequestPerformance {
        let offset = |end: Option<std::time::Instant>| {
            Some(end?.saturating_duration_since(self.started_at?).as_micros() as u64)
        };
        let span = |start: Option<std::time::Instant>, end: Option<std::time::Instant>| {
            Some(end?.saturating_duration_since(start?).as_micros() as u64)
        };
        muta_contracts::RequestPerformance {
            stream_ready_us: offset(self.stream_ready_at),
            ttft_us: offset(self.first_output_at),
            stream_us: span(self.first_output_at, self.last_output_at),
            tail_us: span(self.last_output_at, self.stream_end_at),
            e2e_us: offset(self.validated_at),
            streamed_output_tokens: self.observed_completion_tokens.max(0) as u64,
            first_output_tokens: muta_contracts::count_tokens(&self.first_output_fragment) as u64,
            output_events: self.output_events,
            timing_source: muta_contracts::PerformanceTimingSource::ClientObserved,
            stream_token_source: muta_contracts::StreamTokenSource::Cl100k,
            ..Default::default()
        }
    }

    fn performance_snapshot(
        &self,
        completion_tokens: i64,
        usage_source: muta_contracts::RequestUsageSource,
    ) -> muta_contracts::TurnPerformanceSnapshot {
        muta_contracts::TurnPerformanceSnapshot {
            round: self.round,
            turn: self.turn,
            attempt: self.attempt,
            completion_tokens: completion_tokens.max(0) as u64,
            usage_source,
            performance: self.performance(),
        }
    }

    fn settle(
        &mut self,
        status: muta_contracts::RequestUsageStatus,
        usage: Option<TokenUsage>,
        estimated_completion_tokens: i64,
    ) {
        self.settle_with_error(
            status,
            usage,
            estimated_completion_tokens,
            self.error.clone(),
        );
    }

    fn settle_with_error(
        &mut self,
        status: muta_contracts::RequestUsageStatus,
        usage: Option<TokenUsage>,
        estimated_completion_tokens: i64,
        error: Option<String>,
    ) {
        if self.settled {
            return;
        }
        self.seal_generation();
        if let (Some(ledger), Some(key)) = (&self.ledger, &self.key) {
            ledger.settle_request_with_performance_and_error(
                key,
                status,
                usage,
                estimated_completion_tokens,
                self.generation_ms,
                Some(self.performance()),
                error,
            );
        }
        self.settled = true;
    }
}

impl Drop for RequestAccountingGuard {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        self.seal_generation();
        // Finalize the streamed-output counter so the estimate equals a
        // whole-text count of everything the attempt streamed before it was
        // interrupted or failed (the interrupted path cannot go through
        // `book_turn_usage`, which normally closes the counter).
        self.finish_output();
        let status = if self.cancel.is_cancelled() {
            muta_contracts::RequestUsageStatus::Interrupted
        } else {
            muta_contracts::RequestUsageStatus::Failed
        };
        self.settle_with_error(
            status,
            self.observed_usage,
            self.observed_completion_tokens,
            self.error.clone(),
        );
    }
}

/// Construction-time configuration for an [`Agent`].
///
/// System-prompt policy is assembled before the agent starts running and is immutable
/// afterwards. This keeps request preparation deterministic while allowing an
/// embedding to add product-specific sections or replace the composition for a
/// specialized agent such as the session reviewer.
pub struct AgentBuilder {
    provider: Arc<dyn Provider>,
    toolset: muta_contracts::ToolSet,
    skills_registry: skills::SkillRegistry,
    identity: AgentIdentity,
    model_request_assembler: crate::model_request::ModelRequestAssembler,
}

impl AgentBuilder {
    fn new(
        provider: Arc<dyn Provider>,
        toolset: muta_contracts::ToolSet,
        identity: AgentIdentity,
    ) -> Self {
        Self {
            provider,
            toolset,
            skills_registry: skills::SkillRegistry::empty(),
            identity,
            model_request_assembler: crate::model_request::ModelRequestAssembler::new(
                crate::model_request::default_system_prompt_registry(),
            ),
        }
    }

    /// Add one caller-supplied tool to this agent's capability set.
    ///
    /// Agent-owned stateful identities are installed during build and take
    /// precedence over a caller tool with the same `(name, variant)`.
    pub fn with_tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.toolset.insert(tool);
        self
    }

    /// Add caller-supplied tools to this agent's capability set.
    pub fn with_tools(mut self, tools: impl IntoIterator<Item = Arc<dyn Tool>>) -> Self {
        for tool in tools {
            self.toolset.insert(tool);
        }
        self
    }

    /// Attach a live skill registry. Agents without one use an empty registry
    /// and expose no skill tools or implicit skill context.
    pub fn with_skills(mut self, registry: skills::SkillRegistry) -> Self {
        self.skills_registry = registry;
        self
    }

    /// Add an embedding-owned section to the default system-prompt policy.
    pub fn register_system_prompt_section<S: crate::SystemPromptSection + 'static>(
        mut self,
        section: S,
    ) -> Result<Self, crate::SystemPromptRegistryError> {
        self.model_request_assembler
            .registry_mut()
            .try_register(section)?;
        Ok(self)
    }

    /// Disable a registered default or custom section by its stable id.
    pub fn disable_system_prompt_section(
        mut self,
        id: &str,
    ) -> Result<Self, crate::SystemPromptRegistryError> {
        self.model_request_assembler.registry_mut().disable(id)?;
        Ok(self)
    }

    /// Override a registered section's semantic ordering in the final composition.
    pub fn order_system_prompt_section(
        mut self,
        id: &str,
        order: crate::InstructionOrder,
    ) -> Result<Self, crate::SystemPromptRegistryError> {
        self.model_request_assembler
            .registry_mut()
            .set_order(id, order)?;
        Ok(self)
    }

    /// Override a registered section's rank in the final composition.
    pub fn rank_system_prompt_section(
        mut self,
        id: &str,
        rank: u32,
    ) -> Result<Self, crate::SystemPromptRegistryError> {
        self.model_request_assembler
            .registry_mut()
            .set_rank(id, rank)?;
        Ok(self)
    }

    /// Replace the default composition wholesale.
    pub fn with_system_prompt_registry(mut self, registry: crate::SystemPromptRegistry) -> Self {
        self.model_request_assembler.replace_registry(registry);
        self
    }

    /// Freeze the configuration and construct the agent.
    pub fn build(self) -> Agent {
        Agent::from_toolset_with_model_request_assembler(
            self.provider,
            self.toolset,
            self.skills_registry,
            self.identity,
            self.model_request_assembler,
        )
    }
}

/// Outcome returned by the agent after running one round.
#[derive(Debug, Clone)]
pub struct RoundOutcome {
    pub message: crate::Message,
    pub token_usage: TokenUsage,
    pub duration_ms: u64,
    /// Milliseconds of `duration_ms` spent parked on a human decision (a
    /// permission prompt or an `ask_user`). The "active" generation time is
    /// `duration_ms - paused_ms`; the harness derives an honest tokens/sec
    /// from the active time so the human-thinking pause never drags the
    /// measured server throughput down.
    pub paused_ms: u64,
    /// Time the model actually spent *generating* across this round's
    /// completed provider requests — excluding tool execution, hooks, and
    /// human-decision pauses. The most accurate denominator for tokens/sec.
    pub generation_ms: u64,
}

mod execution;
mod rounds;
mod state;
mod steering;
mod tools_admin;

pub(crate) use rounds::ToolResultRecord;

/// Render a missing runtime grant without conflating it with project asset
/// trust. This is returned when no interactive approver is available.
fn permission_required_output(request: &muta_contracts::PermissionRequest) -> ToolOutput {
    use muta_contracts::ToolPermissionPayload;

    let operation = match request.submission.as_ref().map(|s| &s.payload) {
        Some(ToolPermissionPayload::Command { command, .. }) => {
            format!("Command '{command}' requires runtime execution grant.")
        }
        Some(ToolPermissionPayload::FileEdit { paths, operation }) => format!(
            "File operation '{operation}' on '{}' requires runtime file-modification grant.",
            paths.join(", ")
        ),
        Some(ToolPermissionPayload::Process { target, action }) => {
            format!("Process operation '{action}' on '{target}' requires runtime lifecycle grant.")
        }
        Some(ToolPermissionPayload::Generic { summary, .. }) => {
            format!("External operation '{summary}' requires runtime grant.")
        }
        None => format!(
            "Tool '{}' for scope '{}' requires runtime grant.",
            request.tool, request.scope
        ),
    };
    ToolOutput::Error {
        message: format!(
            "[permission required] {operation}\nAdd a permission rule in settings or approve interactively."
        ),
        detail: None,
    }
}

fn valid_assistant_response(message: &Message) -> bool {
    !message.content.is_empty()
        || message
            .tool_calls
            .as_ref()
            .is_some_and(|calls| !calls.is_empty())
        || message
            .reasoning_content
            .as_ref()
            .is_some_and(|reasoning| !reasoning.is_empty())
}

/// Build the "empty assistant response" error, after logging enough state to
/// diagnose why: whether reasoning came through, whether any tool calls were
/// parsed, and which provider/model was responsible. The matching per-turn
/// stream summary (chars fed vs emitted, reasoning/tool-call traffic) is logged
/// by the provider at `muta_contracts::provider=debug`.
fn empty_response_error(response: &Message) -> HarnessError {
    tracing::warn!(
        target: "muta_contracts::agent",
        provider = ?response.provider,
        model = ?response.model,
        content_chars = response.content.len(),
        reasoning_chars = response
            .reasoning_content
            .as_ref()
            .map(|s| s.len())
            .unwrap_or(0),
        tool_calls = response.tool_calls.as_ref().map(|c| c.len()).unwrap_or(0),
        "empty assistant response: provider returned no content and no tool calls",
    );
    HarnessError::Other(
        "Provider returned an empty assistant response (no content, no tool calls).".to_string(),
    )
}

/// Drop assistant messages that carry neither text nor a tool call — the model
/// occasionally emits an empty assistant frame that would otherwise confuse
/// the next provider request. Called by the shared request assembler, which
/// both turn loops route through (ADR-0061).
pub(crate) fn remove_empty_assistant_messages(messages: &mut Vec<Message>) {
    messages.retain(|message| message.role != Role::Assistant || valid_assistant_response(message));
}

// ---------------------------------------------------------------------------
// PermissionContext: the agent's implementation of the policy-chain capability
// trait. Policies reach the agent's async machinery (hooks, bash policy,
// permission store) through this, keeping permission_policy.rs decoupled from
// the concrete Agent type.
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl crate::permission_policy::PermissionContext for Agent {
    async fn check_pre_tool_use(
        &self,
        tool_name: &str,
        tool_input: &serde_json::Value,
    ) -> crate::hooks::PreToolUseVerdict {
        self.hooks()
            .check_pre_tool_use(
                tool_name,
                tool_input,
                &self.hook_session_id(),
                self.hook_cwd().as_deref(),
            )
            .await
    }

    fn apply_scoped_disables(&self, disables: &[(String, muta_contracts::RestorePoint)]) {
        // Delegate to the existing agent method (same signature).
        Agent::apply_scoped_disables(self, disables);
    }

    async fn check_bash_policy(
        &self,
        command: &str,
        _arguments: &str,
    ) -> crate::permission_policy::BashVerdict {
        // The single source of truth for the chain's BashPolicy gate. Returns
        // a disjoint Allow / Confirm / Deny verdict so the gate can decide
        // everything (including the interactive confirm) without the caller
        // re-evaluating outside the chain.
        let policy = self
            .bash_policy
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(decision) = policy.evaluate(command) else {
            return crate::permission_policy::BashVerdict::Allow;
        };
        match decision.action {
            crate::bash_policy::BashPolicyAction::Deny => {
                tracing::warn!(command = %command, rule = %decision.name, "bash command blocked by policy");
                crate::permission_policy::BashVerdict::Deny {
                    output: decision.blocked_output(command),
                }
            }
            crate::bash_policy::BashPolicyAction::Confirm => {
                crate::permission_policy::BashVerdict::Confirm { match_: decision }
            }
            crate::bash_policy::BashPolicyAction::Allow => {
                crate::permission_policy::BashVerdict::Allow
            }
        }
    }

    fn permissions(&self) -> &crate::permission_store::PermissionStore {
        &self.permissions
    }
}
#[cfg(test)]
mod tests {
    use super::{
        RoundState, ScopedToolDisable, checkpoint_tool_signature, permission_required_output,
        runner_result_text,
    };

    #[test]
    fn missing_command_authority_has_runtime_only_guidance() {
        let command = "pwd; ls -la".to_string();
        let request = muta_contracts::PermissionRequest {
            id: String::new(),
            tool: "execute_command".to_string(),
            label: "run command".to_string(),
            description: String::new(),
            arguments: String::new(),
            scope: command.clone(),
            elevation: false,
            one_off: false,
            origin: None,
            hazard: Some(muta_contracts::HazardLevel::CommandExecution),
            submission: Some(muta_contracts::ToolPermissionSubmission {
                hazard_level: muta_contracts::HazardLevel::CommandExecution,
                label: "run command".to_string(),
                description: String::new(),
                scope: command.clone(),
                payload: muta_contracts::ToolPermissionPayload::Command {
                    command: command.clone(),
                    cwd: None,
                    kill_spec: muta_contracts::ProcessKillSpec {
                        command: "pwd".to_string(),
                        process_group_killable: true,
                        pkill_target: "pkill -f pwd".to_string(),
                        cwd: None,
                    },
                },
            }),
        };

        let output = permission_required_output(&request).to_text();
        assert!(output.contains(
            "[permission required] Command 'pwd; ls -la' requires runtime execution grant."
        ));
        assert!(output.contains("Add a permission rule in settings or approve interactively."));
        assert!(!output.contains("/trust"));
    }

    fn tool_call(id: &str, arguments: &str) -> muta_contracts::ToolCall {
        muta_contracts::ToolCall {
            id: id.to_string(),
            name: "write_file".to_string(),
            arguments: arguments.to_string(),
        }
    }

    #[test]
    fn checkpoint_tool_identity_ignores_json_object_key_order() {
        let first = tool_call("first", r#"{"path":"x","content":"y"}"#);
        let retried = tool_call("retry", r#"{"content":"y","path":"x"}"#);
        assert_eq!(
            checkpoint_tool_signature(&first),
            checkpoint_tool_signature(&retried)
        );
    }

    #[test]
    fn provider_retry_protects_only_calls_completed_before_its_checkpoint() {
        let before_retry = tool_call("first", r#"{"path":"before"}"#);
        let after_retry = tool_call("second", r#"{"path":"after"}"#);
        let mut state = RoundState::default();
        state.remember_completed_tool(&before_retry);
        state.protect_completed_tools_for_retry();
        state.remember_completed_tool(&after_retry);

        assert!(state.is_checkpoint_replay(&before_retry));
        assert!(!state.is_checkpoint_replay(&after_retry));
    }

    /// The successful runner result carries the `[<tool> result]:` header, the
    /// original summary verbatim, and the success re-anchor note.
    #[test]
    fn runner_result_text_reanchors_on_success() {
        let text = runner_result_text("runner", "Found the symbol in lib.rs", false, false);
        assert!(
            text.starts_with("[runner result]:\n"),
            "header present: {text}"
        );
        assert!(
            text.contains("Found the symbol in lib.rs"),
            "summary preserved verbatim: {text}"
        );
        // The anchor must pin the master's write capability back to the
        // master and call out the read-only scope as runner-only.
        assert!(
            text.contains("applies to the runner only"),
            "anchor scope pin missing: {text}"
        );
        assert!(
            text.contains("retain your full toolset"),
            "master re-anchor missing: {text}"
        );
    }

    /// A failed runner carries a different (re-delegate-or-act-directly) anchor,
    /// and still preserves the partial summary for the master to act on.
    #[test]
    fn runner_result_text_reanchors_on_failure() {
        let text = runner_result_text("runner", "partial findings before crash", true, false);
        assert!(
            text.contains("partial findings before crash"),
            "partial summary preserved: {text}"
        );
        assert!(
            text.contains("could not complete its sub-task"),
            "failure anchor missing: {text}"
        );
        // Both anchors must re-affirm the master retains write capability.
        assert!(
            text.contains("retain your full toolset"),
            "master re-anchor missing on failure: {text}"
        );
        // And must NOT carry the success-only phrasing (regression guard against
        // the success anchor leaking onto a failed runner).
        assert!(
            !text.contains("applies to the runner only"),
            "success anchor leaked onto failure: {text}"
        );
    }

    /// The re-anchor is unconditional for any runner result — a regression guard
    /// that a future refactor cannot silently drop it.
    #[test]
    fn runner_result_text_anchor_is_unconditional() {
        for (failed, interrupted) in [(false, false), (true, false), (false, true)] {
            let text = runner_result_text("runner", "x", failed, interrupted);
            assert!(
                text.contains("[system]"),
                "system anchor tag present (failed={failed}, interrupted={interrupted}): {text}"
            );
        }
    }

    /// An interrupted runner gets its own re-anchor: the partial findings are
    /// real work to continue, not an error to work around — and the read-only
    /// framing still does not transfer to the master.
    #[test]
    fn runner_result_text_reanchors_interruption() {
        let text = runner_result_text("runner", "found 2 of 5 handlers", false, true);
        assert!(
            text.contains("found 2 of 5 handlers"),
            "partial summary preserved: {text}"
        );
        assert!(
            text.contains("interrupted mid-task"),
            "interruption anchor missing: {text}"
        );
        assert!(
            !text.contains("could not complete its sub-task"),
            "failure anchor leaked onto interruption: {text}"
        );
        assert!(
            text.contains("retain your full toolset"),
            "master re-anchor missing: {text}"
        );
    }

    use muta_contracts::RestorePoint;

    /// A scoped disable hides the tool until its restore point fires.
    #[test]
    fn scoped_disable_hides_until_restore() {
        let mut scoped = ScopedToolDisable::default();
        assert!(!scoped.contains("execute_command"));
        scoped.disable("execute_command", RestorePoint::TurnEnd);
        assert!(scoped.contains("execute_command"));
        scoped.restore_turn_end();
        assert!(
            !scoped.contains("execute_command"),
            "TurnEnd restore must re-enable the tool"
        );
        assert!(scoped.is_empty(), "both buckets drained");
    }

    /// `TurnEnd` restore clears the turn-scoped bucket only; `RoundEnd`
    /// disables survive until the user-round boundary.
    #[test]
    fn turn_end_restore_keeps_round_end_disables() {
        let mut scoped = ScopedToolDisable::default();
        scoped.disable("execute_command", RestorePoint::TurnEnd);
        scoped.disable("edit_text", RestorePoint::RoundEnd);
        scoped.restore_turn_end();
        assert!(
            !scoped.contains("execute_command"),
            "TurnEnd disable must be restored at the ReAct-turn boundary"
        );
        assert!(
            scoped.contains("edit_text"),
            "RoundEnd disable must survive the ReAct-turn boundary"
        );
    }

    /// Nested disables compose via refcount: two hooks disable `execute_command` at
    /// different restore points; the earlier (TurnEnd) restore must NOT bring
    /// it back while the later (RoundEnd) is still in effect.
    #[test]
    fn nested_disables_refcount_correctly() {
        let mut scoped = ScopedToolDisable::default();
        scoped.disable("execute_command", RestorePoint::RoundEnd);
        scoped.disable("execute_command", RestorePoint::TurnEnd);
        assert!(scoped.contains("execute_command"));
        scoped.restore_turn_end();
        assert!(
            scoped.contains("execute_command"),
            "execute_command still hidden: the RoundEnd disable outlives the TurnEnd restore"
        );
        scoped.restore_round_end();
        assert!(
            !scoped.contains("execute_command"),
            "execute_command back after round end"
        );
    }

    // ── skip_interactive_input wiring (ADR-0043 interactive-input opt-out) ──

    /// Minimal provider mock so an `Agent` can be constructed in unit tests
    /// without a live model. `decide_command_stdin` never reaches the provider, so
    /// the chat/stream impls are unreachable panics.
    struct NoopProvider;

    #[async_trait::async_trait]
    impl muta_contracts::Provider for NoopProvider {
        async fn chat(
            &self,
            _: muta_contracts::ModelRequest,
        ) -> Result<muta_contracts::ProviderCompletion, muta_contracts::ProviderError> {
            unreachable!("decide_command_stdin must not call the provider")
        }
        async fn stream_chat(
            &self,
            _: muta_contracts::ModelRequest,
        ) -> Result<
            futures::stream::BoxStream<'static, Result<String, muta_contracts::ProviderError>>,
            muta_contracts::ProviderError,
        > {
            unreachable!("decide_command_stdin must not call the provider")
        }
    }

    fn stdin_test_agent() -> super::Agent {
        use std::sync::Arc;
        super::Agent::new(
            Arc::new(NoopProvider) as Arc<dyn muta_contracts::Provider>,
            vec![],
            muta_contracts::AgentIdentity::default(),
        )
    }

    /// A `sudo` command (matched by the interactive classifier) must, with
    /// `skip_interactive_input` on, run with stdin **closed** and emit **no**
    /// `InputRequest` — the inline panel never pops. This is the opt-out's core
    /// contract and mirrors the delegated-autonomous path.
    #[tokio::test]
    async fn skip_interactive_input_closes_stdin_without_input_request() {
        use muta_contracts::{AgentEvent, StdinPolicy};
        use tokio::sync::mpsc;
        let agent = stdin_test_agent();
        agent.set_delegated(false);
        agent.set_skip_interactive_input(true);

        let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
        let policy = agent
            .decide_command_stdin(r#"{"command":"sudo ls /root"}"#, &tx)
            .await;
        assert_eq!(policy, StdinPolicy::Closed, "stdin must be closed");
        assert!(
            rx.try_recv().is_err(),
            "no InputRequest must be emitted under skip_interactive_input"
        );
    }

    /// Without the opt-out (and attended), the same `sudo` command must take the
    /// interactive branch — i.e. emit an `InputRequest` and not return
    /// synchronously. We drain one event then cancel the parked oneshot so the
    /// task ends cleanly. Regression guard: a refactor must not silently route
    /// the interactive path to `Closed` when the opt-out is off.
    #[tokio::test]
    async fn interactive_input_path_emits_request_when_opt_out_is_off() {
        use muta_contracts::AgentEvent;
        use tokio::sync::mpsc;
        let agent = stdin_test_agent();
        agent.set_delegated(false);
        agent.set_skip_interactive_input(false);

        let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
        let (tx_cancel, rx_cancel) = tokio::sync::oneshot::channel::<()>();
        let handle = {
            let args = r#"{"command":"sudo ls /root"}"#.to_string();
            tokio::spawn(async move {
                // Park until the test observes the InputRequest, then drop the
                // agent handle via the cancel signal so the task ends.
                let _ = agent.decide_command_stdin(&args, &tx).await;
                let _ = rx_cancel.await;
            })
        };
        let _ = tx_cancel; // keep ownership

        let got = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("timed out waiting for StdinRequest")
            .expect("channel closed");
        assert!(matches!(got, AgentEvent::StdinRequest(_)));
        // Let the spawned task finish (its agent handle drops, resolving the
        // parked oneshot to None on the next round-end guard in real use; here
        // we just let it go out of scope).
        handle.abort();
    }

    /// `apply_master_profile` must seed `skip_interactive_input` from the
    /// profile's runtime config — the wiring the bootstrap path relies on.
    #[test]
    fn apply_master_profile_seeds_skip_interactive_input() {
        let agent = stdin_test_agent();
        assert!(!agent.skip_interactive_input(), "default off");
        let profile = muta_contracts::MasterPreset::with_identity(
            "code",
            muta_contracts::AgentIdentity::default(),
        )
        .with_runtime_config(muta_contracts::MasterRuntimeConfig {
            skip_interactive_input: true,
            ..Default::default()
        });
        agent.apply_master_profile(&profile);
        assert!(
            agent.skip_interactive_input(),
            "profile overlay took effect"
        );
    }
}
