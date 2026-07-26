use super::*;

use futures::future::BoxFuture;

/// Role-reanchoring note appended to a successful envoy's tool-result text in
/// the principal's transcript. Counters "role bleed": after a run of read-only
/// delegations the model may over-generalize the envoy's read-only framing onto
/// the principal itself. The note pins the boundary explicitly and
/// unconditionally — it does not rely on a `[hooks]` entry, so the guarantee is
/// structural.
const ENVOY_REANCHOR_OK: &str = "\
[system] The read-only / toolset-scoped framing above applies to the envoy only. \
You (the principal agent) retain your full toolset — including write and edit tools \
and the shell — across this delegation. Perform any edits or writes yourself; the \
envoy cannot.";

/// Same role-reanchoring for a *failed* envoy. Reaffirms the boundary and nudges
/// the principal toward acting directly rather than re-delegating a failing
/// sub-task.
const ENVOY_REANCHOR_FAILED: &str = "\
[system] That envoy could not complete its sub-task. Its read-only / toolset-scoped \
framing does not transfer to you: you (the principal agent) retain your full toolset \
— including write and edit tools and the shell. Act directly on the findings above, \
or re-delegate with a narrower scope.";

/// Build the model-visible text for an envoy tool result: the envoy's summary
/// wrapped in the standard `[<tool> result]:` header, followed by a
/// deterministic role-reanchoring note (`ENVOY_REANCHOR_OK` on success,
/// `ENVOY_REANCHOR_FAILED` on `failed`). This is the single choke point where
/// an envoy's read-only framing enters the principal's transcript, so the
/// re-anchor is applied here unconditionally — it cannot be forgotten by a
/// missing `[hooks]` config. Extracted from [`Agent::record_tool_result`] so the
/// contract (the anchor is present, and its tone tracks the failure flag) is
/// unit-testable without a full `Agent` fixture.
pub(crate) fn envoy_result_text(name: &str, summary: &str, failed: bool) -> String {
    let reanchor = if failed {
        ENVOY_REANCHOR_FAILED
    } else {
        ENVOY_REANCHOR_OK
    };
    format!("[{name} result]:\n{summary}\n\n{reanchor}")
}

/// In-memory only mask of tools a hook has temporarily disabled via a
/// [`neenee_core::HookOutcome::ScopeTools`] outcome, partitioned by the
/// [`neenee_core::RestorePoint`] at which each should come back.
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
    fn disable(&mut self, tool: &str, restore: neenee_core::RestorePoint) {
        let bucket = match restore {
            neenee_core::RestorePoint::TurnEnd => &mut self.turn_end,
            neenee_core::RestorePoint::RoundEnd => &mut self.round_end,
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

/// Mid-turn save-point closure installed by orchestration (ADR-0035).
///
/// Invoked at each ReAct-turn boundary with the current full round history.
/// The implementation diffs against its own durable baseline and appends only
/// the new tail to the session event log (see `SessionStore::append_turn`).
/// Errors are surfaced back to the ReAct loop, which treats a persist failure
/// as a round-ending error (better to stop than to keep mutating state that may
/// not be recoverable).
pub(crate) type TurnPersistFn =
    Arc<dyn Fn(&[Message]) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

/// Estimated token shape of the next provider request.
///
/// `history_tokens` is the prepared, non-system conversation, including any
/// skill messages injected for this request. `overhead_tokens` covers the
/// freshly composed system message and currently visible tool schemas.
/// Wire-format framing is intentionally left to the compaction policy's
/// utilization headroom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestTokenEstimate {
    pub history_tokens: usize,
    pub overhead_tokens: usize,
    pub total_tokens: usize,
}

// `AgentIdentity` now lives in `neenee-core` (`identity.rs`) as pure domain
// vocabulary, alongside the role profiles. It is re-exported by name at the
// crate root below and via `pub use neenee_core::*`, so all existing
// `neenee_agent::AgentIdentity` / `crate::AgentIdentity` references keep
// resolving unchanged.

#[derive(Default)]
struct AskUserState {
    pending: HashMap<String, oneshot::Sender<Option<UserQuestionReply>>>,
}

/// Parked oneshots for in-flight interactive-input requests (L3.5 β): a
/// `bash` command classified interactive blocks here until the operator's
/// [`InputReply`] arrives (or `None` on cancel/turn-end).
#[derive(Default)]
struct InputState {
    pending: HashMap<String, oneshot::Sender<Option<InputReply>>>,
}

pub struct Agent {
    pub provider: Arc<dyn Provider>,
    /// The full capability set: every tool keyed by capability, with all its
    /// variants. The single source of truth from which the model-visible
    /// [`resolved_tools`](Self::resolved_tools) view is derived for the active
    /// [`variant_selection`](Self::variant_selection).
    pub(crate) toolset: neenee_core::ToolSet,
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
    /// [`neenee_core::RestorePoint`]. See [`ScopedToolDisable`].
    scoped_disabled_tools: Arc<std::sync::Mutex<ScopedToolDisable>>,
    /// SDK/RPC-injected tools (the `user` bucket). Empty today; the bucket
    /// exists so the three-way classification (builtin/user/mcp) and the
    /// name-clash policy (builtin > user > mcp) are wired now. See
    /// [`crate::tool_manager`].
    user_tools: Arc<std::sync::RwLock<Vec<Arc<dyn neenee_core::Tool>>>>,
    /// The unified three-bucket tool manager (kimi-code port). The single
    /// authority for classification, per-turn schema (`loop_tools`), and
    /// dispatch lookup. Shares storage Arcs with the agent's own fields so
    /// both see the same live state. See [`crate::tool_manager`].
    tool_manager: crate::tool_manager::ToolManager,
    /// Unified task list, the single source of truth for "what is left to
    /// do." Drives the sticky panel and persists across restarts. Shared
    /// with the concrete `todo` / `todo_update` tools installed by
    /// [`crate::tool_integration`].
    todos: Arc<std::sync::Mutex<neenee_core::TodoList>>,
    /// Harness round counter, bumped at the start of every `execute_round`.
    /// Shared with the todo tools so they can stamp
    /// `updated_at_round` for the TUI stale detector.
    round_counter: Arc<std::sync::Mutex<u64>>,
    /// In-memory pursuit state: the active [`Pursuit`], the stop-gate armed
    /// flag, and the iteration counter. See [`crate::pursuit_state::PursuitState`].
    pursuit_state: crate::pursuit_state::PursuitState,
    permissions: crate::permission_store::PermissionStore,
    ask_user: std::sync::Mutex<AskUserState>,
    /// Parked interactive-input requests (L3.5 β). Mirrors `ask_user`.
    input: std::sync::Mutex<InputState>,
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
    /// turns. Seeded from `Config::principal.hard_stop_turns` (default `0`
    /// = uncapped, matching ADR-0009) and mutated at runtime via
    /// `set_hard_stop_turns`. This is the sole execution cap; session review
    /// is on-demand (`/review`) and never aborts a round.
    hard_stop_turns: Arc<std::sync::Mutex<usize>>,
    /// Advanced pre-dispatch doom-loop guard configuration. Default
    /// **disabled** ([`neenee_core::DoomGuardConfig::default`]); seeded from
    /// `[principal.nudge]` in `config.toml` and forced to
    /// [`neenee_core::DoomGuardConfig::disabled`] for envoys and the review
    /// diagnostic. Held behind an `Arc<RwLock>` because principal-profile
    /// overlays can replace the configuration atomically; the per-round guard
    /// reads it when `RoundState` is constructed.
    doom_guard_config: Arc<std::sync::RwLock<neenee_core::DoomGuardConfig>>,
    /// Whether the model may supply stdin bytes for a `bash` call it emits
    /// (the opt-in automatic-flow path, L3.5 α). Default `false`; seeded from
    /// `[principal] allow_model_stdin`. Lock-free so the dispatch site reads
    /// it without contention. When `false` the bash schema exposes no `stdin`
    /// parameter (structurally unreachable from the model); when `true` it
    /// does, and a model-supplied `stdin` is threaded as
    /// [`StdinPolicy::Prefilled`].
    allow_model_stdin: Arc<std::sync::atomic::AtomicBool>,
    /// Command-aware safety policy for `bash`. This sits above the ordinary
    /// permission broker so broad approvals such as `bash *` cannot silently
    /// authorize destructive commands like `git reset --hard`.
    bash_policy: std::sync::RwLock<crate::bash_policy::BashPolicy>,
    /// Registered review dimensions evaluated by the on-demand diagnostic
    /// envoy (`/review`). Defaults to [`crate::default_reviews`] (looping);
    /// empty on envoys (which have no `/review` path).
    reviews: Vec<Arc<dyn SessionReview>>,
    /// Runtime operation boundary for this agent (ADR-0028). The main agent is
    /// unrestricted ([`neenee_core::OperationScope::unrestricted`]); an envoy
    /// carries the scope resolved from its profile's `write_paths` and
    /// `command_allowlist` grants. Enforced at the `execute_tool` funnel for
    /// every admitted tool whose [`neenee_core::ScopeTarget`] falls outside the
    /// granted scope, before the permission broker — a hard boundary, not a
    /// prompt.
    operation_scope: std::sync::Mutex<neenee_core::OperationScope>,
    /// Lifecycle event hooks (ADR-0025). Installed once at startup from the
    /// `[hooks]` config by the CLI; empty by default (envoys, tests). Read
    /// at the PreToolUse / PostToolUse / Stop insertion points. Held as a
    /// swappable `Arc` behind a `Mutex` so [`Agent::set_hooks`] can replace the
    /// whole registry without the insertion points holding the lock across the
    /// async `fire` — they clone the `Arc` and drop the guard first.
    hooks: crate::hook_runner::HookRunner,
    /// Inbound steering inbox — the down-direction of full-duplex (ADR-0029).
    /// `None` for agents that were never given a handle (the top-level agent
    /// driven directly by the harness, legacy tests); lazily created by
    /// [`Agent::install_inbox`], which a spawned envoy's dispatcher
    /// (`EnvoyTool`) calls so the parent can steer it mid-round. The driver loop
    /// `take`s the receiver at round entry and drains it at every ReAct-turn
    /// boundary (see [`Agent::drain_inbox`]). Carries only the
    /// "new-input / control" class ([`AgentOp`]); the request/reply class
    /// (permission / ask_user) bypasses this queue and resolves the parked
    /// oneshot directly via `reply_permission` / `reply_user_question`, since a
    /// reply must unblock a tool parked mid-round and cannot wait for the loop.
    inbox_tx: std::sync::Mutex<Option<mpsc::UnboundedSender<AgentOp>>>,
    inbox_rx: std::sync::Mutex<Option<mpsc::UnboundedReceiver<AgentOp>>>,
    /// Human-authored inserts for the currently running principal/side round.
    /// This is deliberately separate from the envoy `AgentOp` inbox: submit,
    /// cancel, and boundary admission all take this one mutex, which gives the
    /// UI an exact answer in the cancellation-vs-admission race. `None` means
    /// the round is not accepting inserts.
    user_input_queue: std::sync::Mutex<Option<UserInputRound>>,
    /// Who this agent is and what it is for. The single string the system
    /// prompt opens with — supplied by the *embedding* (e.g. the CLI), so this
    /// crate stays identity-agnostic and can be reused by frontends that are
    /// not "neenee". See [`AgentIdentity`].
    pub(crate) identity: AgentIdentity,
    /// Optional mid-round save point invoked at every ReAct-turn boundary
    /// (ADR-0035). The embedding (orchestration) installs a closure that
    /// durably appends the round's new messages to the session log so a crash
    /// after a side-effecting tool call leaves the transcript in sync with the
    /// filesystem instead of rewinding to the previous turn. `None` for
    /// envoys, the review diagnostic, and tests — they have no session of
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
    /// spawned envoy — which is an agent on the *same* model — can inherit
    /// the same overrides by sharing this handle (see
    /// [`Agent::variant_selection_handle`]); the agent decides scope, the
    /// model decides variant.
    variant_selection: Arc<std::sync::Mutex<neenee_core::VariantSelection>>,
    /// This agent's **identity-side selection** of the pool (the agent half of
    /// the two-selector model): the capability scope it admits plus any variant
    /// pins it forces. The principal agent is
    /// [`ToolSelection::unrestricted`](neenee_core::ToolSelection::unrestricted)
    /// — every capability, model-chosen variants. A scoped agent (or a future
    /// role-bound principal) narrows this. Composed with the live model's
    /// selection by [`neenee_core::ToolSet::resolve_for`] every time the toolset
    /// is re-resolved: scope by intersection, variants by agent-over-model
    /// precedence, model capability limits applied hard.
    agent_selection: std::sync::Mutex<neenee_core::ToolSelection>,
    /// Token-source accounting: running tally of how many tokens each
    /// provider+model reported authoritatively (upstream `usage`) vs. how many
    /// were filled in by the local estimator. Shared with the TUI so the
    /// token-source report modal renders live. `None` for envoys/tests that
    /// don't surface the report.
    token_ledger: std::sync::Mutex<Option<Arc<neenee_core::TokenSourceLedger>>>,
}

/// Capability handle for steering a running agent from the outside — the
/// parent's down-direction of full-duplex (ADR-0029). Cheap to clone (one
/// `Weak` + one `mpsc::Sender`); obtained from [`Agent::install_inbox`] on an
/// `Arc<Agent>` (a spawned envoy) and typically lodged in a
/// [`crate::envoy_tool::EnvoyRegistry`] keyed by the parent tool-call id so
/// the harness can look it up when a request surfaces.
///
/// Two classes of operation, deliberately split:
///
/// - **Steering** ([`AgentOp`], via [`EnvoyHandle::submit`]): inject a new
///   user message, a hidden inter-agent note, or interrupt/shutdown. Routed
///   through the agent's inbox and applied at the next ReAct-turn boundary —
///   safe to defer because nothing is blocked on it.
/// - **Request/reply** ([`EnvoyHandle::reply_permission`] /
///   [`EnvoyHandle::reply_user_question`]): resolve a permission broker or
///   `ask_user` oneshot the envoy is parked on **right now**, mid-tool.
///   These bypass the inbox and call the agent's shared-state resolvers
///   directly — a queued reply would deadlock the parked tool.
///
/// The `Weak<Agent>` means the handle observes the agent's lifetime: once the
/// envoy's round ends and the dispatcher drops its `Arc`, every method
/// returns `false` / `None` instead of erroring, so a late reply from the UI
/// after the envoy finished degrades gracefully.
#[derive(Clone)]
pub struct EnvoyHandle {
    weak: std::sync::Weak<Agent>,
    ops: mpsc::UnboundedSender<AgentOp>,
}

impl EnvoyHandle {
    /// Submit a steering [`AgentOp`] into the agent's inbox. Returns `false`
    /// if the agent has been dropped (receiver gone) — the op is discarded.
    pub fn submit(&self, op: AgentOp) -> bool {
        self.ops.send(op).is_ok()
    }

    /// Resolve a permission broker request the envoy is parked on. Returns
    /// `false` if the agent was dropped or no matching pending request exists.
    /// This is the down-direction counterpart to an up-going
    /// [`AgentEvent::PermissionRequest`] / [`EnvoyEvent::PermissionRequest`].
    pub fn reply_permission(&self, request_id: &str, decision: PermissionDecision) -> bool {
        if let Some(agent) = self.weak.upgrade() {
            agent.reply_permission(request_id, decision)
        } else {
            false
        }
    }

    /// Resolve an `ask_user` request the envoy is parked on. Returns
    /// `false` if the agent was dropped or no matching pending request exists.
    /// Down-direction counterpart to an up-going
    /// [`AgentEvent::UserQuestionRequest`] / [`EnvoyEvent::UserQuestionRequest`].
    /// An empty outer answer vector means the operator cancelled.
    pub fn reply_user_question(&self, request_id: &str, answers: Vec<Vec<String>>) -> bool {
        if let Some(agent) = self.weak.upgrade() {
            agent.reply_user_question(request_id, answers)
        } else {
            false
        }
    }

    /// Resolve an interactive-input request the envoy's `bash` is parked on
    /// (L3.5 β). Down-direction counterpart to an up-going
    /// [`AgentEvent::InputRequest`] / [`EnvoyEvent::InputRequest`].
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
    /// Cumulative usage already charged to the active pursuit at a stop-gate
    /// boundary. The next boundary books only the delta, avoiding triangular
    /// over-counting as `token_usage` grows across the whole round.
    pursuit_booked_usage: TokenUsage,
    /// Elapsed round time already charged to the active pursuit.
    pursuit_booked_duration_ms: u64,
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
    /// [`Agent::apply_guard_actions`] — so the guard state is always present
    /// even when disabled (it just never fires). It lives and dies with this
    /// `RoundState`, so loop state never crosses user rounds.
    fn guards_default(config: neenee_core::DoomGuardConfig) -> crate::loop_guard::RoundGuardState {
        crate::loop_guard::RoundGuardState::new()
            .with_doom(crate::doom_guard::DoomLoopGuard::new(config))
    }

    fn remember_completed_tool(&mut self, call: &ToolCall) {
        self.completed_tool_calls
            .insert(checkpoint_tool_signature(call));
    }

    fn protect_completed_tools_for_retry(&mut self) {
        self.retry_protected_tool_calls
            .extend(self.completed_tool_calls.iter().cloned());
    }

    fn is_checkpoint_replay(&self, call: &ToolCall) -> bool {
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
    inbox_rx: Option<mpsc::UnboundedReceiver<AgentOp>>,
    started_at: std::time::Instant,
    pending_request: Option<neenee_core::ModelRequest>,
    user_input_generation: Option<u64>,
}

struct UserInputRound {
    session_id: String,
    generation: u64,
    queue: std::collections::VecDeque<neenee_core::QueuedUserInput>,
}

/// RAII settlement for one concrete provider request. Any early-return path
/// (interrupt, timeout, provider error, invalid response) still terminally
/// records the attempt; normal completion explicitly settles it with the
/// provider usage or the local fallback estimate.
struct RequestAccountingGuard {
    ledger: Option<Arc<neenee_core::TokenSourceLedger>>,
    key: Option<neenee_core::RequestUsageKey>,
    cancel: CancellationToken,
    projected_prompt_tokens: i64,
    observed_completion_tokens: i64,
    observed_usage: Option<TokenUsage>,
    settled: bool,
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
            ledger.begin_request_for_actor(
                &agent.thread_id().unwrap_or_default(),
                &agent.accounting_actor_id(),
                provider,
                model,
                agent.round_count(),
                turn_index.saturating_add(1) as u32,
                projected_prompt_tokens as i64,
            )
        });
        Self {
            ledger,
            key,
            cancel: cancel.clone(),
            projected_prompt_tokens: projected_prompt_tokens as i64,
            observed_completion_tokens: 0,
            observed_usage: None,
            settled: false,
        }
    }

    fn observe_output(&mut self, text: &str) {
        self.observed_completion_tokens = self
            .observed_completion_tokens
            .saturating_add(pressure::estimate_string_tokens(text));
    }

    fn observe_usage(&mut self, usage: TokenUsage) {
        self.observed_usage = Some(usage);
    }

    fn settle(
        &mut self,
        status: neenee_core::RequestUsageStatus,
        usage: Option<TokenUsage>,
        estimated_completion_tokens: i64,
    ) {
        if self.settled {
            return;
        }
        if let (Some(ledger), Some(key)) = (&self.ledger, &self.key) {
            ledger.settle_request(key, status, usage, estimated_completion_tokens);
        }
        self.settled = true;
    }
}

impl Drop for RequestAccountingGuard {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let status = if self.cancel.is_cancelled() {
            neenee_core::RequestUsageStatus::Interrupted
        } else {
            neenee_core::RequestUsageStatus::Failed
        };
        self.settle(status, self.observed_usage, self.observed_completion_tokens);
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
    toolset: neenee_core::ToolSet,
    skills_registry: skills::SkillRegistry,
    identity: AgentIdentity,
    model_request_assembler: crate::model_request::ModelRequestAssembler,
}

impl AgentBuilder {
    fn new(
        provider: Arc<dyn Provider>,
        toolset: neenee_core::ToolSet,
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
}

impl Agent {
    /// Start configuring an agent from a flat tool list.
    pub fn builder(
        provider: Arc<dyn Provider>,
        tools: Vec<Arc<dyn Tool>>,
        identity: AgentIdentity,
    ) -> AgentBuilder {
        AgentBuilder::new(provider, neenee_core::ToolSet::from_tools(tools), identity)
    }

    /// Start configuring an agent from a full multi-variant tool set.
    pub fn builder_from_toolset(
        provider: Arc<dyn Provider>,
        toolset: neenee_core::ToolSet,
        identity: AgentIdentity,
    ) -> AgentBuilder {
        AgentBuilder::new(provider, toolset, identity)
    }

    /// Construct an agent from a flat tool list. The tools are grouped into a
    /// [`neenee_core::ToolSet`] (one capability per [`Tool::name`], one variant
    /// per [`Tool::variant`]) — the common case for a single-variant toolset or
    /// an already-resolved envoy toolset. Use [`Agent::from_toolset`] to
    /// preserve a multi-variant set so per-model variant selection can switch
    /// between variants at runtime.
    pub fn new(
        provider: Arc<dyn Provider>,
        tools: Vec<Arc<dyn Tool>>,
        identity: AgentIdentity,
    ) -> Self {
        Self::from_toolset(provider, neenee_core::ToolSet::from_tools(tools), identity)
    }

    /// Construct an agent from a full [`neenee_core::ToolSet`], preserving every
    /// capability's variants so [`Agent::set_variant_selection`] can swap the
    /// model-visible variant at runtime.
    pub fn from_toolset(
        provider: Arc<dyn Provider>,
        toolset: neenee_core::ToolSet,
        identity: AgentIdentity,
    ) -> Self {
        Self::builder_from_toolset(provider, toolset, identity).build()
    }

    fn from_toolset_with_model_request_assembler(
        provider: Arc<dyn Provider>,
        toolset: neenee_core::ToolSet,
        skills_registry: skills::SkillRegistry,
        identity: AgentIdentity,
        model_request_assembler: crate::model_request::ModelRequestAssembler,
    ) -> Self {
        let pursuit_state = crate::pursuit_state::PursuitState::new();
        let thread_id = Arc::new(std::sync::Mutex::new(None));

        let mut toolset = toolset;
        let round_counter = Arc::new(std::sync::Mutex::new(0u64));
        let todos = Arc::new(std::sync::Mutex::new(neenee_core::TodoList::default()));
        crate::tool_integration::install_agent_owned_tools(
            &mut toolset,
            Arc::clone(&todos),
            Arc::clone(&round_counter),
        );

        // Seed the model-visible view by resolving the pool for the live model
        // with no role restriction and no model variant overrides yet: the
        // principal's identity selection (unrestricted) composed with the
        // model's capability limits. `set_variant_selection` re-resolves once
        // the model's `[tool_variants]` selection is known and on every switch.
        let agent_selection = neenee_core::ToolSelection::unrestricted();
        let seed_model = neenee_core::resolve_model(&provider.model());
        let resolved_tools = Arc::new(std::sync::RwLock::new(toolset.resolve_for(
            &seed_model,
            &agent_selection,
            &neenee_core::ToolSelection::unrestricted(),
        )));
        let dynamic_tools = Arc::new(crate::dynamic_tools::DynamicToolRegistry::default());
        let disabled_tools = Arc::new(std::sync::Mutex::new(HashSet::new()));
        let scoped_disabled_tools =
            Arc::new(std::sync::Mutex::new(ScopedToolDisable::default()));
        let user_tools = Arc::new(std::sync::RwLock::new(Vec::new()));
        // The unified ToolManager view (kimi-code port) owns the single
        // authority for classification, per-turn schema, and dispatch lookup.
        // It shares the storage Arcs with the agent so both reach the same
        // live state. See `tool_manager`.
        let tool_manager = crate::tool_manager::ToolManager::new(
            Arc::clone(&resolved_tools),
            Arc::clone(&dynamic_tools),
            Arc::clone(&user_tools),
            Arc::clone(&disabled_tools),
            Arc::clone(&scoped_disabled_tools),
        );

        Self {
            provider,
            toolset,
            resolved_tools,
            dynamic_tools,
            disabled_tools,
            scoped_disabled_tools,
            user_tools,
            tool_manager,
            todos,
            round_counter,
            pursuit_state,
            permissions: crate::permission_store::PermissionStore::new(),
            ask_user: std::sync::Mutex::new(AskUserState::default()),
            input: std::sync::Mutex::new(InputState::default()),
            skills_registry,
            thread_id,
            accounting_actor_id: std::sync::Mutex::new("principal".to_string()),
            context_prune_threshold_tokens: Arc::new(std::sync::Mutex::new(0)),
            context_projection_gate: Arc::new(std::sync::Mutex::new(None)),
            hard_stop_turns: Arc::new(std::sync::Mutex::new(0)),
            doom_guard_config: Arc::new(std::sync::RwLock::new(
                neenee_core::DoomGuardConfig::default(),
            )),
            allow_model_stdin: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            bash_policy: std::sync::RwLock::new(crate::bash_policy::BashPolicy::default()),
            reviews: crate::default_reviews(),
            operation_scope: std::sync::Mutex::new(neenee_core::OperationScope::unrestricted()),
            hooks: crate::hook_runner::HookRunner::new(),
            inbox_tx: std::sync::Mutex::new(None),
            inbox_rx: std::sync::Mutex::new(None),
            user_input_queue: std::sync::Mutex::new(None),
            identity,
            turn_persist: std::sync::Mutex::new(None),
            model_request_assembler,
            variant_selection: Arc::new(
                std::sync::Mutex::new(neenee_core::VariantSelection::new()),
            ),
            agent_selection: std::sync::Mutex::new(agent_selection),
            token_ledger: std::sync::Mutex::new(None),
        }
    }

    /// Context-pressure threshold (in tokens) for mid-turn relief. `0` (the
    /// default) disables the mid-turn [`ContextProjectionGate`]. Re-seed on provider
    /// switch so the threshold tracks the new model's context window.
    pub fn set_context_prune_threshold(&self, budget_tokens: usize) {
        *self
            .context_prune_threshold_tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = budget_tokens;
    }

    /// Replace the per-model tool-variant selection and re-resolve the
    /// model-visible toolset to match. Seeded from `[tool_variants."<model-id>"]`
    /// config and re-applied on model switch so the resolved variants — and the
    /// live model's hard capability limits (e.g. vision) — always track the
    /// live model. An empty map (the default) realizes every capability with its
    /// model-chosen / default variant.
    pub fn set_variant_selection(&self, selection: neenee_core::VariantSelection) {
        self.reresolve_tools(&selection);
        *self
            .variant_selection
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = selection;
    }

    /// Replace this agent's identity-side selection (capability scope + variant
    /// pins) and re-resolve the model-visible toolset. The principal is
    /// unrestricted by default; this narrows it (e.g. confining a role-bound
    /// principal to a capability subset). The current per-model variant
    /// selection is preserved and re-composed.
    pub fn set_agent_selection(&self, selection: neenee_core::ToolSelection) {
        *self
            .agent_selection
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = selection;
        let model_variants = self
            .variant_selection
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        self.reresolve_tools(&model_variants);
    }

    /// Re-resolve [`resolved_tools`](Self::resolved_tools) from the pool for the
    /// live model, composing this agent's identity selection with the model's
    /// selection (`model_variants` overrides + the model's hard capability
    /// limits). The single choke point through which both the principal seed and
    /// every model/selection switch flow, so the schema sent to the provider and
    /// the dispatch table always reflect `agent_scope ∩ model_caps`.
    fn reresolve_tools(&self, model_variants: &neenee_core::VariantSelection) {
        let model = neenee_core::resolve_model(&self.provider.model());
        let agent_selection = self
            .agent_selection
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let model_selection =
            neenee_core::ToolSelection::unrestricted().with_variants(model_variants.clone());
        *self
            .resolved_tools
            .write()
            .unwrap_or_else(|e| e.into_inner()) =
            self.toolset
                .resolve_for(&model, &agent_selection, &model_selection);
    }

    /// Every currently installed tool, including dynamic sources. Static
    /// capabilities win name collisions; dynamic source order is deterministic.
    pub fn installed_tools(&self) -> Vec<Arc<dyn Tool>> {
        // Delegate to the unified ToolManager — the single authority for the
        // three-bucket classification (builtin/user/mcp) and name-clash priority.
        self.tool_manager
            .installed()
            .into_iter()
            .map(|s| s.tool)
            .collect()
    }

    /// The unified tool manager (kimi-code port). Exposed so the dispatcher /
    /// model-request assembly can call its authoritative methods directly.
    #[allow(dead_code)]
    pub(crate) fn tool_manager(&self) -> &crate::tool_manager::ToolManager {
        &self.tool_manager
    }

    /// The permission policy chain for this agent. Built fresh per call.
    /// Holds the synchronous permission gates; the asynchronous gates (hook,
    /// bash-policy, ask_user, broker park) stay in `execute_tool`.
    pub(crate) fn permission_chain(&self) -> crate::permission_policy::PermissionChain {
        crate::permission_policy::PermissionChain::new(
            crate::permission_policy::default_chain(),
        )
    }

    /// Snapshot the live state available to declarative system-prompt policy.
    fn system_prompt_context(&self, tools: &[Arc<dyn Tool>]) -> crate::SystemPromptContext {
        let tool_names = tools.iter().map(|tool| tool.name().to_string()).collect();
        let model_guidance = neenee_core::resolve_model(&self.provider.model()).model_guidance;
        let provider_guidance = self.provider.prompt_hints().system_guidance;

        crate::SystemPromptContext {
            identity_preamble: self.identity.preamble(),
            pursuit: self.get_pursuit(),
            tool_names,
            model_guidance,
            provider_guidance,
            unattended: self.get_unattended(),
        }
    }

    /// Build one immutable provider request from a borrowed conversation window.
    /// Implicit skill loading is evaluated on a private copy so estimates and
    /// debug previews use the same projection without mutating durable state.
    fn model_request(&self, messages: &[Message]) -> neenee_core::ModelRequest {
        let mut enriched = messages.to_vec();
        crate::conversation_context::inject_mentioned_skills(&self.skills_registry, &mut enriched);
        let tools = self.visible_tools();
        let context = self.system_prompt_context(&tools);
        self.model_request_assembler
            .assemble(&enriched, &context, &tools)
    }

    fn estimate_model_request(request: &neenee_core::ModelRequest) -> RequestTokenEstimate {
        // Use per-message wire weight here rather than `estimate_tokens`: the
        // latter intentionally includes persisted envoy children, while the
        // provider receives only the parent message's rendered result.
        let message_tokens = |messages: &[Message]| {
            messages
                .iter()
                .map(neenee_core::estimate_message_tokens)
                .sum::<i64>()
                .max(0) as usize
        };
        let history = request
            .messages
            .iter()
            .filter(|message| message.role != Role::System)
            .cloned()
            .collect::<Vec<_>>();
        let history_tokens = message_tokens(&history);
        let prepared_message_tokens = message_tokens(&request.messages);
        let tool_schema_tokens = request
            .tool_specs
            .iter()
            .map(|spec| {
                // Estimate over the full spec (name + description + the JSON
                // Schema parameters), matching the old whole-Value estimate.
                let val = serde_json::to_value(spec).unwrap_or(serde_json::Value::Null);
                neenee_core::estimate_semantic_json_tokens(&val).max(0) as usize
            })
            .sum::<usize>();
        let total_tokens = prepared_message_tokens.saturating_add(tool_schema_tokens);

        RequestTokenEstimate {
            history_tokens,
            overhead_tokens: total_tokens.saturating_sub(history_tokens),
            total_tokens,
        }
    }

    /// Estimate the complete next request at the same immutable request
    /// boundary the provider call uses.
    pub fn estimate_next_request_tokens(&self, messages: &[Message]) -> RequestTokenEstimate {
        Self::estimate_model_request(&self.model_request(messages))
    }

    /// Dev-only dry run: rebuild the head system message and auto-load any
    /// skills mentioned in the latest visible user round against a borrowed
    /// message list, exactly as the next turn would, but with no provider call
    /// and no mutation of live round history. Powers
    /// the `/debug preview` so it captures the *real* request shape —
    /// including the freshly composed system prompt and injected skills —
    /// rather than a degenerate reconstruction.
    pub fn prepare_request_messages_debug(&self, messages: &mut Vec<Message>) {
        *messages = self.model_request(messages).messages;
    }

    /// A shared handle to this agent's live variant selection (the **override**
    /// axis). Handed to a spawned envoy's dispatch tool so the envoy — an
    /// agent on the same model — resolves its admitted capabilities to the same
    /// variants the parent uses, tracking model switches live. The profile still
    /// owns the orthogonal **scope** axis.
    pub fn variant_selection_handle(&self) -> Arc<std::sync::Mutex<neenee_core::VariantSelection>> {
        Arc::clone(&self.variant_selection)
    }

    /// Override the opt-in hard-stop budget. Mirrors `[principal] hard_stop_turns`
    /// in `config.toml` but can be flipped at runtime. `0` (the default) leaves
    /// the round uncapped, matching ADR-0009. The reviewer envoy gets a
    /// tight non-zero bound so a runaway diagnostic cannot loop.
    pub fn set_hard_stop_turns(&self, turns: usize) {
        *self
            .hard_stop_turns
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = turns;
    }

    /// Current hard-stop budget. Read by the `/hard-stop` slash command (if
    /// present) and by `check_hard_stop` at each ReAct-turn boundary.
    pub fn get_hard_stop_turns(&self) -> usize {
        *self
            .hard_stop_turns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// The review dimensions effective for this agent: its registered set, or
    /// the built-in defaults ([`crate::default_reviews`]) when none are
    /// registered. Centralizes the "empty → default" fallback so the runner in
    /// `session_review` does not touch private fields.
    pub(crate) fn effective_reviews(&self) -> Vec<Arc<dyn SessionReview>> {
        if self.reviews.is_empty() {
            crate::default_reviews()
        } else {
            self.reviews.to_vec()
        }
    }

    /// Replace the live doom-guard configuration atomically. The next round
    /// reconstructs its per-round guard from the new settings; the current
    /// round, if any, keeps its already-built guard state.
    ///
    /// Wired from `[principal.nudge]` in `config.toml` at startup and forced to
    /// [`neenee_core::DoomGuardConfig::disabled`] on envoys and the review
    /// diagnostic so they run unobstructed regardless of user settings.
    pub fn set_doom_guard_config(&self, config: neenee_core::DoomGuardConfig) {
        *self
            .doom_guard_config
            .write()
            .unwrap_or_else(|e| e.into_inner()) = config;
    }

    /// Snapshot of the live doom-guard configuration. The turn boundary reads
    /// `enabled` to gate the pre-dispatch doom check.
    pub fn doom_guard_config(&self) -> neenee_core::DoomGuardConfig {
        *self
            .doom_guard_config
            .read()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Whether the doom guard is currently armed (allowed to block). Convenience
    /// wrapper over [`Self::doom_guard_config`] for the turn-boundary fast path.
    pub fn doom_guard_enabled(&self) -> bool {
        self.doom_guard_config().enabled
    }

    /// Enable or disable the model-supplied-stdin path for `bash` (L3.5 α).
    /// Mirrors `[principal] allow_model_stdin` in `config.toml`. When off
    /// (the default), the bash schema exposes no `stdin` parameter and a
    /// command needing input either gets it from a human (interactive
    /// classifier → input panel) or fails fast. When on, the model may feed
    /// a command's stdin directly — for unattended/automatic flows.
    pub fn set_allow_model_stdin(&self, enabled: bool) {
        self.allow_model_stdin
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
    }

    /// Replace the command-aware bash safety policy from `[bash_policy]` config.
    /// Built-in dangerous-command rules remain compiled into the policy; config
    /// only supplies toggles and user-defined overrides/additions.
    pub fn set_bash_policy(&self, config: &neenee_persistence::config::BashPolicyConfig) {
        let policy = crate::bash_policy::BashPolicy::from_config(config);
        for error in policy.invalid_rules() {
            tracing::warn!(error = %error, "ignoring invalid bash policy rule");
        }
        *self.bash_policy.write().unwrap_or_else(|e| e.into_inner()) = policy;
    }

    /// Whether the model may supply stdin for a `bash` call. Read at the
    /// dispatch site to decide the [`StdinPolicy`] and whether the bash schema
    /// exposes a `stdin` parameter.
    pub fn allow_model_stdin(&self) -> bool {
        self.allow_model_stdin
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Install (or clear with `None`) the mid-turn model-context projection gate.
    pub fn set_context_projection_gate(&self, gate: Option<Arc<dyn ContextProjectionGate>>) {
        *self
            .context_projection_gate
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = gate;
    }

    /// Install the shared token-source ledger so this agent books each turn's
    /// token counts (reported vs. estimated) into it. The embedding shares the
    /// same `Arc` with the TUI so the token-source report modal reads live.
    /// No-op for envoys/tests that never call this (the ledger stays `None`
    /// and booking is skipped).
    pub fn install_token_ledger(&self, ledger: Arc<neenee_core::TokenSourceLedger>) {
        *self.token_ledger.lock().unwrap_or_else(|e| e.into_inner()) = Some(ledger);
    }

    /// A handle to the token-source ledger, if one was installed. The TUI uses
    /// this to snapshot the report for the modal.
    pub fn token_ledger(&self) -> Option<Arc<neenee_core::TokenSourceLedger>> {
        self.token_ledger
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Book one turn's token usage into [`RoundState::token_usage`] and, when a
    /// ledger is installed, into the token-source ledger.
    ///
    /// `streamed_usage` is the usage reported mid-stream (OpenAI
    /// `include_usage` / Anthropic `message_delta`), if any. When absent, we
    /// fall back to [`Provider::take_last_usage`] (the non-streaming path) and
    /// finally to the local char-class estimator.
    ///
    /// This is the single point that decides whether a turn counts as
    /// **reported** (authoritative) or **estimated** (heuristic), and records
    /// that classification so the token-source report modal can render it.
    fn book_turn_usage(
        &self,
        state: &mut RoundState,
        response: &Message,
        streamed_usage: Option<TokenUsage>,
        request: &mut RequestAccountingGuard,
    ) {
        // Prefer the usage the provider reported (streamed, then drained).
        let reported = streamed_usage.or_else(|| self.provider.take_last_usage());
        if let Some(usage) = reported {
            state.token_usage.total_tokens += usage.total_tokens;
            state.token_usage.prompt_tokens += usage.prompt_tokens;
            state.token_usage.completion_tokens += usage.completion_tokens;
            state.token_usage.cache_creation_input_tokens += usage.cache_creation_input_tokens;
            state.token_usage.cache_read_input_tokens += usage.cache_read_input_tokens;
            request.settle(neenee_core::RequestUsageStatus::Completed, Some(usage), 0);
        } else {
            // Estimate both sides of the request. The old fallback counted
            // only the assistant response while the reported path counted
            // prompt + completion, making mixed-source totals incomparable.
            let completion = pressure::estimate_message_tokens(response).max(0);
            let prompt = request.projected_prompt_tokens.max(0);
            let estimated = prompt.saturating_add(completion);
            state.token_usage.total_tokens += estimated;
            state.token_usage.prompt_tokens += prompt;
            state.token_usage.completion_tokens += completion;
            request.settle(neenee_core::RequestUsageStatus::Completed, None, completion);
        }
    }

    /// Install the lifecycle hook registry (ADR-0025). Replaces any prior
    /// registry; intended to be called once at startup after the `[hooks]`
    /// config is parsed. Envoys and tests leave the default empty registry.
    pub fn set_hooks(&self, registry: crate::hooks::HookRegistry) {
        self.hooks.set(registry);
    }

    /// Install the mid-round save point fired at every ReAct-turn boundary
    /// (ADR-0035). The closure receives the current full round history and
    /// should durably append only the new tail (see
    /// `SessionStore::append_turn`). Called once by orchestration after the
    /// agent is built and the session is open; envoys and the review
    /// diagnostic never call this, so the default `None` keeps their turn
    /// boundaries no-ops.
    pub fn set_turn_persist(&self, f: TurnPersistFn) {
        *self.turn_persist.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
    }

    /// Fire the mid-round save point if installed. Returns `Ok(())` when no
    /// closure is set (the envoy / review / test path) so the call site
    /// stays unconditional. Invoked at the turn boundary — after a turn's
    /// tool results are in `messages` and before the next model request.
    async fn fire_turn_persist(&self, messages: &[Message]) -> Result<(), HarnessError> {
        let f = self
            .turn_persist
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        match f {
            Some(f) => f(messages).await.map_err(|error| {
                HarnessError::Other(format!("could not persist mid-round turn: {error}"))
            }),
            None => Ok(()),
        }
    }

    /// Snapshot the hook registry as a cheap `Arc` clone, so insertion points
    /// fire hooks without holding the swap lock across the async `fire`.
    fn hooks(&self) -> Arc<crate::hooks::HookRegistry> {
        self.hooks.get()
    }

    /// The session id hooks see (the live thread id, if any).
    fn hook_session_id(&self) -> String {
        self.thread_id().unwrap_or_default()
    }

    /// The cwd hooks run under (the persisted project root, if any).
    fn hook_cwd(&self) -> Option<std::path::PathBuf> {
        self.permissions.project_root()
    }

    // --- Public hook entry points (ADR-0025) ---------------------------------
    // The PreToolUse / PostToolUse / Stop insertion points are inline in the
    // loop above (they need local control flow); the lifecycle entry points
    // below are called by the driver / orchestration at the session, turn, and
    // compaction boundaries.

    /// `UserPromptSubmit` gate. Called by `execute_round` before the prompt
    /// enters the transcript: a `Deny` drops it, a `Prepend` prefixes context.
    pub async fn fire_user_prompt_submit(&self, prompt: &str) -> crate::hooks::UserPromptVerdict {
        self.hooks()
            .check_user_prompt_submit(prompt, &self.hook_session_id(), self.hook_cwd().as_deref())
            .await
    }

    /// `PreCompact` observers. Returns any injected context to fold into the
    /// upcoming summarization (ADR-0025).
    pub async fn fire_pre_compact(&self) -> Vec<String> {
        self.hooks()
            .pre_compact(&self.hook_session_id(), self.hook_cwd().as_deref())
            .await
    }

    /// `PostCompact` observers. Informational only.
    pub async fn fire_post_compact(&self) {
        self.hooks()
            .post_compact(&self.hook_session_id(), self.hook_cwd().as_deref())
            .await
    }

    /// `SessionStart` observers; injected context becomes hidden setup messages.
    pub async fn fire_session_start(
        &self,
        source: neenee_core::SessionSource,
        messages: &mut Vec<Message>,
    ) {
        self.hooks()
            .session_start(
                source,
                &self.hook_session_id(),
                self.hook_cwd().as_deref(),
                messages,
            )
            .await
    }

    /// `SessionEnd` observers. Informational only.
    pub async fn fire_session_end(&self) {
        self.hooks()
            .session_end(&self.hook_session_id(), self.hook_cwd().as_deref())
            .await
    }

    /// Between ReAct turns, if context pressure exceeds the configured budget,
    /// hand the live message list to the [`ContextProjectionGate`] so it can
    /// produce and persist the next model-visible window.
    async fn project_context_if_needed(
        &self,
        messages: &mut Vec<Message>,
        cancel: &CancellationToken,
    ) -> Result<(), HarnessError> {
        let budget = *self
            .context_prune_threshold_tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if budget == 0 || self.estimate_next_request_tokens(messages).total_tokens <= budget {
            return Ok(());
        }
        let gate = self
            .context_projection_gate
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(gate) = gate else {
            return Ok(());
        };
        let replacement = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(HarnessError::Interrupted),
            replacement = gate.project_context(messages.clone()) => replacement,
        };
        if let Some(replacement) = replacement
            && !replacement.is_empty()
        {
            *messages = replacement;
        }
        Ok(())
    }

    pub fn set_thread_id(&self, thread_id: impl Into<String>) {
        *self.thread_id.lock().unwrap_or_else(|e| e.into_inner()) = Some(thread_id.into());
    }

    pub fn thread_id_handle(&self) -> Arc<std::sync::Mutex<Option<String>>> {
        Arc::clone(&self.thread_id)
    }

    pub fn round_counter_handle(&self) -> Arc<std::sync::Mutex<u64>> {
        Arc::clone(&self.round_counter)
    }

    pub fn set_accounting_actor_id(&self, actor_id: impl Into<String>) {
        *self
            .accounting_actor_id
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = actor_id.into();
    }

    fn accounting_actor_id(&self) -> String {
        self.accounting_actor_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn clear_thread_id(&self) {
        *self.thread_id.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Current task list snapshot. Read by the harness to mirror into the
    /// session and by the TUI to render the sticky panel.
    pub fn todos(&self) -> neenee_core::TodoList {
        self.todos.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Replace the task list. Used by session-restore paths on resume.
    pub fn set_todos(&self, todos: neenee_core::TodoList) {
        *self.todos.lock().unwrap_or_else(|e| e.into_inner()) = todos;
    }

    /// Drop the task list.
    pub fn clear_todos(&self) {
        *self.todos.lock().unwrap_or_else(|e| e.into_inner()) = neenee_core::TodoList::default();
    }

    /// Current harness round counter — bumped at the start of every
    /// `execute_round`. Used by the TUI to detect a stale task panel (one
    /// whose `updated_at_round` lags the current round by more than
    /// `TODO_STALE_TURN_THRESHOLD`).
    pub fn round_count(&self) -> u64 {
        *self.round_counter.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Advance the round counter. Called once per `execute_round`. The TUI
    /// reads the resulting value to compute "not updated for N rounds".
    pub fn bump_round(&self) {
        let mut g = self.round_counter.lock().unwrap_or_else(|e| e.into_inner());
        *g = g.saturating_add(1);
    }

    /// Restore the round counter to a persisted value on resume (ADR-0048
    /// Phase 2). The counter is session-scoped; without this a resumed
    /// session's todo stale-detector comparisons reset to 0 and go stale
    /// immediately.
    pub fn restore_round_count(&self, count: u64) {
        *self.round_counter.lock().unwrap_or_else(|e| e.into_inner()) = count;
    }

    pub fn get_unattended(&self) -> bool {
        self.permissions.unattended()
    }

    pub fn set_unattended(&self, enabled: bool) {
        self.permissions.set_unattended(enabled);
    }

    /// Set this agent's operation boundary (ADR-0028). The main agent leaves it
    /// unrestricted; `EnvoyTool` sets the scope resolved from the bound
    /// envoy profile on the child before it runs.
    pub fn set_operation_scope(&self, scope: neenee_core::OperationScope) {
        *self
            .operation_scope
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = scope;
    }

    /// Apply a declarative principal profile (ADR-0053) — set every knob a
    /// [`neenee_core::PrincipalProfile`] declares in one call. The
    /// principal-side mirror of how `EnvoyTool` binds an
    /// [`neenee_core::EnvoyProfile`].
    ///
    /// Sets: the capability scope ([`Self::set_agent_selection`]), the
    /// write/command boundary ([`Self::set_operation_scope`]), and the runtime
    /// execution knobs (`hard_stop` / doom guard / model-stdin /
    /// attended flag). The profile's [`neenee_core::AgentIdentity`] is **not**
    /// re-applied here — identity is immutable past construction (it feeds the
    /// system-prompt preamble), so the embedding supplies it to `Agent::new` /
    /// `from_toolset`. A role whose identity should differ per instance composes
    /// [`neenee_core::PrincipalProfile::with_identity`] before construction.
    ///
    /// Idempotent over defaults: a profile built with
    /// [`neenee_core::PrincipalProfile::with_identity`] (no further narrowing)
    /// reproduces the agent constructor's built-in values, so binding it is a
    /// no-op for an already-default agent.
    pub fn apply_principal_profile(&self, profile: &neenee_core::PrincipalProfile) {
        self.set_agent_selection(profile.agent_selection.clone());
        self.set_operation_scope(profile.operation_scope.clone());
        self.set_hard_stop_turns(profile.config.hard_stop_turns);
        self.set_doom_guard_config(profile.config.nudge);
        self.set_allow_model_stdin(profile.config.allow_model_stdin);
        self.set_unattended(profile.unattended);
    }

    /// Snapshot of this agent's operation boundary. Used by the `execute_tool`
    /// funnel to gate tools whose target falls outside the granted scope.
    fn operation_scope(&self) -> neenee_core::OperationScope {
        self.operation_scope
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_else(|_| neenee_core::OperationScope::unrestricted())
    }

    /// The identity this agent was constructed with (name + mission, or a
    /// persona override). Immutable past construction; feeds the system-prompt
    /// preamble. Lets an embedding reuse the primary's identity (e.g. a
    /// `/btw` side session) instead of recomposing it, so the server layer
    /// never hardcodes a product identity.
    pub fn identity(&self) -> &AgentIdentity {
        &self.identity
    }

    pub fn get_pursuit(&self) -> Option<Pursuit> {
        self.pursuit_state.get()
    }

    pub fn set_pursuit(&self, pursuit: Pursuit) {
        self.pursuit_state.set(pursuit);
    }

    pub fn restore_pursuit(&self, pursuit: Pursuit) {
        self.pursuit_state.restore(pursuit);
    }

    pub fn clear_pursuit(&self) {
        self.pursuit_state.clear();
    }

    pub fn pursuit_can_complete(&self) -> bool {
        self.pursuit_state.can_complete()
    }

    // ── Pursuit stop-gate ───────────────────────────────────────────────
    // `/pursue <condition>` arms the gate. Each time the model would end the
    // round, the gate re-injects the condition and forces another turn until
    // the model signals completion, the safety cap is hit, or the pursuit is
    // disarmed. See [`PursuitState::continuation`].

    pub fn arm_pursuit(&self) {
        self.pursuit_state.arm();
    }

    pub fn resume_pursuit(&self) {
        self.pursuit_state.resume();
    }

    pub fn disarm_pursuit(&self) {
        self.pursuit_state.disarm();
    }

    pub fn stop_pursuit(&self, reason: impl Into<String>) -> Option<Pursuit> {
        self.pursuit_state.stop(reason)
    }

    pub fn is_pursuit_armed(&self) -> bool {
        self.pursuit_state.is_armed()
    }

    pub fn pursuit_iterations(&self) -> u32 {
        self.pursuit_state.iterations()
    }

    /// A snapshot of the per-pursuit runtime counters (passes / tokens /
    /// wall-clock), zeroed when the pursuit is armed and accumulated at each
    /// stop-gate boundary (ADR-0083). Used to surface usage in the stop summary
    /// and to enforce [`neenee_core::PursuitBudget`].
    pub fn pursuit_stats(&self) -> PursuitStats {
        self.pursuit_state.stats()
    }

    /// Restore the stop-gate runtime view (armed + iterations) from persisted
    /// state on resume (ADR-0048 Phase 2). Does not reset the iteration
    /// counter — an armed pursuit mid-iteration resumes with its count intact.
    pub fn restore_pursuit_runtime(&self, armed: bool, iterations: u32, stats: PursuitStats) {
        self.pursuit_state.restore_runtime(armed, iterations, stats);
    }

    pub(crate) fn pursuit_continuation(&self, response: &Message) -> Option<String> {
        self.pursuit_state
            .continuation(response, MAX_PURSUIT_ITERATIONS)
    }

    /// The round-end gate (ADR-0025). Combines the `/pursue` stop-gate with any
    /// `Stop` hooks: a pursuit forcing continuation wins; otherwise a `Stop`
    /// hook may force another turn with feedback. Returns `None` to let the
    /// round end — i.e. both the pursuit gate and every Stop hook must agree
    /// to stop. The pursuit gate is queried first so its safety-cap disarm
    /// side effect is preserved.
    ///
    /// Returns the prompt together with the [`InjectionKind`] that produced it,
    /// so the push site stamps the correct provenance (pursuit continuation vs
    /// a `Stop` hook inject) instead of guessing from the text.
    async fn stop_gate(&self, response: &Message) -> Option<(String, InjectionKind)> {
        if let Some(prompt) = self.pursuit_continuation(response) {
            return Some((prompt, InjectionKind::PursuitContinuation));
        }
        self.hooks()
            .check_stop(
                response.content.as_str(),
                &self.hook_session_id(),
                self.hook_cwd().as_deref(),
            )
            .await
            .map(|prompt| (prompt, InjectionKind::Hook(HookEventKind::Stop)))
    }

    /// Book the usage delta since the previous stop-gate boundary into the
    /// active pursuit (ADR-0083). This runs before continuation policy so a
    /// budget reached by the just-finished pass stops immediately.
    fn book_pursuit_pass(&self, state: &mut RoundState, duration_ms: u64) {
        if !self.pursuit_state.is_armed() {
            return;
        }
        let previous = state.pursuit_booked_usage;
        let delta = TokenUsage {
            prompt_tokens: state
                .token_usage
                .prompt_tokens
                .saturating_sub(previous.prompt_tokens),
            completion_tokens: state
                .token_usage
                .completion_tokens
                .saturating_sub(previous.completion_tokens),
            total_tokens: state
                .token_usage
                .total_tokens
                .saturating_sub(previous.total_tokens),
            cache_creation_input_tokens: state
                .token_usage
                .cache_creation_input_tokens
                .saturating_sub(previous.cache_creation_input_tokens),
            cache_read_input_tokens: state
                .token_usage
                .cache_read_input_tokens
                .saturating_sub(previous.cache_read_input_tokens),
        };
        let duration_delta = duration_ms.saturating_sub(state.pursuit_booked_duration_ms);
        self.pursuit_state.book_pass(delta, duration_delta);
        state.pursuit_booked_usage = state.token_usage;
        state.pursuit_booked_duration_ms = duration_ms;
    }

    /// Inject convergence guidance after continuation has been approved and a
    /// configured budget is at least 75% consumed.
    fn inject_pursuit_convergence_reminder(&self, messages: &mut Vec<Message>) {
        let stats = self.pursuit_state.stats();
        // Convergence guidance: once any budget axis crosses 75%, steer the
        // model toward finishing rather than starting new optional work. Fires
        // once per crossing band to avoid repeating the same nudge on later turns.
        if let Some(pursuit) = self.pursuit_state.get()
            && let Some(budget) = pursuit.budget
            && let Some(fraction) =
                budget.usage_fraction(stats.passes, stats.tokens, stats.wall_clock_ms)
            && (0.75..1.0).contains(&fraction)
        {
            crate::conversation_context::inject_reminders(messages, |sink| {
                sink.remind(format!(
                    "Pursuit budget is {:.0}% consumed (passes {}, tokens {}, {:.0}s). \
                     Converge on the objective: finish in-flight work, verify it, and \
                     emit {marker} rather than starting new optional work.",
                    fraction * 100.0,
                    stats.passes,
                    stats.tokens,
                    stats.wall_clock_ms as f64 / 1000.0,
                    marker = crate::PURSUIT_COMPLETE_MARKER,
                ));
            });
        }
    }

    pub fn thread_id(&self) -> Option<String> {
        self.thread_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn inject_pursuit_continuation(&self, messages: &mut Vec<Message>) {
        self.pursuit_state.inject_continuation(messages);
    }

    pub fn inject_objective_updated(&self, messages: &mut Vec<Message>) {
        self.pursuit_state.inject_objective_updated(messages);
    }

    pub fn reply_permission(&self, request_id: &str, decision: PermissionDecision) -> bool {
        self.permissions.reply(request_id, decision)
    }

    pub fn reject_pending_permissions(&self) {
        self.permissions.reject_pending();
    }

    /// Resolve a parked `ask_user` request. An empty outer vector means the
    /// operator cancelled; answered questions remain distinguishable because
    /// they carry one inner vector per question (which may itself be empty).
    pub fn reply_user_question(&self, request_id: &str, answers: Vec<Vec<String>>) -> bool {
        let sender = self
            .ask_user
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .remove(request_id);
        sender.is_some_and(|sender| {
            let reply = if answers.is_empty() {
                None
            } else {
                Some(UserQuestionReply {
                    request_id: request_id.to_string(),
                    answers,
                })
            };
            sender.send(reply).is_ok()
        })
    }

    pub fn reject_pending_user_questions(&self) {
        let pending = std::mem::take(
            &mut self
                .ask_user
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pending,
        );
        for (_, sender) in pending {
            let _ = sender.send(None);
        }
    }

    /// Resolve a parked interactive-input request (L3.5 β) with the operator's
    /// text, unblocking the `bash` dispatch that issued it. Returns `false` if
    /// no matching request is parked (e.g. already resolved or cancelled).
    /// An empty `text` is a valid "cancel" — the command then runs with
    /// closed stdin and fails fast.
    pub fn reply_input(&self, request_id: &str, text: String) -> bool {
        let sender = self
            .input
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .remove(request_id);
        sender.is_some_and(|sender| {
            sender
                .send(Some(InputReply {
                    request_id: request_id.to_string(),
                    text,
                }))
                .is_ok()
        })
    }

    /// Cancel every parked input request (e.g. on round end / interrupt),
    /// resolving each with `None` so the awaiting dispatch returns a
    /// cancelled result.
    pub fn reject_pending_inputs(&self) {
        let pending =
            std::mem::take(&mut self.input.lock().unwrap_or_else(|e| e.into_inner()).pending);
        for (_, sender) in pending {
            let _ = sender.send(None);
        }
    }

    pub fn allowed_tools(&self) -> Vec<String> {
        self.permissions.allowed_tools()
    }

    pub fn clear_allowed_tools(&self) {
        self.permissions.clear_allowed();
    }

    /// Revoke a single cached "always allow" rule. Returns whether a rule was
    /// actually removed (false if the rule was never cached). Powers the
    /// session modal's per-row revoke.
    pub fn revoke_allowed_tool(&self, tool: &str, scope: &str) -> bool {
        self.permissions.revoke_allowed(tool, scope)
    }

    /// Install (or reuse) the steering inbox and return a [`EnvoyHandle`]
    /// the caller can steer the agent with mid-turn — the entry point of
    /// full-duplex (ADR-0029). Requires `Arc<Self>` because the handle holds a
    /// `Weak<Agent>` so it can observe the agent's lifetime without keeping it
    /// alive after its dispatcher ends the round.
    ///
    /// Idempotent: the first call creates the `mpsc` pair (sender stored on the
    /// agent so [`Agent::submit`] works too, receiver left for the driver to
    /// `take`); later calls reuse the same pair. The top-level agent driven
    /// directly by the harness never calls this and stays non-steerable by an
    /// inbox — its interrupt path is the `CancellationToken` passed to the run,
    /// and its permission/ask_user replies go through the harness directly.
    pub fn install_inbox(self: &Arc<Self>) -> EnvoyHandle {
        let mut tx_guard = self.inbox_tx.lock().unwrap_or_else(|e| e.into_inner());
        let tx = match tx_guard.clone() {
            Some(existing) => existing,
            None => {
                let (tx, rx) = mpsc::unbounded_channel();
                *tx_guard = Some(tx.clone());
                drop(tx_guard);
                *self.inbox_rx.lock().unwrap_or_else(|e| e.into_inner()) = Some(rx);
                tx
            }
        };
        EnvoyHandle {
            weak: Arc::downgrade(self),
            ops: tx,
        }
    }

    /// Submit a steering [`AgentOp`] without going through a handle. Equivalent
    /// to [`EnvoyHandle::submit`] but usable when the caller already holds a
    /// reference to the agent rather than a handle (e.g. the top-level harness
    /// steering the primary session). Returns `false` if no inbox was ever
    /// installed ([`Agent::install_inbox`] was not called) or the receiver was
    /// dropped.
    pub fn submit(&self, op: AgentOp) -> bool {
        self.inbox_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|tx| tx.send(op).is_ok())
    }

    /// Open a fresh, cancellable user-input queue for one interactive round.
    /// Any stale entries are returned to the caller so it can surface them as
    /// unavailable instead of silently carrying them into a different round.
    pub fn begin_user_input_round(
        &self,
        session_id: impl Into<String>,
        generation: u64,
    ) -> Vec<neenee_core::QueuedUserInput> {
        self.user_input_queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(UserInputRound {
                session_id: session_id.into(),
                generation,
                queue: std::collections::VecDeque::new(),
            })
            .map(|round| round.queue)
            .unwrap_or_default()
            .into_iter()
            .collect()
    }

    /// Queue human-authored input for the next safe turn boundary. Returns
    /// `false` once the round has atomically closed its admission gate.
    pub fn submit_user_input(&self, session_id: &str, input: neenee_core::QueuedUserInput) -> bool {
        let mut queue = self
            .user_input_queue
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(round) = queue
            .as_mut()
            .filter(|round| round.session_id == session_id)
        else {
            return false;
        };
        round.queue.push_back(input);
        true
    }

    /// Cancel a queued insert. Taking the same mutex as boundary admission
    /// makes the result definitive: `Some` means the input cannot be admitted;
    /// `None` means admission already won (or the id was unknown).
    pub fn cancel_user_input(
        &self,
        session_id: &str,
        input_id: &str,
    ) -> Option<neenee_core::QueuedUserInput> {
        let mut queue = self
            .user_input_queue
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let round = queue
            .as_mut()
            .filter(|round| round.session_id == session_id)?;
        let position = round.queue.iter().position(|input| input.id == input_id)?;
        round.queue.remove(position)
    }

    /// Stop accepting inserts and return anything that never crossed a turn
    /// boundary. Used on interrupted/error/blocked terminal paths.
    pub fn close_user_input_round(&self, generation: u64) -> Vec<neenee_core::QueuedUserInput> {
        let mut queue = self
            .user_input_queue
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !queue
            .as_ref()
            .is_some_and(|round| round.generation == generation)
        {
            return Vec::new();
        }
        queue
            .take()
            .map(|round| round.queue)
            .unwrap_or_default()
            .into_iter()
            .collect()
    }

    fn user_input_generation(&self) -> Option<u64> {
        self.user_input_queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|round| round.generation)
    }

    /// Admit every currently queued human input. When `close_if_empty` is
    /// true, observing an empty queue also closes the round atomically so a
    /// concurrent submit must fail and can be promoted to a next-round item.
    fn admit_user_inputs<F>(
        &self,
        generation: Option<u64>,
        messages: &mut Vec<Message>,
        close_if_empty: bool,
        on_event: &mut F,
    ) -> usize
    where
        F: FnMut(AgentEvent),
    {
        let inputs = {
            let mut queue = self
                .user_input_queue
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let Some(open) = queue
                .as_mut()
                .filter(|round| Some(round.generation) == generation)
            else {
                return 0;
            };
            if open.queue.is_empty() {
                if close_if_empty {
                    *queue = None;
                }
                return 0;
            }
            open.queue.drain(..).collect::<Vec<_>>()
        };

        let admitted = inputs.len();
        for input in inputs {
            let mut message = crate::conversation_context::visible_user(
                InjectionKind::UserSteer,
                input.text.clone(),
            );
            if let Some(display) = input.display_text.clone() {
                message = message.with_display_content(display);
            }
            if let Some(sent_at_ms) = input.sent_at_ms {
                message = message.with_sent_at_ms(sent_at_ms);
            }
            if !input.images.is_empty() {
                message = message.with_images(input.images.clone());
            }
            messages.push(message);
            on_event(AgentEvent::UserInputInserted(input));
        }
        admitted
    }

    /// Drain every op currently buffered in the inbox and apply it to the live
    /// round. Called by the driver at the top of every ReAct turn (the only
    /// place it is safe to mutate `messages` or end the round).
    ///
    /// Returns `false` when an `Interrupt` / `Shutdown` was observed — the
    /// caller then returns [`HarnessError::Interrupted`] (`Shutdown` is the
    /// same flow today; a future graceful variant would distinguish them).
    /// `None` for `rx` (no inbox installed) is a no-op that returns `true`, so
    /// non-steerable agents pay nothing.
    fn drain_inbox(
        &self,
        rx: &mut Option<mpsc::UnboundedReceiver<AgentOp>>,
        messages: &mut Vec<Message>,
    ) -> bool {
        let Some(rx) = rx.as_mut() else {
            return true;
        };
        let mut interrupted = false;
        while let Ok(op) = rx.try_recv() {
            match op {
                AgentOp::InjectUserMessage(text) => {
                    messages.push(crate::conversation_context::visible_user(
                        InjectionKind::EnvoySteer,
                        text,
                    ));
                }
                AgentOp::InterAgentMessage { msg } => {
                    messages.push(crate::conversation_context::hidden_user(
                        InjectionKind::InterAgent,
                        msg,
                    ));
                }
                AgentOp::Interrupt | AgentOp::Shutdown => {
                    interrupted = true;
                }
            }
        }
        !interrupted
    }

    /// Structured view of the cached "always allow" rules, for the session
    /// modal's Permissions pane. Unlike [`Agent::allowed_tools`] (which collapses
    /// each rule to a single formatted string), this keeps the tool/scope pair
    /// intact so the modal can target an individual rule for revocation.
    pub fn allowed_tools_structured(&self) -> Vec<neenee_core::PermissionRuleInfo> {
        self.permissions.allowed_tools_structured()
    }

    /// Designate the project whose bucket backs the persistent "always"
    /// allowlist, and load any rules already on disk into the in-memory set.
    /// Pass `None` to disable persistence (envoys and most tests do this).
    ///
    /// Loading is best-effort: a missing, unreadable, or unsupported file is
    /// silently ignored — the agent simply starts with an empty allowlist and
    /// re-prompts the user. This is the cross-session hook: a fresh session in
    /// the same project inherits prior `Always` approvals without re-asking.
    pub fn set_project_root(&self, root: Option<std::path::PathBuf>) {
        self.permissions.set_project_root(root);
    }

    /// Seed declarative permission rules from `[permissions]` config. Delegates
    /// to `PermissionStore::seed_from_config`.
    pub fn seed_permissions_from_config(
        &self,
        rules: &[neenee_persistence::config::PermissionRuleConfig],
    ) {
        self.permissions.seed_from_config(rules);
    }

    /// Replace the complete tool snapshot published by one dynamic source.
    pub fn replace_dynamic_tools(&self, source: &str, tools: Vec<Arc<dyn Tool>>) {
        self.dynamic_tools.replace(source, tools);
    }

    /// Remove one dynamic source and every tool it published.
    pub fn remove_dynamic_tools(&self, source: &str) {
        self.dynamic_tools.remove(source);
    }

    /// The connector-facing publication port. It deliberately exposes no
    /// agent-owned lock or protocol-specific state.
    pub fn dynamic_tool_sink(&self) -> Arc<dyn neenee_core::DynamicToolSink> {
        self.dynamic_tools.clone()
    }

    /// Set the session-level enabled flag for a tool. No-op when the name is
    /// unknown (so a stale toggle from the modal cannot poison the dispatch
    /// table). Returns whether the flag actually changed.
    pub fn set_tool_enabled(&self, name: &str, enabled: bool) -> bool {
        let known = self.toolset.variants_of(name).is_some() || self.dynamic_tools.contains(name);
        if !known {
            return false;
        }
        let mut guard = self
            .disabled_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let currently_enabled = !guard.contains(name);
        if enabled == currently_enabled {
            return false;
        }
        if enabled {
            guard.remove(name);
        } else {
            guard.insert(name.to_string());
        }
        true
    }

    /// Whether `name` is currently enabled (i.e. visible to the model and
    /// dispatchable). Unknown tools report `false`.
    pub fn is_tool_enabled(&self, name: &str) -> bool {
        if self.toolset.variants_of(name).is_none() && !self.dynamic_tools.contains(name) {
            return false;
        }
        let guard = self
            .disabled_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        !guard.contains(name)
    }

    /// Whether `name` is hidden from the model by *either* mask: the persisted
    /// session-level mask (user `/tools` toggles) or the in-memory hook-scoped
    /// mask ([`HookOutcome::ScopeTools`]). This is the model-facing truth —
    /// `visible_tools` and the dispatch guard both consult it. The pub
    /// [`Self::is_tool_enabled`] deliberately reports only the user mask so the
    /// UI's Tools modal is not confused by transient hook scoping.
    fn is_name_disabled(&self, name: &str) -> bool {
        let user = self
            .disabled_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if user.contains(name) {
            return true;
        }
        let scoped = self
            .scoped_disabled_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        scoped.contains(name)
    }

    /// Apply hook-fired [`HookOutcome::ScopeTools`] disables: record each name
    /// (only known tools, matching the user-mask contract) under its restore
    /// point. Idempotent across repeated fires via refcounting.
    fn apply_scoped_disables(&self, disables: &[(String, neenee_core::RestorePoint)]) {
        if disables.is_empty() {
            return;
        }
        let mut scoped = self
            .scoped_disabled_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for (name, restore) in disables {
            // Only known tools: a stale/typo'd name from a hook cannot poison
            // the mask (mirrors `set_tool_enabled`'s known-tool guard).
            if self.toolset.variants_of(name).is_some() {
                scoped.disable(name, *restore);
            }
        }
    }

    /// Restore every `TurnEnd` disable at the ReAct-turn boundary.
    pub(crate) fn restore_scoped_turn_end(&self) {
        let mut scoped = self
            .scoped_disabled_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        scoped.restore_turn_end();
    }

    /// Restore every scoped disable (both buckets) at user-round end.
    pub(crate) fn restore_scoped_round_end(&self) {
        let mut scoped = self
            .scoped_disabled_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        scoped.restore_round_end();
    }

    /// Restore the disabled-tool mask from a persisted set on resume
    /// (ADR-0048 Phase 2). Replaces the in-memory mask wholesale so a user
    /// toggle survives restart. Only known tool names are retained so a stale
    /// toggle (e.g. a tool removed from config) cannot poison the dispatch
    /// table.
    pub fn restore_disabled_tools(&self, tools: std::collections::HashSet<String>) {
        let mut guard = self
            .disabled_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.clear();
        for name in tools {
            if self.toolset.variants_of(&name).is_some() {
                guard.insert(name);
            }
        }
    }

    /// Snapshot the disabled-tool mask for persistence (ADR-0048 Phase 2).
    pub fn disabled_tools_snapshot(&self) -> std::collections::HashSet<String> {
        self.disabled_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// All installed tools that the model may see this turn: every tool whose
    /// name is not disabled by *either* mask (the persisted user mask or the
    /// in-memory hook-scoped mask). Used at the schema-build choke points so a
    /// disabled tool's definition never reaches the provider.
    pub(crate) fn visible_tools(&self) -> Vec<Arc<dyn Tool>> {
        // `ask_user` is reclaimed under unattended: no human is reachable to
        // answer, so admitting it would only deadlock a round. Drop its schema
        // so the model never names it. The dispatch guard also short-circuits
        // any stale call (a name carried over from an earlier turn's tool
        // list) — see `execute_tool`.
        let reclaim_ask_user = self.get_unattended();
        self.installed_tools()
            .into_iter()
            .filter(|t| !self.is_name_disabled(t.name()))
            .filter(|t| !(reclaim_ask_user && t.name() == "ask_user"))
            .collect()
    }

    /// Structured view of every installed tool, for the session modal's Tools
    /// pane. `enabled` reflects the disabled mask; `source` classifies origin
    /// (`builtin`, `envoy`, or the publisher-provided dynamic source id).
    pub fn snapshot_tools(&self) -> Vec<neenee_core::ToolInfo> {
        // Classification mirrors installed_tools()'s three buckets, with the
        // extra UI affordance that `envoy` is labeled distinctly from other
        // builtins. The source label is display-only; dispatch treats all
        // three buckets uniformly via name-clash priority (builtin > user > mcp).
        let disabled = self
            .disabled_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let envoy = ["envoy"];

        let mut seen: HashSet<String> = HashSet::new();
        let mut sourced_tools: Vec<(String, Arc<dyn Tool>)> = Vec::new();

        // 1. builtin (resolved static), with envoy broken out for display.
        for tool in self
            .resolved_tools
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
        {
            if seen.insert(tool.name().to_string()) {
                let source = if envoy.contains(&tool.name()) {
                    "envoy".to_string()
                } else {
                    "builtin".to_string()
                };
                sourced_tools.push((source, tool));
            }
        }

        // 2. user (SDK/RPC-injected).
        for tool in self
            .user_tools
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
        {
            if seen.insert(tool.name().to_string()) {
                sourced_tools.push(("user".to_string(), tool));
            }
        }

        // 3. mcp (dynamic snapshot).
        for entry in self.dynamic_tools.snapshot() {
            if seen.insert(entry.tool.name().to_string()) {
                sourced_tools.push((entry.source, entry.tool));
            }
        }

        let mut infos: Vec<neenee_core::ToolInfo> = sourced_tools
            .into_iter()
            .map(|(source, tool)| {
                let name = tool.name();
                neenee_core::ToolInfo {
                    name: name.to_string(),
                    description: tool.description().to_string(),
                    enabled: !disabled.contains(name),
                    source,
                }
            })
            .collect();
        infos.sort_by(|a, b| a.source.cmp(&b.source).then_with(|| a.name.cmp(&b.name)));
        infos
    }

    /// Structured view of the skills registry, for the session modal's Skills
    /// pane. Mirrors [`skills::RegistryGuard::list`] into the render-friendly
    /// DTO.
    pub fn snapshot_skills(&self) -> Vec<neenee_core::SkillInfo> {
        let guard = self.skills_registry.lock();
        guard
            .list()
            .into_iter()
            .map(|skill| neenee_core::SkillInfo {
                name: skill.name.clone(),
                description: skill.description.clone(),
                version: skill.version.clone(),
                enabled: skill.enabled,
                source: skill.scope.to_string(),
                tags: skill.tags.clone(),
            })
            .collect()
    }

    pub async fn run(&self, messages: &mut Vec<Message>) -> Result<RoundOutcome, HarnessError> {
        // Non-interactive convenience path: not cancellable from the outside.
        self.run_with_events(messages, &CancellationToken::new(), |event| match event {
            AgentEvent::PermissionRequest(request) => {
                self.reply_permission(&request.id, PermissionDecision::Reject);
            }
            AgentEvent::UserQuestionRequest(request) => {
                self.reply_user_question(&request.id, Vec::new());
            }
            _ => {}
        })
        .await
    }

    #[tracing::instrument(skip_all, name = "round", fields(streaming = false))]
    pub async fn run_with_events<F>(
        &self,
        messages: &mut Vec<Message>,
        cancel: &CancellationToken,
        mut on_event: F,
    ) -> Result<RoundOutcome, HarnessError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        let round_started_at = std::time::Instant::now();
        let mut state = RoundState {
            guards: RoundState::guards_default(self.doom_guard_config()),
            ..RoundState::default()
        };
        let mut turn_index = 0;
        // Take the steering inbox receiver for this round (ADR-0029). `None` for
        // a non-steerable agent (no `install_inbox` call) → `drain_inbox` is a
        // no-op. Taken once per agent: a re-run after the first returns `None`
        // too, which is fine for the top-level harness (driven directly) and
        // for envoys (single run).
        let mut inbox_rx = self
            .inbox_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let user_input_generation = self.user_input_generation();

        loop {
            if cancel.is_cancelled() {
                return Err(HarnessError::Interrupted);
            }
            // Apply any steering ops queued since the last turn (inject a
            // message, or abort via Interrupt/Shutdown) before requesting the
            // next completion. Replies (permission/ask_user) do NOT flow here
            // — they resolve the parked oneshot directly.
            if !self.drain_inbox(&mut inbox_rx, messages) {
                return Err(HarnessError::Interrupted);
            }
            if self.admit_user_inputs(user_input_generation, messages, false, &mut on_event) > 0 {
                // User input is an admission boundary just like a tool result:
                // persist it before the provider can observe it.
                self.fire_turn_persist(messages).await?;
            }

            crate::conversation_context::inject_mentioned_skills(&self.skills_registry, messages);
            // TurnStart hooks (symmetric to the turn-end Turn hooks): inject
            // any context at the top of this turn's attention, before the
            // model is asked for its next completion. No-op without a
            // `[hooks]` config (envoys, tests).
            self.run_turn_start_hooks(messages, &state, turn_index)
                .await;
            let request = self.model_request(messages);
            let request_projection = Self::estimate_model_request(&request).total_tokens;
            let request_provider = self.provider.provider_id();
            let request_model = self.provider.model();
            let mut request_accounting = RequestAccountingGuard::begin(
                self,
                cancel,
                &request_provider,
                &request_model,
                turn_index,
                request_projection,
            );
            on_event(AgentEvent::ModelRequestStarted {
                round: self.round_count(),
                turn: turn_index,
                context_tokens: request_projection,
            });

            let response = match tokio::time::timeout(
                CHAT_RESPONSE_TIMEOUT,
                self.provider.chat(request),
            )
            .await
            {
                Ok(result) => result?,
                Err(_elapsed) => {
                    tracing::warn!(
                        timeout_secs = CHAT_RESPONSE_TIMEOUT.as_secs(),
                        "non-streaming chat request timed out"
                    );
                    return Err(HarnessError::Retryable {
                        message: format!(
                            "Provider did not respond within {} seconds.",
                            CHAT_RESPONSE_TIMEOUT.as_secs()
                        ),
                        retry_after_ms: None,
                    });
                }
            };
            if !valid_assistant_response(&response) {
                return Err(empty_response_error(&response));
            }
            self.book_turn_usage(&mut state, &response, None, &mut request_accounting);
            messages.push(response.clone());

            // The model produced no text stream, so nothing was shown to the UI
            // that a fallback tool call would need to retract.
            if self
                .dispatch_tool_calls(
                    &response,
                    messages,
                    &mut state,
                    false,
                    cancel,
                    &mut on_event,
                )
                .await?
            {
                turn_index += 1;
                if self.check_hard_stop(turn_index).is_break() {
                    return Err(self.hard_stop_error());
                }
                self.project_context_if_needed(messages, cancel).await?;
                // Mid-turn save point (ADR-0035): see the streaming path.
                self.fire_turn_persist(messages).await?;
                self.run_turn_hooks(messages, &state, turn_index).await;
                // Restore TurnEnd disables now that the ReAct turn is over, so
                // tools narrowed for one turn come back for the next.
                // RoundEnd disables survive until the user round
                // ends (see the return path below).
                self.restore_scoped_turn_end();
                continue;
            }

            // Turn-exit gates. Pursuit may force another turn, and a human
            // insert queued during the just-finished provider request does the
            // same. When neither applies, `close_if_empty` atomically closes
            // admission before this round returns.
            let duration_ms = round_started_at.elapsed().as_millis() as u64;
            self.book_pursuit_pass(&mut state, duration_ms);
            let mut continue_round = false;
            if let Some((prompt, kind)) = self.stop_gate(&response).await {
                self.pursuit_state.bump_iterations();
                self.inject_pursuit_convergence_reminder(messages);
                messages.push(crate::conversation_context::hidden_user(kind, prompt));
                continue_round = true;
            }
            let admitted = self.admit_user_inputs(
                user_input_generation,
                messages,
                !continue_round,
                &mut on_event,
            );
            if admitted > 0 {
                continue_round = true;
            }
            if continue_round {
                turn_index += 1;
                if self.check_hard_stop(turn_index).is_break() {
                    return Err(self.hard_stop_error());
                }
                self.fire_turn_persist(messages).await?;
                self.run_turn_hooks(messages, &state, turn_index).await;
                self.restore_scoped_turn_end();
                continue;
            }

            // User-round end: clear every scoped disable so the toolset is
            // fresh for the next user request.
            self.restore_scoped_round_end();
            return Ok(RoundOutcome {
                message: response,
                token_usage: state.token_usage,
                duration_ms,
            });
        }
    }

    #[tracing::instrument(skip_all, name = "round", fields(streaming = true))]
    pub async fn run_streaming_with_events<F>(
        &self,
        messages: &mut Vec<Message>,
        cancel: &CancellationToken,
        on_event: F,
    ) -> Result<RoundOutcome, HarnessError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        let mut round = self.begin_streaming_round();
        self.resume_streaming_with_events(messages, cancel, &mut round, on_event)
            .await
    }

    /// Start the durable in-memory state for one streaming user round.
    ///
    /// The top-level orchestrator retains this across transient provider
    /// failures so it can retry the failed request without re-entering the
    /// round from scratch. Standalone callers use [`Self::run_streaming_with_events`],
    /// which creates and consumes the state in one call.
    pub(crate) fn begin_streaming_round(&self) -> StreamingRoundState {
        StreamingRoundState {
            state: RoundState {
                guards: RoundState::guards_default(self.doom_guard_config()),
                ..RoundState::default()
            },
            turn_index: 0,
            inbox_rx: self
                .inbox_rx
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take(),
            started_at: std::time::Instant::now(),
            pending_request: None,
            user_input_generation: self.user_input_generation(),
        }
    }

    /// Run or resume a streaming round from its last provider-request boundary.
    ///
    /// A [`HarnessError::Retryable`] leaves `round` reusable. If the failed
    /// request followed completed tool calls, their messages and the complete
    /// per-round state are already present, so the next invocation sends the
    /// same pending provider request instead of executing those tools again.
    pub(crate) async fn resume_streaming_with_events<F>(
        &self,
        messages: &mut Vec<Message>,
        cancel: &CancellationToken,
        round: &mut StreamingRoundState,
        mut on_event: F,
    ) -> Result<RoundOutcome, HarnessError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        loop {
            if cancel.is_cancelled() {
                return Err(HarnessError::Interrupted);
            }

            let resuming_provider_request = round.pending_request.is_some();
            if resuming_provider_request {
                round.state.protect_completed_tools_for_retry();
            }
            if round.pending_request.is_none() {
                // Apply steering ops queued since the last turn before
                // preparing a new provider request. Replies bypass this (see
                // `drain_inbox`). A transient retry deliberately skips this
                // block: the already-prepared request is the checkpoint.
                if !self.drain_inbox(&mut round.inbox_rx, messages) {
                    return Err(HarnessError::Interrupted);
                }
                if self.admit_user_inputs(
                    round.user_input_generation,
                    messages,
                    false,
                    &mut on_event,
                ) > 0
                {
                    self.fire_turn_persist(messages).await?;
                }

                crate::conversation_context::inject_mentioned_skills(
                    &self.skills_registry,
                    messages,
                );
                // TurnStart hooks belong to a logical ReAct turn, not to
                // each network attempt. Run them once before checkpointing the
                // request so retries cannot duplicate injected context or hook
                // side effects.
                self.run_turn_start_hooks(messages, &round.state, round.turn_index)
                    .await;
                round.pending_request = Some(self.model_request(messages));
            }
            tracing::debug!(
                turn = round.turn_index,
                resumed = resuming_provider_request,
                "requesting model completion"
            );
            let Some(request) = round.pending_request.as_ref() else {
                return Err(HarnessError::from(
                    "internal error: provider request was not assembled".to_string(),
                ));
            };
            let request_projection = Self::estimate_model_request(request).total_tokens;
            let request_provider = self.provider.provider_id();
            let request_model = self.provider.model();
            let mut request_accounting = RequestAccountingGuard::begin(
                self,
                cancel,
                &request_provider,
                &request_model,
                round.turn_index,
                request_projection,
            );
            on_event(AgentEvent::ModelRequestStarted {
                round: self.round_count(),
                turn: round.turn_index,
                context_tokens: request_projection,
            });
            // Race the model request against cancellation so an interrupt
            // while we're waiting on the network resolves promptly instead of
            // blocking until the first stream chunk arrives. The idle-timeout
            // arm covers a provider endpoint that accepts the connection but
            // never sends HTTP response headers (overloaded upstream, dropped
            // proxy) — without it the select would hang forever on `.send()`.
            let mut stream = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(HarnessError::Interrupted),
                result = tokio::time::timeout(
                    STREAM_IDLE_TIMEOUT,
                    self.provider.stream_chat_events(request.clone()),
                ) => match result {
                    Ok(Ok(stream)) => stream,
                    Ok(Err(error)) => return Err(HarnessError::from(error)),
                    Err(_elapsed) => {
                        tracing::warn!(
                            timeout_secs = STREAM_IDLE_TIMEOUT.as_secs(),
                            "stream request timed out before any response"
                        );
                        return Err(HarnessError::Retryable {
                            message: format!(
                                "Provider did not start streaming within {} seconds.",
                                STREAM_IDLE_TIMEOUT.as_secs()
                            ),
                            retry_after_ms: None,
                        });
                    }
                },
            };
            let mut content = String::new();
            let mut reasoning_content = String::new();
            let mut calls: Vec<ToolCall> = Vec::new();
            let mut emitted_text = false;
            let mut emitted_reasoning = false;
            // Token usage reported mid-stream via a `Usage` event (OpenAI
            // `include_usage`, Anthropic `message_delta`). Captured here and
            // preferred over the local estimate when booking the turn.
            let mut streamed_usage: Option<TokenUsage> = None;

            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(HarnessError::Interrupted),
                    // Guard against a stalled SSE stream: providers use
                    // `reqwest::Client::new()` with no read timeout, so without
                    // this bound a connection that stays open but stops sending
                    // (common with overloaded reasoning-model endpoints) blocks
                    // the turn forever. The idle clock resets on every chunk,
                    // so a legitimately slow reasoning model that keeps
                    // trickling deltas is never cut off.
                    event = tokio::time::timeout(STREAM_IDLE_TIMEOUT, stream.next()) => {
                        let event = match event {
                            Ok(Some(event)) => event,
                            Ok(None) => break,
                            Err(_elapsed) => {
                                tracing::warn!(
                                    idle_timeout_secs = STREAM_IDLE_TIMEOUT.as_secs(),
                                    "stream stalled: no data received within idle timeout"
                                );
                                return Err(HarnessError::Retryable {
                                    message: format!(
                                        "Provider stream stalled — no data received \
                                         for {} seconds.",
                                        STREAM_IDLE_TIMEOUT.as_secs()
                                    ),
                                    retry_after_ms: None,
                                });
                            }
                        };
                        match event? {
                            ProviderStreamEvent::TextDelta(delta) => {
                                request_accounting.observe_output(&delta);
                                content.push_str(&delta);
                                on_event(AgentEvent::AssistantDelta {
                                    delta,
                                    start: !emitted_text,
                                });
                                emitted_text = true;
                            }
                            ProviderStreamEvent::ReasoningDelta(delta) => {
                                request_accounting.observe_output(&delta);
                                reasoning_content.push_str(&delta);
                                on_event(AgentEvent::ReasoningDelta {
                                    delta,
                                    start: !emitted_reasoning,
                                });
                                emitted_reasoning = true;
                            }
                            ProviderStreamEvent::ToolCallDelta {
                                index,
                                id,
                                name,
                                arguments,
                            } => {
                                while calls.len() <= index {
                                    calls.push(ToolCall {
                                        id: String::new(),
                                        name: String::new(),
                                        arguments: String::new(),
                                    });
                                }
                                let call = &mut calls[index];
                                if let Some(id) = id {
                                    call.id.push_str(&id);
                                }
                                if let Some(name) = name {
                                    request_accounting.observe_output(&name);
                                    call.name.push_str(&name);
                                }
                                request_accounting.observe_output(&arguments);
                                call.arguments.push_str(&arguments);
                            }
                            ProviderStreamEvent::Usage(usage) => {
                                // Take the last reported usage (providers may
                                // emit one final usage chunk). Prefer it over
                                // the local estimate at booking time.
                                request_accounting.observe_usage(usage);
                                streamed_usage = Some(usage);
                            }
                        }
                    }
                }
            }
            // Strict stream finalization (the same discipline praxion's
            // accumulator enforces at `finish()`): the stream must not end
            // mid-tool-call. A slot that accumulated id/argument bytes but
            // never received a name is the residue of a truncated stream —
            // dropping it silently would mistake a connection failure for
            // the model's intent. Surface it as retryable so the idempotent
            // request retry re-runs the turn instead of committing a partial
            // response. Slots that stayed completely empty (a provider delta
            // that carried only an index) are still dropped below.
            for call in &calls {
                if call.name.is_empty() && (!call.id.is_empty() || !call.arguments.is_empty()) {
                    return Err(HarnessError::Retryable {
                        message: "Provider stream ended mid-tool-call; the response \
                                  was likely truncated."
                            .to_string(),
                        retry_after_ms: None,
                    });
                }
            }
            if emitted_text {
                on_event(AgentEvent::AssistantEnd(content.clone()));
            }
            if emitted_reasoning {
                on_event(AgentEvent::ReasoningEnd(reasoning_content.clone()));
            }

            calls.retain(|call| !call.name.is_empty());
            for call in &mut calls {
                // Arguments that streamed but do not parse as JSON mean the
                // connection died mid-payload; fail retryable here instead of
                // executing half a call and surfacing the parse error as if
                // the model had emitted bad JSON. (`arguments == ""` is the
                // legitimate shape of a zero-argument tool and must not trip
                // this check.)
                if !call.arguments.is_empty()
                    && serde_json::from_str::<serde_json::Value>(&call.arguments).is_err()
                {
                    return Err(HarnessError::Retryable {
                        message: format!(
                            "Provider stream ended with truncated arguments for tool \
                             call `{}`; the response was likely cut off.",
                            call.name
                        ),
                        retry_after_ms: None,
                    });
                }
                if call.id.is_empty() {
                    call.id = format!("call_{}", uuid::Uuid::new_v4());
                }
            }
            let response = Message {
                role: Role::Assistant,
                content,
                content_blob: None,
                display_content: None,
                reasoning_content: (!reasoning_content.is_empty()).then_some(reasoning_content),
                tool_calls: (!calls.is_empty()).then_some(calls),
                tool_call_id: None,
                images: None,
                // Stamp which provider/model produced this turn so a session
                // that mixes models stays traceable after resume. The proxy
                // provider delegates to whichever concrete provider is active.
                provider: Some(request_provider),
                model: Some(request_model),
                // Drain any provider-opaque wire credential the turn
                // accumulated (e.g. the Anthropic thinking signature) so the
                // next replay re-emits it verbatim. None for providers that
                // carry none; the map is opaque to this layer.
                provider_meta: self.provider.take_last_provider_meta(),
                hidden: false,
                children: None,
                envoy_meta: None,
                origin: None,
                timestamp: Some(neenee_core::todos::unix_now()),
                sent_at_ms: None,
            };
            if !valid_assistant_response(&response) {
                return Err(empty_response_error(&response));
            }
            // The request checkpoint is consumed only after a complete,
            // valid response is available. Any earlier return leaves it set
            // so orchestration can retry this exact request.
            round.pending_request = None;
            self.book_turn_usage(
                &mut round.state,
                &response,
                streamed_usage.take(),
                &mut request_accounting,
            );
            messages.push(response.clone());

            // `emitted_text` means assistant text was already streamed to the
            // UI; a text-fallback tool call must then retract it via a discard.
            if self
                .dispatch_tool_calls(
                    &response,
                    messages,
                    &mut round.state,
                    emitted_text,
                    cancel,
                    &mut on_event,
                )
                .await?
            {
                round.turn_index += 1;
                if self.check_hard_stop(round.turn_index).is_break() {
                    return Err(self.hard_stop_error());
                }
                self.project_context_if_needed(messages, cancel).await?;
                // Mid-round save point (ADR-0035): persist this turn's new
                // messages (the assistant response + all tool results) before
                // any further work, so a crash leaves the transcript in sync
                // with filesystem side effects.
                self.fire_turn_persist(messages).await?;
                self.run_turn_hooks(messages, &round.state, round.turn_index)
                    .await;
                // Restore TurnEnd-scoped disables (mirror of the non-streaming
                // path). RoundEnd-scoped disables survive until user-round end.
                self.restore_scoped_turn_end();
                continue;
            }

            // Round-exit gates (mirror of the non-streaming path). The insert
            // drain happens after the provider response commits, so an input
            // typed during a would-be final answer can still force one more
            // turn in this same round.
            let duration_ms = round.started_at.elapsed().as_millis() as u64;
            self.book_pursuit_pass(&mut round.state, duration_ms);
            let mut continue_round = false;
            if let Some((prompt, kind)) = self.stop_gate(&response).await {
                self.pursuit_state.bump_iterations();
                self.inject_pursuit_convergence_reminder(messages);
                messages.push(crate::conversation_context::hidden_user(kind, prompt));
                continue_round = true;
            }
            let admitted = self.admit_user_inputs(
                round.user_input_generation,
                messages,
                !continue_round,
                &mut on_event,
            );
            if admitted > 0 {
                continue_round = true;
            }
            if continue_round {
                round.turn_index += 1;
                if self.check_hard_stop(round.turn_index).is_break() {
                    return Err(self.hard_stop_error());
                }
                self.fire_turn_persist(messages).await?;
                self.run_turn_hooks(messages, &round.state, round.turn_index)
                    .await;
                self.restore_scoped_turn_end();
                continue;
            }

            // User-round end: clear every scoped disable so the toolset is
            // fresh for the next user request.
            self.restore_scoped_round_end();
            return Ok(RoundOutcome {
                message: response,
                token_usage: round.state.token_usage,
                duration_ms,
            });
        }
    }

    /// Execute any tool calls carried by `response`, emitting events and
    /// appending tool results to `messages`. Shared by the streaming and
    /// non-streaming loops so the dispatch contract — repeated-call guard,
    /// up-front `ToolCall` events, concurrent execution with FIFO-ordered
    /// results, and pursuit/mode updates — lives in exactly one place.
    ///
    /// `streamed_text` is true when the response text was already streamed to
    /// the UI, so a recognised text-fallback tool call retracts it with an
    /// `AssistantDiscard`. Returns `true` when a tool-carrying ReAct turn ran
    /// (the caller should loop again), `false` when the round is complete.
    ///
    /// `cancel` makes tool execution cooperative: if the turn is interrupted
    /// mid-flight, every already-announced [`AgentEvent::ToolCall`] is paired
    /// with a terminal [`AgentEvent::ToolCancelled`] before this returns
    /// `Err(HarnessError::Interrupted)`, so no step is left "running".
    async fn dispatch_tool_calls<F>(
        &self,
        response: &Message,
        messages: &mut Vec<Message>,
        state: &mut RoundState,
        streamed_text: bool,
        cancel: &CancellationToken,
        on_event: &mut F,
    ) -> Result<bool, HarnessError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        // Native tool calls (OpenAI-style function calling). An empty list is
        // treated as "no tool calls" so we fall through to the text fallback.
        if let Some(tool_calls) = response
            .tool_calls
            .as_ref()
            .filter(|calls| !calls.is_empty())
        {
            // Classify this turn once, for two consumers: the turn-hook axis
            // (consecutive read-only streak, surfaced to user hooks) and the
            // round-scoped guard registry (checked at the turn boundary). Any call
            // whose target is a real Path/Command (i.e. not Unspecified) makes
            // the turn "progress", resetting both.
            let all_read = tool_calls
                .iter()
                .all(|c| self.tool_target_is_unspecified(&c.name, &c.arguments));
            if all_read {
                state.consecutive_readonly_turns =
                    state.consecutive_readonly_turns.saturating_add(1);
            } else {
                state.consecutive_readonly_turns = 0;
            }

            // A provider retry may produce the same tool request again even
            // though its terminal result is already in the checkpointed
            // history. Treat exact matches as idempotency replays regardless
            // of the optional doom-loop setting.
            let checkpoint_replays: Vec<bool> = tool_calls
                .iter()
                .map(|call| state.is_checkpoint_replay(call))
                .collect();
            if checkpoint_replays.iter().any(|replayed| *replayed) {
                on_event(AgentEvent::Notice(
                    AgentNotice::new(
                        NoticeKind::ProviderRetry,
                        NoticeSeverity::Warning,
                        "Completed tool call not repeated",
                        NoticeSource::Harness,
                    )
                    .with_body(
                        "The retried model request repeated a tool call that already completed. \
                         The checkpointed result remains authoritative; the tool was not run again.",
                    )
                    .with_surface(NoticeSurface::Toast),
                ));
            }

            // Pre-dispatch doom-loop check (the decisive intervention). Before
            // any tool runs this turn, ask the doom guard whether any call is a
            // repeat of one already issued this round. A repeat is blocked here
            // and now — the tool never executes, so its result never enters
            // context. Unlike the post-hoc read-loop guard, this covers all
            // watched tools (bash/webfetch/edit/...), not just reads, and trips
            // on the *first* repeat (threshold = 2). `Block` records the
            // repeated signatures into the per-round mask, so the per-call
            // `is_blocked` filter below short-circuits them without re-running
            // the guard. We surface the guard's message as a notice + a hidden
            // user message so the model learns the call is refused.
            let doom_calls: Vec<(&str, &str)> = tool_calls
                .iter()
                .zip(&checkpoint_replays)
                .filter(|(_, replayed)| !**replayed)
                .map(|(call, _)| (call.name.as_str(), call.arguments.as_str()))
                .collect();
            let doom_action = state.guards.check_doom_ahead(&doom_calls);
            let doom_message: Option<String> = match &doom_action {
                crate::loop_guard::GuardAction::Block { message, .. } => {
                    tracing::warn!(
                        blocked = ?state.guards.blocked_summary(),
                        "doom guard blocked a repeating tool call before execution"
                    );
                    on_event(AgentEvent::Notice(
                        AgentNotice::new(
                            NoticeKind::NudgeInjected,
                            NoticeSeverity::Warning,
                            "Repeating tool call blocked",
                            NoticeSource::TurnGuard,
                        )
                        .with_body(
                            "The agent tried to re-run a tool call it already issued this round. \
                             The call was blocked before it ran — the result it already has is \
                             unchanged, so re-running it cannot help. The agent must change \
                             approach (or call `abort`).",
                        )
                        .with_surface(NoticeSurface::Toast),
                    ));
                    Some(message.clone())
                }
                _ => None,
            };

            // Emit all ToolCall events up front.
            let call_ids: Vec<String> = tool_calls
                .iter()
                .map(|_| format!("call_{}", uuid::Uuid::new_v4()))
                .collect();
            tracing::info!(count = tool_calls.len(), "dispatching native tool calls");
            for (call, id) in tool_calls.iter().zip(&call_ids) {
                tracing::debug!(tool = %call.name, "tool call");
                on_event(AgentEvent::ToolCall {
                    id: id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                });
            }
            // Signature-level loop guard (ADR-0036): a call whose canonical
            // signature is in the per-round block mask — set either by the
            // read-loop guard (a repeat that escalated past a nudge) or by the
            // doom guard above (any watched tool's first repeat) — is
            // short-circuited here, before execution. The model gets an
            // explanatory error instead of the content/side-effect, so it is
            // physically unable to re-enter the loop. Blocked calls are split
            // out so the rest run concurrently exactly as before. Each blocked
            // call is given the same ToolResult event an executed call would
            // get, so the UI never sees an orphaned running step.
            let blocked_output = |name: &str| {
                ToolOutput::Text(format!(
                    "[loop guard] This call ({name}) is blocked for the rest of the turn \
                     because it was a repeat of one already issued this round. Re-running it \
                     cannot help: the result is already in context above. Act on it now \
                     (use what you already have, try a *different* command/file/query), or, \
                     if you cannot proceed, say so explicitly or call `abort`."
                ))
            };
            let checkpoint_output = |name: &str| {
                ToolOutput::Text(format!(
                    "[retry checkpoint] This exact {name} call already completed before the \
                     provider retry. Its result is present earlier in the conversation and \
                     remains authoritative. The tool was not executed again."
                ))
            };
            let mut results: Vec<Option<(ToolOutput, u64)>> =
                (0..tool_calls.len()).map(|_| None).collect();
            let exec_indices: Vec<usize> = tool_calls
                .iter()
                .enumerate()
                .filter(|(idx, c)| {
                    if checkpoint_replays[*idx] {
                        tracing::warn!(
                            tool = %c.name,
                            args = %c.arguments,
                            "provider retry repeated a completed tool call"
                        );
                        let output = checkpoint_output(&c.name);
                        let id = &call_ids[*idx];
                        on_event(AgentEvent::ToolResult {
                            id: id.clone(),
                            name: c.name.clone(),
                            output: output.to_text(),
                            structured: output.clone(),
                            duration_ms: 0,
                        });
                        results[*idx] = Some((output, 0));
                        false
                    } else if state.guards.is_blocked(&c.name, &c.arguments) {
                        tracing::warn!(
                            tool = %c.name,
                            args = %c.arguments,
                            "tool call blocked by turn-loop guard signature mask"
                        );
                        let output = blocked_output(&c.name);
                        let id = &call_ids[*idx];
                        // Emit the ToolResult the executed path would have, so
                        // the UI pairs this call's ToolCall with a terminal
                        // result instead of leaving it "running".
                        on_event(AgentEvent::Notice(
                            AgentNotice::new(
                                NoticeKind::NudgeInjected,
                                NoticeSeverity::Warning,
                                "Blocked repeating tool call",
                                NoticeSource::TurnGuard,
                            )
                            .with_body(format!(
                                "A tool call ({}) was blocked by the loop guard — it is a \
                                 repeat of a call already issued this round. Use the result \
                                 already in context, or try a different call.",
                                c.name,
                            ))
                            .with_surface(NoticeSurface::Toast),
                        ));
                        on_event(AgentEvent::ToolResult {
                            id: id.clone(),
                            name: c.name.clone(),
                            output: output.to_text(),
                            structured: output.clone(),
                            duration_ms: 0,
                        });
                        results[*idx] = Some((output, 0));
                        false // do not execute
                    } else {
                        true // execute
                    }
                })
                .map(|(idx, _)| idx)
                .collect();
            if !exec_indices.is_empty() {
                let exec_calls: Vec<ToolCall> = exec_indices
                    .iter()
                    .map(|&i| tool_calls[i].clone())
                    .collect();
                let exec_ids: Vec<String> =
                    exec_indices.iter().map(|&i| call_ids[i].clone()).collect();
                let exec_results = self
                    .execute_tools_concurrent(&exec_calls, &exec_ids, cancel, on_event)
                    .await?;
                for (pos, &idx) in exec_indices.iter().enumerate() {
                    results[idx] = exec_results.get(pos).cloned();
                    state.remember_completed_tool(&tool_calls[idx]);
                }
            }
            // Flatten back to a positional Vec, matching tool_calls order.
            let results: Vec<(ToolOutput, u64)> = results
                .into_iter()
                .map(|r| {
                    r.unwrap_or_else(|| (ToolOutput::Text("[loop guard] blocked".to_string()), 0))
                })
                .collect();
            let denied = results
                .iter()
                .any(|(result, _)| matches!(result, ToolOutput::PermissionDenied { .. }));
            for (idx, ((call, id), (result, duration_ms))) in
                tool_calls.iter().zip(&call_ids).zip(results).enumerate()
            {
                self.record_tool_result(
                    call,
                    id,
                    &result,
                    duration_ms,
                    messages,
                    state,
                    checkpoint_replays[idx],
                    false,
                    on_event,
                );
                if !checkpoint_replays[idx] {
                    self.run_post_tool_hooks(call, &result, duration_ms, messages)
                        .await;
                }
            }
            // If the user denied permission for any call, stop the round here
            // instead of feeding the (possibly partial) results back to the
            // model and asking it to continue.
            // If the doom guard blocked any repeats this round, deliver its
            // consolidated message as a hidden user note alongside the blocked
            // tool results, so the model learns *why* its call was refused and
            // what to do instead. Non-terminating: the turn continues with the
            // (now masked) signatures hard-blocked for subsequent turns in this round.
            if let Some(message) = doom_message {
                messages.push(crate::conversation_context::hidden_user(
                    InjectionKind::LoopReviewNudge,
                    message,
                ));
            }
            return Ok(!denied);
        }

        // Text-based fallback: any provider may emit a JSON tool call as text.
        if let Some(call) = crate::tool_call::parse_text_tool_call(&response.content) {
            if streamed_text {
                on_event(AgentEvent::AssistantDiscard);
            }
            tracing::debug!(tool = %call.name, "tool call (text fallback)");
            crate::tool_call::attach_fallback_tool_call(messages, &call);
            // Classify + feed this turn to the guard, mirroring the native
            // path. The text-fallback emits one call per turn, so a read-only
            // turn is exactly "this single call is read-tier". Without this the
            // guard would never see text-fallback turns and a model on such a
            // provider could loop with zero coverage.
            let all_read = self.tool_target_is_unspecified(&call.name, &call.arguments);
            if all_read {
                state.consecutive_readonly_turns =
                    state.consecutive_readonly_turns.saturating_add(1);
            } else {
                state.consecutive_readonly_turns = 0;
            }
            let checkpoint_replay = state.is_checkpoint_replay(&call);
            if checkpoint_replay {
                on_event(AgentEvent::Notice(
                    AgentNotice::new(
                        NoticeKind::ProviderRetry,
                        NoticeSeverity::Warning,
                        "Completed tool call not repeated",
                        NoticeSource::Harness,
                    )
                    .with_body(
                        "The retried model request repeated a tool call that already completed. \
                         The checkpointed result remains authoritative; the tool was not run again.",
                    )
                    .with_surface(NoticeSurface::Toast),
                ));
            }
            // Pre-dispatch doom check, mirroring the native path: catch a repeat
            // *before* the text-fallback tool runs.
            let doom_action = if checkpoint_replay {
                crate::loop_guard::GuardAction::Continue
            } else {
                state
                    .guards
                    .check_doom_ahead(&[(call.name.as_str(), call.arguments.as_str())])
            };
            let doom_message: Option<String> = match &doom_action {
                crate::loop_guard::GuardAction::Block { message, .. } => {
                    tracing::warn!(
                        tool = %call.name,
                        args = %call.arguments,
                        "text-fallback call blocked by doom guard before execution"
                    );
                    Some(message.clone())
                }
                _ => None,
            };
            let call_id = format!("call_{}", uuid::Uuid::new_v4());
            on_event(AgentEvent::ToolCall {
                id: call_id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            });
            // Signature-level loop guard: short-circuit a blocked call before it
            // executes (ADR-0036). Same contract as the native path — the model
            // gets an explanatory error instead of the content. Covers any tool
            // masked by either the read-loop guard or the doom guard above.
            let guard_blocked = state.guards.is_blocked(&call.name, &call.arguments);
            let result = if checkpoint_replay {
                tracing::warn!(
                    tool = %call.name,
                    args = %call.arguments,
                    "provider retry repeated a completed text-fallback tool call"
                );
                let output = ToolOutput::Text(format!(
                    "[retry checkpoint] This exact {} call already completed before the \
                     provider retry. Its result is present earlier in the conversation and \
                     remains authoritative. The tool was not executed again.",
                    call.name,
                ));
                on_event(AgentEvent::ToolResult {
                    id: call_id.clone(),
                    name: call.name.clone(),
                    output: output.to_text(),
                    structured: output.clone(),
                    duration_ms: 0,
                });
                output
            } else if guard_blocked {
                tracing::warn!(
                    tool = %call.name,
                    args = %call.arguments,
                    "text-fallback call blocked by turn-loop guard signature mask"
                );
                on_event(AgentEvent::Notice(
                    AgentNotice::new(
                        NoticeKind::NudgeInjected,
                        NoticeSeverity::Warning,
                        "Blocked repeating tool call",
                        NoticeSource::TurnGuard,
                    )
                    .with_body(format!(
                        "A tool call ({}) was blocked by the loop guard — it is a repeat \
                         of a call already issued this round. Use the result already in \
                         context, or try a different call.",
                        call.name,
                    ))
                    .with_surface(NoticeSurface::Toast),
                ));
                let output = ToolOutput::Text(format!(
                    "[loop guard] This call ({}) is blocked for the rest of the turn \
                     because it was a repeat of one already issued this round. Re-running it \
                     cannot help: the result is already in context above. Act on it now \
                     (use what you already have, try a *different* command/file/query), or, \
                     if you cannot proceed, say so explicitly or call `abort`.",
                    call.name,
                ));
                on_event(AgentEvent::ToolResult {
                    id: call_id.clone(),
                    name: call.name.clone(),
                    output: output.to_text(),
                    structured: output.clone(),
                    duration_ms: 0,
                });
                output
            } else {
                self.execute_tool_evented(&call, &call_id, cancel, on_event)
                    .await?
            };
            if !checkpoint_replay && !guard_blocked {
                state.remember_completed_tool(&call);
            }
            let denied = matches!(result, ToolOutput::PermissionDenied { .. });
            let duration_ms = std::time::Instant::now().elapsed().as_millis() as u64;
            self.record_tool_result(
                &call,
                &call_id,
                &result,
                duration_ms,
                messages,
                state,
                checkpoint_replay,
                true,
                on_event,
            );
            if !checkpoint_replay {
                self.run_post_tool_hooks(&call, &result, duration_ms, messages)
                    .await;
            }
            if let Some(message) = doom_message {
                messages.push(crate::conversation_context::hidden_user(
                    InjectionKind::LoopReviewNudge,
                    message,
                ));
            }
            return Ok(!denied);
        }

        Ok(false)
    }

    /// Account for, surface, and persist a single tool result. The argument
    /// count reflects the per-result state it must thread; grouping it further
    /// would only move the noise to the call sites.
    #[allow(clippy::too_many_arguments)]
    fn record_tool_result<F>(
        &self,
        call: &ToolCall,
        call_id: &str,
        result: &ToolOutput,
        duration_ms: u64,
        messages: &mut Vec<Message>,
        state: &mut RoundState,
        checkpoint_replay: bool,
        emit_event: bool,
        on_event: &mut F,
    ) where
        F: FnMut(AgentEvent) + Send,
    {
        let text = result.to_text();
        // Cost attribution: an envoy's true token consumption can be 100x
        // the byte-estimate of its final summary, so accumulate the real
        // `TokenUsage` it reported. For every other tool the byte-estimate
        // remains the only signal we have.
        if checkpoint_replay {
            // The short checkpoint reference is new model-visible context,
            // but the original tool (especially an envoy) did no new work, so
            // do not attribute its nested usage a second time.
            state.token_usage.total_tokens += pressure::estimate_string_tokens(&text);
        } else if let Some((_sub_messages, sub_usage)) = result.envoy_payload() {
            state.token_usage.total_tokens += sub_usage.total_tokens;
            state.token_usage.prompt_tokens += sub_usage.prompt_tokens;
            state.token_usage.completion_tokens += sub_usage.completion_tokens;
            // Still count the summary bytes that the parent model will
            // actually re-read on the next turn.
            state.token_usage.total_tokens += pressure::estimate_string_tokens(&text);
        } else {
            state.token_usage.total_tokens += pressure::estimate_string_tokens(&text);
        }
        tracing::info!(
            tool = %call.name,
            duration_ms,
            bytes = text.len(),
            checkpoint_replay,
            "tool result"
        );
        if !checkpoint_replay {
            self.emit_todos_change(call, on_event);
        }
        if emit_event {
            on_event(AgentEvent::ToolResult {
                id: call_id.to_string(),
                name: call.name.clone(),
                output: text.clone(),
                structured: result.clone(),
                duration_ms,
            });
        }
        // For envoy results, attach the nested transcript as `children` on
        // the persisted Tool-role message so resume can rebuild the envoy
        // view without a live event stream. The nested `Message`s already
        // self-contain their own tool_calls / tool_call_id / children, so
        // arbitrarily deep envoy trees round-trip through session.json.
        // Sidecar `envoy_meta` captures what the live event stream knew but
        // the bare transcript cannot reconstruct on resume: duration, the
        // task description, the toolset size, and an explicit failure flag.
        // The envoy result text is built by `envoy_result_text`, which appends
        // a deterministic role-reanchoring note at this single choke point (see
        // its doc for the "role bleed" rationale). For non-envoy results the
        // plain header is used unchanged.
        let tool_message = match result.envoy_payload() {
            Some((sub_messages, _)) => {
                let meta = crate::message::EnvoyMeta {
                    duration_ms: Some(duration_ms),
                    failed: result.is_error(),
                    ..Default::default()
                };
                Message::tool_result(
                    call,
                    envoy_result_text(&call.name, &text, result.is_error()),
                )
                .with_children(sub_messages.to_vec())
                .with_envoy_meta(meta)
            }
            None => Message::tool_result(call, format!("[{} result]:\n{}", call.name, text)),
        };
        messages.push(tool_message);

        // Image peel-out (mirrors opencode's OpenAI-Chat lowering). The tool
        // message only carries text (OpenAI Chat Completions requires tool
        // content to be a string), so the actual image is injected as a
        // follow-up user-role message with the image attached — the same
        // channel paste-up uses. The provider serialises it to `image_url`
        // (OpenAI-compat) / `inline_data` (Gemini), letting the model see the
        // pixels. A short textual link ties the two messages together.
        if let ToolOutput::Image { mime, data } = result {
            messages.push(crate::conversation_context::tool_image(
                &call.name,
                mime.clone(),
                data.clone(),
            ));
        }
    }

    /// Fire PostToolUse (success) or PostToolUseFailure (error) hooks and append
    /// any injected context as hidden user messages (ADR-0025). No-op when the
    /// registry is empty, which is the common case (envoys, tests, no
    /// `[hooks]` config).
    async fn run_post_tool_hooks(
        &self,
        call: &ToolCall,
        result: &ToolOutput,
        duration_ms: u64,
        messages: &mut Vec<Message>,
    ) {
        let registry = self.hooks();
        if registry.is_empty() {
            return;
        }
        let summary = result.to_text();
        let session_id = self.hook_session_id();
        let cwd = self.hook_cwd();
        let is_error = result.is_error();
        let injected = if is_error {
            registry
                .run_post_tool_use_failure(
                    call.name.as_str(),
                    &summary,
                    &session_id,
                    cwd.as_deref(),
                )
                .await
        } else {
            registry
                .run_post_tool_use(
                    call.name.as_str(),
                    &summary,
                    duration_ms,
                    &session_id,
                    cwd.as_deref(),
                )
                .await
        };
        let kind = if is_error {
            InjectionKind::Hook(HookEventKind::PostToolUseFailure)
        } else {
            InjectionKind::Hook(HookEventKind::PostToolUse)
        };
        for context in injected {
            messages.push(crate::conversation_context::hidden_user(kind, context));
        }
    }

    /// Whether a tool call's [`ScopeTarget`] is [`ScopeTarget::Unspecified`] —
    /// i.e. the tool declares no locatable target (a pure read/search like
    /// `read_text`, `grep`). Used to classify a turn as read-only for the
    /// turn-hook streak counter. An unknown tool name reads as `true`
    /// (unspecified), matching the trait default.
    fn tool_target_is_unspecified(&self, name: &str, arguments: &str) -> bool {
        match self
            .resolved_tools
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|t| t.name() == name)
        {
            Some(t) => matches!(
                t.scope_target(arguments),
                neenee_core::ScopeTarget::Unspecified
            ),
            None => true,
        }
    }

    /// Fire user-configured `Turn` hooks at the turn boundary and fold any
    /// `Inject` context into hidden user messages. `Deny` is already discarded
    /// by [`HookRegistry::run_turn`], so a turn hook cannot abort the round.
    /// `ScopeTools` disables are applied to the scoped mask.
    async fn run_turn_hooks(&self, messages: &mut Vec<Message>, state: &RoundState, turn: usize) {
        let registry = self.hooks();
        if registry.is_empty() {
            return;
        }
        let side = registry
            .run_turn(
                self.round_count(),
                turn,
                state.consecutive_readonly_turns,
                &self.hook_session_id(),
                self.hook_cwd().as_deref(),
            )
            .await;
        for context in side.injected {
            messages.push(crate::conversation_context::hidden_user(
                InjectionKind::Hook(HookEventKind::Turn),
                context,
            ));
        }
        self.apply_scoped_disables(&side.scoped_disables);
    }

    /// Fire `TurnStart` hooks at the start of each ReAct
    /// turn (after tools are prepared, before the next model completion) and
    /// fold any `Inject` context into hidden user messages. `Deny` is already
    /// discarded by [`HookRegistry::run_turn_start`], so this hook
    /// cannot abort the round. `ScopeTools` disables are applied to the scoped
    /// mask. The symmetric partner of [`Self::run_turn_hooks`].
    async fn run_turn_start_hooks(
        &self,
        messages: &mut Vec<Message>,
        state: &RoundState,
        turn: usize,
    ) {
        let registry = self.hooks();
        if registry.is_empty() {
            return;
        }
        let side = registry
            .run_turn_start(
                self.round_count(),
                turn,
                state.consecutive_readonly_turns,
                &self.hook_session_id(),
                self.hook_cwd().as_deref(),
            )
            .await;
        for context in side.injected {
            messages.push(crate::conversation_context::hidden_user(
                InjectionKind::Hook(HookEventKind::TurnStart),
                context,
            ));
        }
        self.apply_scoped_disables(&side.scoped_disables);
    }

    /// Fire `PermissionRequest` hooks at the moment the agent is about to block
    /// on a permission decision. Observe-only: hooks run for side effects (the
    /// canonical use is a fire-and-forget notification so the user notices the
    /// agent is parked); outcomes are ignored by the registry. No-op without a
    /// `[hooks]` config.
    async fn fire_permission_request_hooks(&self, request: &neenee_core::PermissionRequest) {
        let registry = self.hooks();
        if registry.is_empty() {
            return;
        }
        registry
            .run_permission_request(request, &self.hook_session_id(), self.hook_cwd().as_deref())
            .await;
    }

    /// Fire `UserQuestion` hooks at the moment the agent is about to block on
    /// an `ask_user` question. Observe-only, same contract as
    /// [`Self::fire_permission_request_hooks`].
    async fn fire_user_question_hooks(&self, request: &neenee_core::UserQuestionRequest) {
        let registry = self.hooks();
        if registry.is_empty() {
            return;
        }
        registry
            .run_user_question(request, &self.hook_session_id(), self.hook_cwd().as_deref())
            .await;
    }

    /// The opt-in hard-stop gate (ADR-0018). Called once per continuing ReAct
    /// turn with the count of turns already run in this round. Returns
    /// `ControlFlow::Break` only when a finite `hard_stop_turns` budget was
    /// configured and `turns` has reached it — the caller converts that into
    /// a terminal `HarnessError` via [`Self::hard_stop_error`]. The default
    /// budget (`0`) keeps the round uncapped, exactly matching ADR-0009.
    ///
    /// Session review no longer fires automatically from the loop: it is on-demand via
    /// `/review` ([`Self::review_now`]), which runs the diagnostic envoy
    /// against the live transcript and reports a verdict without aborting.
    fn check_hard_stop(&self, turns: usize) -> std::ops::ControlFlow<()> {
        let budget = self.get_hard_stop_turns();
        if budget > 0 && turns >= budget {
            std::ops::ControlFlow::Break(())
        } else {
            std::ops::ControlFlow::Continue(())
        }
    }

    /// Terminal error surfaced when an opt-in `hard_stop_turns` budget is
    /// exhausted. Echoes the configured budget so the user can tell this apart
    /// from a normal completion in the transcript. The review itself never
    /// produces this — only an explicit user-configured budget does.
    fn hard_stop_error(&self) -> HarnessError {
        let budget = self.get_hard_stop_turns();
        HarnessError::Other(format!(
            "Agent stopped: the configured hard-stop budget of {budget} ReAct \
             turns was reached. This budget is opt-in (`hard_stop_turns`); \
             raise it or set it to 0 (the default) for an uncapped round."
        ))
    }

    /// Collapse a set of review verdicts into one human-facing alert string.
    /// Empty when every dimension is healthy (the TUI treats empty as "clear
    /// any prior alert"). Otherwise the worst status wins, with each
    /// non-healthy dimension's detail folded in. The turn count gives the user
    /// a sense of how long the round has run. Associated (no `&self`) so
    /// the `/review` handler and tests can call it without an `Agent` handle.
    pub fn render_review_alert(verdicts: &[ReviewVerdict], turns: usize) -> String {
        let worst = verdicts.iter().map(|v| v.status).max();
        match worst {
            None | Some(ReviewStatus::Healthy) => String::new(),
            Some(status) => {
                let label = status.label();
                let turn_unit = if turns == 1 { "turn" } else { "turns" };
                let details: Vec<&str> = verdicts
                    .iter()
                    .filter(|v| v.status != ReviewStatus::Healthy && !v.detail.trim().is_empty())
                    .map(|v| v.detail.trim())
                    .collect();
                if details.is_empty() {
                    format!("review: {label} · {turns} {turn_unit} — Esc to interrupt")
                } else {
                    format!(
                        "review: {label} · {turns} {turn_unit} — {} — Esc to interrupt",
                        details.join("; ")
                    )
                }
            }
        }
    }

    /// On-demand session review (ADR-0018): run the bounded read-only
    /// diagnostic envoy against `messages` and return one verdict per
    /// registered dimension. Driven by the `/review` command — the harness no
    /// longer fires review on a turn cadence. Safe to call while a round is
    /// running: the reviewer is an independent child agent that only reads a
    /// transcript snapshot and cannot mutate the parent's round state.
    pub async fn review_now(&self, messages: &[Message]) -> Vec<ReviewVerdict> {
        let turns = Self::estimate_completed_turns(messages);
        self.run_session_review(messages, turns).await
    }

    /// Rough count of tool-carrying turns represented by `messages`: the
    /// number of assistant messages that carry tool calls. Used to label
    /// on-demand review output with a sense of how long the round has run.
    pub fn estimate_completed_turns(messages: &[Message]) -> usize {
        messages
            .iter()
            .filter(|m| {
                m.role == Role::Assistant && m.tool_calls.as_ref().is_some_and(|c| !c.is_empty())
            })
            .count()
    }

    /// Emit a [`AgentEvent::TodosUpdated`] snapshot whenever a tool mutates
    /// the task list (`todo` full-replace or `todo_update` surgical edit).
    /// The TUI stores the snapshot and re-renders the sticky panel above the
    /// input box.
    fn emit_todos_change<F>(&self, call: &ToolCall, on_event: &mut F)
    where
        F: FnMut(AgentEvent) + Send,
    {
        if matches!(call.name.as_str(), "todo" | "todo_update") {
            on_event(AgentEvent::TodosUpdated(self.todos()));
        }
    }

    async fn execute_ask_user(
        &self,
        call: &ToolCall,
        _call_id: &str,
        event_tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> ToolOutput {
        let args: serde_json::Value = match serde_json::from_str(&call.arguments) {
            Ok(v) => v,
            Err(e) => {
                return ToolOutput::Text(format!("Invalid ask_user arguments: {}", e));
            }
        };
        let questions: Vec<UserQuestion> = match serde_json::from_value(
            args.get("questions")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        ) {
            Ok(q) => q,
            Err(e) => {
                return ToolOutput::Text(format!("Invalid ask_user questions: {}", e));
            }
        };
        if !(1..=5).contains(&questions.len()) {
            return ToolOutput::Text(
                "ask_user requires between one and five questions.".to_string(),
            );
        }
        for (i, q) in questions.iter().enumerate() {
            if !(2..=4).contains(&q.options.len()) {
                return ToolOutput::Text(format!(
                    "ask_user question {} requires between two and four options.",
                    i + 1
                ));
            }
        }

        let request = UserQuestionRequest {
            id: format!("ask_user_{}", uuid::Uuid::new_v4()),
            questions,
        };
        let (sender, receiver) = oneshot::channel();
        self.ask_user
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .insert(request.id.clone(), sender);
        tracing::info!(questions = request.questions.len(), "asking user");
        let _ = event_tx.send(AgentEvent::UserQuestionRequest(request.clone()));
        // Observe-only interrupt hook: fire notifications (desktop/bell) so the
        // user notices the agent is blocked on their input. No-op without
        // `[hooks]`. Outcomes are ignored — this never gates the question.
        self.fire_user_question_hooks(&request).await;

        match receiver.await.unwrap_or(None) {
            Some(reply) => {
                let output = serde_json::to_string_pretty(&reply.answers)
                    .unwrap_or_else(|_| format!("{:?}", reply.answers));
                ToolOutput::Text(format!(
                    "User answered the question(s). Selected option labels:\n{}",
                    output
                ))
            }
            None => {
                ToolOutput::Text("User cancelled the question; no answer was provided.".to_string())
            }
        }
    }

    /// Park an interactive-input request for a `bash` command (L3.5 β) and
    /// await the operator's reply. Called from `execute_tool` when the
    /// interactive classifier matches and no model-supplied stdin is
    /// authorized. Emits [`AgentEvent::InputRequest`]; the TUI shows an inline
    /// input panel and the reply travels back via [`Self::reply_input`].
    ///
    /// Returns `Some(StdinPolicy::Prefilled)` with the operator's input, or
    /// `None` if the operator cancelled (the caller then runs the command with
    /// closed stdin → fast failure + non-interactive remedy footer).
    async fn collect_input_injection(
        &self,
        command: &str,
        prompt: &str,
        secret: bool,
        event_tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Option<StdinPolicy> {
        let request = InputRequest {
            id: format!("input_{}", uuid::Uuid::new_v4()),
            command: command.to_string(),
            prompt: prompt.to_string(),
            secret,
        };
        let (sender, receiver) = oneshot::channel();
        self.input
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .insert(request.id.clone(), sender);
        tracing::info!(%secret, "requesting operator input for interactive command");
        let _ = event_tx.send(AgentEvent::InputRequest(request.clone()));
        match receiver.await.unwrap_or(None) {
            Some(reply) if !reply.text.is_empty() => {
                Some(StdinPolicy::Prefilled { data: reply.text })
            }
            _ => None,
        }
    }

    /// Enforce the command-aware safety layer for `bash` before the ordinary
    /// permission broker. A broad cached permission such as `bash *` therefore
    /// cannot silently authorize commands the policy marks as destructive.
    ///
    /// Returns `Some(output)` when execution must stop, or `None` when the
    /// command may continue to the normal permission/stdin/spawn path.
    async fn check_bash_policy(
        &self,
        command: &str,
        arguments: &str,
        event_tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Option<ToolOutput> {
        let policy = self
            .bash_policy
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let decision = policy.evaluate(command)?;
        match decision.action {
            crate::bash_policy::BashPolicyAction::Allow => None,
            crate::bash_policy::BashPolicyAction::Deny => {
                tracing::warn!(command = %command, rule = %decision.name, "bash command blocked by policy");
                Some(ToolOutput::Error {
                    message: format!("[bash policy] Blocked dangerous command: {command}"),
                    detail: Some(format!(
                        "Rule: {}{}\nReason: {}\nThis command was not executed.",
                        decision.name,
                        if decision.builtin { " (built-in)" } else { "" },
                        decision.reason,
                    )),
                })
            }
            crate::bash_policy::BashPolicyAction::Confirm => {
                if self.get_unattended() {
                    return match policy.unattended_confirm_action() {
                        crate::bash_policy::BashPolicyAction::Allow => {
                            tracing::warn!(
                                command = %command,
                                rule = %decision.name,
                                "bash policy confirmation bypassed by unattended_confirm=allow"
                            );
                            None
                        }
                        _ => Some(ToolOutput::Error {
                            message: format!(
                                "[bash policy] Dangerous command requires confirmation but session is unattended: {command}"
                            ),
                            detail: Some(format!(
                                "Rule: {}{}\nReason: {}\nThis command was not executed.",
                                decision.name,
                                if decision.builtin { " (built-in)" } else { "" },
                                decision.reason,
                            )),
                        }),
                    };
                }

                let request = PermissionRequest {
                    id: format!("permission_{}", uuid::Uuid::new_v4()),
                    tool: "bash".to_string(),
                    label: "Dangerous bash command".to_string(),
                    description: format!(
                        "Bash policy requires one-off confirmation before running this command.\n\nRule: {}{}\nReason: {}\n\nA broad bash allowlist entry does not bypass this safety check.",
                        decision.name,
                        if decision.builtin { " (built-in)" } else { "" },
                        decision.reason,
                    ),
                    arguments: arguments.to_string(),
                    scope: command.to_string(),
                };
                let receiver = self.permissions.park_request(request.id.clone());
                tracing::info!(command = %command, rule = %decision.name, "bash policy confirmation requested");
                let _ = event_tx.send(AgentEvent::PermissionRequest(request.clone()));
                self.fire_permission_request_hooks(&request).await;

                match receiver.await.unwrap_or(PermissionDecision::Reject) {
                    PermissionDecision::Once | PermissionDecision::Always => {
                        // Deliberately do not persist `Always`: a dangerous-command
                        // confirmation is sharper than ordinary tool permission and
                        // must stay one-off unless the user writes an explicit
                        // `[bash_policy.rules] action = "allow"` override.
                        tracing::info!(command = %command, "bash policy confirmation granted once");
                        None
                    }
                    PermissionDecision::Reject => {
                        tracing::warn!(command = %command, "bash policy confirmation rejected");
                        Some(ToolOutput::PermissionDenied {
                            tool: "bash".to_string(),
                        })
                    }
                }
            }
        }
    }

    /// Three-way stdin policy for a `bash` call (L3 + L3.5). See the decision
    /// block in [`Self::execute_tool`] for the contract. `arguments` is the
    /// raw JSON tool arguments.
    async fn decide_bash_stdin(
        &self,
        arguments: &str,
        event_tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> StdinPolicy {
        // (α) opt-in model stdin: only when the flag is on, which is what
        // dynamically exposes the `stdin` schema field. Read it defensively
        // (absent/invalid → not a model-supplied stdin).
        if self.allow_model_stdin()
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(arguments)
            && let Some(data) = v.get("stdin").and_then(|s| s.as_str())
            && !data.is_empty()
        {
            return StdinPolicy::Prefilled {
                data: data.to_string(),
            };
        }
        // (β) human input: classify the command; if interactive, ask the
        // operator. `command` is read from the args (bash's scope_target
        // already extracts it, but re-reading here keeps this self-contained).
        let command = serde_json::from_str::<serde_json::Value>(arguments)
            .ok()
            .and_then(|v| {
                v.get("command")
                    .and_then(|c| c.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_default();
        if let Some(input_kind) = crate::shell_input::classify(&command) {
            // Unattended: no operator is reachable to type into the prompt, so
            // the inline input panel would deadlock. Close stdin instead — the
            // command then fails fast with a non-interactive remedy, which is
            // the honest outcome for an interactive command run unattended.
            if self.get_unattended() {
                tracing::info!(command = %command, "interactive command stdin closed under unattended");
                return StdinPolicy::default();
            }
            let secret = input_kind.is_secret();
            let prompt = if secret {
                "Enter the secret this command is waiting for:".to_string()
            } else {
                format!("This command needs input ({command}):")
            };
            return self
                .collect_input_injection(&command, &prompt, secret, event_tx)
                .await
                .unwrap_or_default();
        }
        // (default) closed hard floor.
        StdinPolicy::default()
    }

    async fn execute_tool(
        &self,
        call: &ToolCall,
        call_id: &str,
        event_tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> ToolOutput {
        let tool: Arc<dyn Tool> = match self.tool_manager.find(&call.name) {
            Some(sourced) => sourced.tool,
            None => {
                return ToolOutput::Error {
                    message: format!("Tool '{}' not found", call.name),
                    detail: None,
                };
            }
        };

        // ── Permission policy chain (full async chain) ──
        // Every permission gate — PreToolUse hook, disabled mask, schema
        // validation, operation-scope gate, bash policy (Deny/unattended),
        // ask_user shortcut, and the broker's always-allowed fast path — runs
        // as one chain evaluation (see `permission_policy`). The chain is
        // async because some gates await (hooks, bash policy). Outcomes:
        //   • Deny    → short-circuit with the policy's output.
        //   • Approve → proceed (already-allowed, or unattended bypass).
        //   • Ask     → the broker wants a live user decision: park, emit the
        //               request, fire observe hooks, await.
        //   • Pass    → (chain fallback) proceed.
        let target = tool.scope_target(&call.arguments);
        // Snapshot the disable masks and scope *before* the chain runs, then
        // drop the guards — the chain is async and MutexGuards are not Send, so
        // they must not live across the `.await`.
        let (disabled_snapshot, scoped_snapshot, operation_scope) = {
            let disabled = self
                .disabled_tools
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let scoped = self
                .scoped_disabled_tools
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let scope = self.operation_scope();
            (disabled, scoped, scope)
        };
        let pctx = crate::permission_policy::PolicyContext {
            tool: &tool,
            call_name: call.name.as_str(),
            arguments: &call.arguments,
            scope_target: target.clone(),
            unattended: self.get_unattended(),
            operation_scope,
            disabled: disabled_snapshot,
            scoped_disabled: scoped_snapshot,
            ctx: self, // Agent: PermissionContext
        };
        match self.permission_chain().evaluate(&pctx).await {
                crate::permission_policy::PolicyDecision::Pass
                | crate::permission_policy::PolicyDecision::Approve => {}
                crate::permission_policy::PolicyDecision::Deny { output, .. } => {
                    return output;
                }
                crate::permission_policy::PolicyDecision::Ask { request, rule } => {
                    // The broker's interactive park. Fill the request id, emit,
                    // fire observe hooks, await the user's decision.
                    let request = neenee_core::PermissionRequest {
                        id: format!("permission_{}", uuid::Uuid::new_v4()),
                        ..request
                    };
                    let receiver = self.permissions.park_request(request.id.clone());
                    tracing::info!(tool = %request.tool, scope = %request.scope, "permission requested");
                    let _ = event_tx.send(AgentEvent::PermissionRequest(request.clone()));
                    self.fire_permission_request_hooks(&request).await;
                    match receiver.await.unwrap_or(PermissionDecision::Reject) {
                        PermissionDecision::Once => {
                            tracing::info!(tool = %tool.name(), decision = "once", "permission granted");
                        }
                        PermissionDecision::Always => {
                            tracing::info!(tool = %tool.name(), decision = "always", "permission granted");
                            self.permissions.add_always(rule);
                        }
                        PermissionDecision::Reject => {
                            tracing::warn!(tool = %tool.name(), "permission denied");
                            return ToolOutput::PermissionDenied {
                                tool: tool.name().to_string(),
                            };
                        }
                    }
                }
            }

            // Bash interactive Confirm: the chain's BashPolicy handled Deny and
        // unattended-Confirm, but a non-unattended Confirm needs the event
        // channel to park for one-off approval. Re-run the full check here —
        // it's idempotent for Deny (already short-circuited) and a no-op for
        // Allow; only Confirm reaches the park.
        if call.name == "bash"
            && let neenee_core::ScopeTarget::Command(command) = &target
            && let Some(output) = self
                .check_bash_policy(command, &call.arguments, event_tx)
                .await
        {
            return output;
        }

        // ask_user: the chain's AskUserPolicy refused under unattended; here we
        // execute the interactive path (park for a user answer).
        if call.name == "ask_user" {
            return self.execute_ask_user(call, call_id, event_tx).await;
        }

        // ── Stdin policy decision (L3 + L3.5) ──
        // Decided here, before spawn, for bash only. The three-way decision:
        //   1. opt-in model stdin (α): `allow_model_stdin` on AND the model
        //      supplied a `stdin` arg → Prefilled{model}. Structurally
        //      unreachable unless the flag exposed the schema field.
        //   2. human input (β, default): the interactive classifier matched →
        //      ask the operator; Prefilled{human} or Closed (if cancelled).
        //   3. closed (default hard floor): everything else.
        // For non-bash tools, Closed is always correct (they ignore stdin).
        let stdin_policy = if call.name == "bash" {
            self.decide_bash_stdin(&call.arguments, event_tx).await
        } else {
            StdinPolicy::default()
        };

        // The Envoy / ToolStream events must carry the same id as the
        // up-front ToolCall event (the dispatch-generated `call_id`), not the
        // model's `call.id` — the UI keys its step off the ToolCall event id,
        // so using `call.id` here would orphan every envoy child stream and
        // every live tool stream, leaving the envoy view empty.
        let parent_call_id = call_id.to_string();
        let stream_call_id = call_id.to_string();
        let stream_tx = event_tx.clone();
        let mut on_stream = move |stream: ToolStream| {
            let _ = stream_tx.send(AgentEvent::ToolStream {
                id: stream_call_id.clone(),
                stream,
            });
        };
        match tool
            .call_structured_with_events(
                call_id,
                &call.arguments,
                Box::new(|event| {
                    let _ = event_tx.send(AgentEvent::Envoy {
                        parent_call_id: parent_call_id.clone(),
                        event,
                    });
                }),
                &mut on_stream,
                stdin_policy,
            )
            .await
        {
            Ok(output) => output,
            Err(err) => ToolOutput::Error {
                message: format!("Error executing {}: {}", call.name, err),
                detail: None,
            },
        }
    }

    /// Single-call wrapper that forwards channel events to a mutable callback.
    /// Used by text-fallback paths (one tool call at a time).
    ///
    /// Cancellation-aware: if `cancel` fires while the tool is in flight, the
    /// already-announced call (identified by `call_id`) is paired with a
    /// terminal [`AgentEvent::ToolCancelled`] and this returns
    /// `Err(HarnessError::Interrupted)`.
    pub(crate) async fn execute_tool_evented<F>(
        &self,
        call: &ToolCall,
        call_id: &str,
        cancel: &CancellationToken,
        on_event: &mut F,
    ) -> Result<ToolOutput, HarnessError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let fut = self.execute_tool(call, call_id, &tx);
        tokio::pin!(fut);
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    while let Ok(event) = rx.try_recv() {
                        on_event(event);
                    }
                    on_event(AgentEvent::ToolCancelled {
                        id: call_id.to_string(),
                        name: call.name.clone(),
                    });
                    return Err(HarnessError::Interrupted);
                }
                event = rx.recv() => {
                    if let Some(event) = event {
                        on_event(event);
                    }
                }
                result = &mut fut => {
                    while let Ok(event) = rx.try_recv() {
                        on_event(event);
                    }
                    return Ok(result);
                }
            }
        }
    }

    /// Execute multiple tool calls concurrently, forwarding interleaved events
    /// to the callback in real time. Returns `(result, duration_ms)` pairs in
    /// the same order as the input calls.
    ///
    /// Cancellation-aware: an interrupt emits a [`AgentEvent::ToolCancelled`]
    /// for every dispatched call id (the whole batch is abandoned — partial
    /// side effects are neither recorded nor replayed by the caller) and
    /// returns `Err(HarnessError::Interrupted)`.
    async fn execute_tools_concurrent<F>(
        &self,
        calls: &[ToolCall],
        call_ids: &[String],
        cancel: &CancellationToken,
        on_event: &mut F,
    ) -> Result<Vec<(ToolOutput, u64)>, HarnessError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        // Stage-2 switchover: partition the batch into conflict-free sub-batches
        // via declarative ToolAccesses (kimi-code model). Calls in the same
        // sub-batch run concurrently (historical join_all behavior); sub-batches
        // run strictly in order, so two writes to the same file — or a read and
        // a write of the same path — never race. Non-conflicting reads still
        // parallelize. This replaces the previous "join_all everything" which
        // could let two edits of the same file clobber each other.
        //
        // accesses are computed up front from each call's resolved tool (a few
        // HashMap lookups; negligible vs. the tool call itself). A tool that
        // can't be resolved gets `none()` (freely parallel) — it will still
        // produce its "not found" error inside `execute_tool`.
        let accesses: Vec<neenee_core::ToolAccesses> = calls
            .iter()
            .map(|call| self.accesses_for_call(call))
            .collect();
        let assignment = neenee_core::tool_access::group_by_conflict(&accesses);
        let batch_count = assignment.iter().copied().max().map(|m| m + 1).unwrap_or(0);

        let mut results: Vec<(ToolOutput, u64)> = Vec::with_capacity(calls.len());
        // Per-input result slots, filled as batches complete, then flattened in
        // input order at the end (preserving the dispatcher's order invariant).
        let mut slots: Vec<Option<(ToolOutput, u64)>> = vec![None; calls.len()];

        for batch in 0..batch_count {
            // Collect this batch's (call_index) members, in input order.
            let members: Vec<usize> = (0..calls.len()).filter(|&i| assignment[i] == batch).collect();
            if members.is_empty() {
                continue;
            }

            let (tx, mut rx) = mpsc::unbounded_channel();
            let futs: Vec<_> = members
                .iter()
                .map(|&i| {
                    let tx = tx.clone();
                    let name = calls[i].name.clone();
                    let call_id = call_ids[i].clone();
                    let call = calls[i].clone();
                    async move {
                        let started = std::time::Instant::now();
                        let result = self.execute_tool(&call, &call_id, &tx).await;
                        let duration_ms = started.elapsed().as_millis() as u64;
                        // Emit ToolResult immediately so the TUI transitions
                        // this step Running→Completed without waiting for
                        // siblings in the same batch.
                        let output = result.to_text();
                        let _ = tx.send(AgentEvent::ToolResult {
                            id: call_id.clone(),
                            name: name.clone(),
                            output,
                            structured: result.clone(),
                            duration_ms,
                        });
                        (i, result, duration_ms)
                    }
                })
                .collect();

            let batch_fut = join_all(futs);
            tokio::pin!(batch_fut);

            // Same event loop as before, but bounded to this one batch.
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        while let Ok(event) = rx.try_recv() {
                            on_event(event);
                        }
                        // Cancel abandons the whole batch (and round): emit
                        // ToolCancelled for every dispatched id, not just this
                        // batch's, matching the historical whole-batch abort.
                        for (id, call) in call_ids.iter().zip(calls) {
                            on_event(AgentEvent::ToolCancelled {
                                id: id.clone(),
                                name: call.name.clone(),
                            });
                        }
                        return Err(HarnessError::Interrupted);
                    }
                    event = rx.recv() => {
                        if let Some(event) = event {
                            on_event(event);
                        }
                    }
                    batch_results = &mut batch_fut => {
                        while let Ok(event) = rx.try_recv() {
                            on_event(event);
                        }
                        for (i, result, duration_ms) in batch_results {
                            slots[i] = Some((result, duration_ms));
                        }
                        break;
                    }
                }
            }
        }

        // Flatten in input order. Any slot still None means its batch was
        // never reached (shouldn't happen outside cancel); synthesize a
        // loop-guard placeholder to keep the contract non-panicking.
        for slot in slots {
            results.push(slot.unwrap_or((
                ToolOutput::Text("[loop guard] blocked".to_string()),
                0,
            )));
        }
        Ok(results)
    }

    /// Resolve `call`'s tool (resolved → dynamic fallback) and return its
    /// declared [`ToolAccesses`]. Used by the dispatcher to group calls into
    /// conflict-free batches. A tool that can't be resolved yields
    /// [`ToolAccesses::none`] (freely parallel) — it will report its own
    /// "not found" error inside `execute_tool`; there's no point serializing
    /// an error.
    fn accesses_for_call(&self, call: &ToolCall) -> neenee_core::ToolAccesses {
        let tool: Option<Arc<dyn Tool>> = self
            .resolved_tools
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|t| t.name() == call.name)
            .cloned()
            .or_else(|| self.dynamic_tools.find(&call.name));
        match tool {
            Some(tool) => tool.accesses(&call.arguments),
            None => neenee_core::ToolAccesses::none(),
        }
    }
}

/// Render a [`neenee_core::ScopeTarget`] as the stable string used to key and
/// display a permission rule. A path becomes the path string; a command becomes
/// the command string; [`ScopeTarget::Unspecified`] becomes `"*"` (the legacy
/// "any scope" sentinel), so tools without a locatable target are ruled as
/// before. This string is purely a dedup key + UI label — the actual scope
/// admission decision is made by [`neenee_core::OperationScope::allows`].
#[allow(dead_code)]
fn scope_target_to_rule(target: &neenee_core::ScopeTarget) -> String {
    match target {
        neenee_core::ScopeTarget::Path(p) => p.to_string_lossy().into_owned(),
        neenee_core::ScopeTarget::Command(c) => c.clone(),
        neenee_core::ScopeTarget::Unspecified => "*".to_string(),
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
/// by the provider at `neenee_core::provider=debug`.
fn empty_response_error(response: &Message) -> HarnessError {
    tracing::warn!(
        target: "neenee_core::agent",
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

#[cfg(test)]
mod tests {
    use super::{RoundState, ScopedToolDisable, checkpoint_tool_signature, envoy_result_text};

    fn tool_call(id: &str, arguments: &str) -> neenee_core::ToolCall {
        neenee_core::ToolCall {
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

    /// The successful envoy result carries the `[<tool> result]:` header, the
    /// original summary verbatim, and the success re-anchor note.
    #[test]
    fn envoy_result_text_reanchors_on_success() {
        let text = envoy_result_text("envoy", "Found the symbol in lib.rs", false);
        assert!(
            text.starts_with("[envoy result]:\n"),
            "header present: {text}"
        );
        assert!(
            text.contains("Found the symbol in lib.rs"),
            "summary preserved verbatim: {text}"
        );
        // The anchor must pin the principal's write capability back to the
        // principal and call out the read-only scope as envoy-only.
        assert!(
            text.contains("applies to the envoy only"),
            "anchor scope pin missing: {text}"
        );
        assert!(
            text.contains("retain your full toolset"),
            "principal re-anchor missing: {text}"
        );
    }

    /// A failed envoy carries a different (re-delegate-or-act-directly) anchor,
    /// and still preserves the partial summary for the principal to act on.
    #[test]
    fn envoy_result_text_reanchors_on_failure() {
        let text = envoy_result_text("envoy", "partial findings before crash", true);
        assert!(
            text.contains("partial findings before crash"),
            "partial summary preserved: {text}"
        );
        assert!(
            text.contains("could not complete its sub-task"),
            "failure anchor missing: {text}"
        );
        // Both anchors must re-affirm the principal retains write capability.
        assert!(
            text.contains("retain your full toolset"),
            "principal re-anchor missing on failure: {text}"
        );
        // And must NOT carry the success-only phrasing (regression guard against
        // the success anchor leaking onto a failed envoy).
        assert!(
            !text.contains("applies to the envoy only"),
            "success anchor leaked onto failure: {text}"
        );
    }

    /// The re-anchor is unconditional for any envoy result — a regression guard
    /// that a future refactor cannot silently drop it.
    #[test]
    fn envoy_result_text_anchor_is_unconditional() {
        for failed in [false, true] {
            let text = envoy_result_text("envoy", "x", failed);
            assert!(
                text.contains("[system]"),
                "system anchor tag present (failed={failed}): {text}"
            );
        }
    }

    use neenee_core::RestorePoint;

    /// A scoped disable hides the tool until its restore point fires.
    #[test]
    fn scoped_disable_hides_until_restore() {
        let mut scoped = ScopedToolDisable::default();
        assert!(!scoped.contains("bash"));
        scoped.disable("bash", RestorePoint::TurnEnd);
        assert!(scoped.contains("bash"));
        scoped.restore_turn_end();
        assert!(
            !scoped.contains("bash"),
            "TurnEnd restore must re-enable the tool"
        );
        assert!(scoped.is_empty(), "both buckets drained");
    }

    /// `TurnEnd` restore clears the turn-scoped bucket only; `RoundEnd`
    /// disables survive until the user-round boundary.
    #[test]
    fn turn_end_restore_keeps_round_end_disables() {
        let mut scoped = ScopedToolDisable::default();
        scoped.disable("bash", RestorePoint::TurnEnd);
        scoped.disable("edit_file", RestorePoint::RoundEnd);
        scoped.restore_turn_end();
        assert!(
            !scoped.contains("bash"),
            "TurnEnd disable must be restored at the ReAct-turn boundary"
        );
        assert!(
            scoped.contains("edit_file"),
            "RoundEnd disable must survive the ReAct-turn boundary"
        );
    }

    /// Nested disables compose via refcount: two hooks disable `bash` at
    /// different restore points; the earlier (TurnEnd) restore must NOT bring
    /// it back while the later (RoundEnd) is still in effect.
    #[test]
    fn nested_disables_refcount_correctly() {
        let mut scoped = ScopedToolDisable::default();
        scoped.disable("bash", RestorePoint::RoundEnd);
        scoped.disable("bash", RestorePoint::TurnEnd);
        assert!(scoped.contains("bash"));
        scoped.restore_turn_end();
        assert!(
            scoped.contains("bash"),
            "bash still hidden: the RoundEnd disable outlives the TurnEnd restore"
        );
        scoped.restore_round_end();
        assert!(!scoped.contains("bash"), "bash back after round end");
    }
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

    fn apply_scoped_disables(&self, disables: &[(String, neenee_core::RestorePoint)]) {
        // Delegate to the existing agent method (same signature).
        Agent::apply_scoped_disables(self, disables);
    }

    async fn check_bash_policy(
        &self,
        command: &str,
        _arguments: &str,
    ) -> Option<neenee_core::ToolOutput> {
        // Non-interactive resolution only: Deny outright, or a Confirm that
        // resolves under unattended. The interactive Confirm path (with its
        // event-channel park) stays in execute_tool's full check_bash_policy.
        let policy = self
            .bash_policy
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let decision = policy.evaluate(command)?;
        match decision.action {
            crate::bash_policy::BashPolicyAction::Deny => Some(neenee_core::ToolOutput::Error {
                message: format!("[bash policy] Blocked dangerous command: {command}"),
                detail: Some(format!(
                    "Rule: {}{}\nReason: {}\nThis command was not executed.",
                    decision.name,
                    if decision.builtin { " (built-in)" } else { "" },
                    decision.reason,
                )),
            }),
            crate::bash_policy::BashPolicyAction::Confirm => {
                // Only the unattended resolution belongs here; non-unattended
                // Confirm needs the event channel, handled in execute_tool.
                if self.get_unattended() {
                    match policy.unattended_confirm_action() {
                        crate::bash_policy::BashPolicyAction::Allow => None,
                        _ => Some(neenee_core::ToolOutput::Error {
                            message: format!(
                                "[bash policy] Dangerous command requires confirmation but session is unattended: {command}"
                            ),
                            detail: Some(format!(
                                "Rule: {}{}\nReason: {}\nThis command was not executed.",
                                decision.name,
                                if decision.builtin { " (built-in)" } else { "" },
                                decision.reason,
                            )),
                        }),
                    }
                } else {
                    // Needs interactive confirm: signal "no decision here" so
                    // execute_tool runs the full check_bash_policy.
                    None
                }
            }
            crate::bash_policy::BashPolicyAction::Allow => None,
        }
    }

    fn permissions(&self) -> &crate::permission_store::PermissionStore {
        &self.permissions
    }

    fn unattended(&self) -> bool {
        self.get_unattended()
    }
}
