//! Steering surface of [`Agent`]: round counters, pause accounting,
//! interrupts, turn/interaction gates, and the pending-message queue.

use super::*;

impl Agent {
    /// Current harness round counter — bumped at the start of every
    /// `execute_round`. Used by the TUI to detect a stale task panel (one
    /// whose `updated_at_round` lags the current round by more than
    /// `TODO_STALE_TURN_THRESHOLD`).
    pub fn round_count(&self) -> u64 {
        *self.round_counter.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Human-decision pause time accumulated by the current round, in
    /// milliseconds. Captured into a `/retry` resume point when a round
    /// stops, and seeded back on resume, so tokens/sec stays honest across
    /// the stop.
    pub fn round_paused_ms(&self) -> u64 {
        self.round_paused_ms
            .load(std::sync::atomic::Ordering::Relaxed)
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

    pub fn unattended(&self) -> bool {
        self.permissions.unattended()
    }

    pub fn is_confined(&self) -> bool {
        self.confinement.is_confined()
    }

    pub fn set_confined(&self, confined: bool) {
        self.confinement.set_confined(confined);
    }

    pub fn shared_confinement(&self) -> muta_contracts::SharedConfinement {
        self.confinement.clone()
    }

    pub fn bind_shared_confinement(&mut self, confinement: muta_contracts::SharedConfinement) {
        self.confinement = confinement;
    }

    /// ADR-0141: whether a human can currently answer this agent's parked
    /// requests. Interactive posture means yes; autonomous means no human
    /// is reachable and parked requests settle by labeled policy. With a
    /// channel accountant bound (hosted sessions), this is the live OR over
    /// attached clients; otherwise the static declared posture.
    pub fn human_posture(&self) -> muta_contracts::human_request::HumanChannelPosture {
        self.interaction.human_posture()
    }

    /// ADR-0141: bind a live channel source (hosted sessions). The agent's
    /// posture then tracks attached clients instead of a static flag.
    pub fn set_human_channel_accountant(
        &self,
        accountant: std::sync::Arc<muta_contracts::human_request::HumanChannelAccountant>,
    ) {
        self.interaction.set_human_channel(Some(accountant));
    }

    /// ADR-0141: how an autonomous session settles a parked question.
    /// Sourced from `[master] ask_user_fallback` config; defaults to
    /// fail-closed (a missing human is an error, not an opinion).
    pub fn autonomous_fallback_policy(&self) -> AutonomousFallbackPolicy {
        self.interaction.autonomous_fallback_policy()
    }

    /// ADR-0141: configure the autonomous fallback policy (see
    /// [`Self::autonomous_fallback_policy`]).
    pub fn set_autonomous_fallback_policy(&self, policy: AutonomousFallbackPolicy) {
        self.interaction.set_autonomous_fallback_policy(policy);
    }

    /// ADR-0141: declare this agent's human-channel posture. Attaching
    /// clients' declarations are OR-ed at the session level; subagents inherit
    /// their parent's posture at spawn.
    pub fn set_human_posture(&self, posture: muta_contracts::human_request::HumanChannelPosture) {
        self.interaction.set_human_posture(posture);
    }

    pub fn set_unattended(&self, enabled: bool) {
        self.permissions.set_unattended(enabled);
        self.interaction.set_unattended(enabled);
    }

    pub fn set_workspace_security(&self, snapshot: muta_contracts::WorkspaceSecuritySnapshot) {
        *self
            .workspace_security
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = snapshot;
    }

    pub fn set_project_rules(&self, rules: impl Into<String>) {
        if let Ok(mut current) = self.project_rules.write() {
            *current = rules.into();
        }
    }

    pub fn workspace_security(&self) -> muta_contracts::WorkspaceSecuritySnapshot {
        self.workspace_security
            .lock()
            .map(|snapshot| snapshot.clone())
            .unwrap_or_default()
    }

    pub fn workspace_security_handle(
        &self,
    ) -> Arc<std::sync::Mutex<muta_contracts::WorkspaceSecuritySnapshot>> {
        Arc::clone(&self.workspace_security)
    }

    /// Bind a child agent to the parent's live workspace authority cell. This
    /// is intentionally a construction-time operation: once an Agent is shared
    /// through `Arc`, its authority master cannot be swapped.
    pub fn bind_workspace_security_handle(
        &mut self,
        handle: Arc<std::sync::Mutex<muta_contracts::WorkspaceSecuritySnapshot>>,
    ) {
        self.workspace_security = handle;
    }

    /// Set this agent's operation boundary (ADR-0028). The main agent leaves it
    /// unrestricted; `SubagentTool` sets the scope resolved from the bound
    /// subagent profile on the child before it runs.
    pub fn set_operation_scope(&self, scope: muta_contracts::OperationScope) {
        *self
            .operation_scope
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = scope;
    }

    /// Apply a declarative agent preset (ADR-0053) — set every knob an
    /// [`muta_contracts::AgentPreset`] declares in one call.
    ///
    /// Sets: the capability scope ([`Self::set_agent_selection`]), the
    /// write/command boundary ([`Self::set_operation_scope`]), and the runtime
    /// execution knobs (`hard_stop` / doom guard / model-stdin /
    /// attended flag). The preset's [`muta_contracts::AgentIdentity`] is **not**
    /// re-applied here — identity is immutable past construction (it feeds the
    /// system-prompt preamble), so the embedding supplies it to `Agent::new` /
    /// `from_toolset`. A role whose identity should differ per instance composes
    /// [`muta_contracts::AgentPreset::with_identity`] before construction.
    ///
    /// The archetype / kind of this agent, derived purely from its [`ExecutionPolicy`] (ADR-0183).
    pub fn kind(&self) -> muta_contracts::AgentKind {
        if self.is_root() {
            muta_contracts::AgentKind::Root
        } else {
            muta_contracts::AgentKind::Subagent
        }
    }

    /// Set this agent's archetype posture. Updates [`ExecutionPolicy`] accordingly.
    pub fn set_kind(&self, kind: muta_contracts::AgentKind) {
        let mut policy = self.execution_policy();
        match kind {
            muta_contracts::AgentKind::Root => {
                policy.depth = 0;
                policy.allow_human_interaction = true;
                policy.lifecycle = muta_contracts::ContextLifecycle::DurableSession;
            }
            muta_contracts::AgentKind::Subagent => {
                if policy.depth == 0 {
                    policy.depth = 1;
                }
                policy.allow_human_interaction = false;
                policy.lifecycle = muta_contracts::ContextLifecycle::EphemeralScratchpad;
            }
        }
        self.set_execution_policy(policy);
    }

    /// Execution policy governing this agent's runtime posture, delegation limits, and depth (ADR-0183).
    pub fn execution_policy(&self) -> muta_contracts::ExecutionPolicy {
        self.execution_policy
            .read()
            .map(|p| p.clone())
            .unwrap_or_else(|_| muta_contracts::ExecutionPolicy::root_default())
    }

    /// Set this agent's execution policy.
    pub fn set_execution_policy(&self, policy: muta_contracts::ExecutionPolicy) {
        if let Ok(mut guard) = self.execution_policy.write() {
            *guard = policy;
        }
    }

    /// Whether this agent is the root node of the delegation tree.
    pub fn is_root(&self) -> bool {
        self.execution_policy().is_root()
    }

    /// Whether this agent is permitted to spawn sub-agents.
    pub fn can_spawn_subagent(&self) -> bool {
        self.execution_policy().can_spawn_subagent()
    }

    /// Shared handle to the agent's tool pool.
    pub fn tool_pool(&self) -> Arc<std::sync::RwLock<muta_contracts::ToolPool>> {
        self.pool.clone()
    }

    /// Record an agent's requirement declaration against the pool.
    pub fn declare_tools(&self, declaration: &muta_contracts::ToolDeclaration) {
        if let Ok(pool_guard) = self.pool.read() {
            pool_guard.declare(declaration.clone());
        }
    }

    /// Apply an agent delegation policy (ADR-0183) to adjust declared tool availability.
    pub fn apply_delegation(&self, delegation: &muta_contracts::DelegationPolicy) {
        self.set_agent_selection(delegation.selection());
    }

    /// Apply a declarative agent preset (ADR-0183).
    pub fn apply_preset(&self, preset: &muta_contracts::AgentPreset) {
        self.apply_profile(preset);
    }

    /// Idempotent over defaults: applies an [`muta_contracts::AgentRole`] (ADR-0183 / ADR-0211).
    pub fn apply_profile(&self, profile: &muta_contracts::AgentRole) {
        self.set_identity(profile.identity.clone());
        self.set_agent_selection(profile.agent_selection.clone());
        self.set_operation_scope(profile.operation_scope.clone());
        self.set_hard_stop_turns(profile.config.hard_stop_turns);
        self.set_doom_guard_config(profile.config.nudge);
        self.set_allow_model_stdin(profile.config.allow_model_stdin);
        self.set_skip_interactive_input(profile.config.skip_interactive_input);
        self.set_unattended(profile.unattended);
        if !profile.facets.is_empty() {
            *self.facets.write().unwrap_or_else(|e| e.into_inner()) = profile.facets.clone();
        }
    }

    /// Replace this agent's identity (name + mission, or a persona override).
    pub fn set_identity(&self, identity: AgentIdentity) {
        if let Ok(mut guard) = self.identity.write() {
            *guard = identity;
        }
    }

    /// Switch the live agent into a named role (ADR-0183 / ADR-0211).
    pub fn apply_role(&self, role: &str) -> Option<muta_contracts::AgentRoleId> {
        let resolved = muta_contracts::AgentRoleId::parse(role)?;
        let base = self.identity();
        let mut profile = muta_contracts::AgentRole::for_role(resolved, &base);

        // ADR-0211: Staff the role with appropriate instance-bound HarnessFacets
        match resolved {
            muta_contracts::AgentRoleId::Code => {
                profile
                    .facets
                    .push(Arc::new(crate::facet::CodeIntelligenceFacet::new(1024)));
            }
            muta_contracts::AgentRoleId::CodeAnalyst
            | muta_contracts::AgentRoleId::Reviewer
            | muta_contracts::AgentRoleId::Architect => {
                profile
                    .facets
                    .push(Arc::new(crate::facet::CodeIntelligenceFacet::read_only(
                        1024,
                    )));
            }
            muta_contracts::AgentRoleId::Security => {}
        }
        self.apply_profile(&profile);
        *self.facets.write().unwrap_or_else(|e| e.into_inner()) = profile.facets;

        Some(resolved)
    }

    /// Snapshot of this agent's operation boundary. Used by the `execute_tool`
    /// funnel to gate tools whose target falls outside the granted scope.
    pub(super) fn operation_scope(&self) -> muta_contracts::OperationScope {
        self.operation_scope
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_else(|_| muta_contracts::OperationScope::unrestricted())
    }

    /// A snapshot of this agent's identity (name + mission, or a persona
    /// override). Feeds the system-prompt preamble. Returns a clone because
    /// identity is now live-mutable behind a lock (so `/master` can switch
    /// personas); callers that need a stable view across an await should take
    /// the snapshot once. Lets an embedding reuse the primary's identity (e.g.
    /// a `/btw` side session) instead of recomposing it.
    pub fn identity(&self) -> AgentIdentity {
        self.identity
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// The round-end gate (ADR-0025). A `Stop` hook may force another turn with
    /// feedback. Returns `None` to let the round end — i.e. every Stop hook
    /// must agree to stop. Returns the prompt together with the
    /// [`InjectionKind`] that produced it, so the push site stamps the correct
    /// provenance instead of guessing from the text.
    pub(super) async fn stop_gate(&self, response: &Message) -> Option<(String, InjectionKind)> {
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
    /// Add `ms` to the current round's accumulated human-decision pause time
    /// (a permission prompt or `ask_user`). Called around every blocking
    /// `receiver.await` so the round exit gate can subtract it from the
    /// wall-clock for an honest tokens/sec. No-op after saturating at `u64::MAX`.
    pub(super) fn book_pause(&self, ms: u64) {
        self.round_paused_ms
            .fetch_add(ms, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn thread_id(&self) -> Option<String> {
        self.thread_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn reply_permission(&self, request_id: &str, decision: PermissionDecision) -> bool {
        // ADR-0141: settles through the broker — wire replies carry
        // provenance-User by construction.
        self.human_broker
            .reply_user(request_id, HumanReply::Permission(decision))
    }

    pub fn reject_pending_permissions(&self) {
        // ADR-0141: teardown cancels route through the broker for uniform
        // metrics and exactly-once settlement.
        self.human_broker.cancel_kind(HumanRequestKind::Permission);
    }

    /// Resolve a parked `ask_user` request. An empty outer vector means the
    /// operator cancelled; answered questions remain distinguishable because
    /// they carry one inner vector per question (which may itself be empty).
    /// ADR-0141: settles through the human-request broker — wire replies are
    /// `ReplyProvenance::User` by construction.
    pub fn reply_user_question(&self, request_id: &str, answers: Vec<Vec<String>>) -> bool {
        let reply = if answers.is_empty() {
            None
        } else {
            Some(UserQuestionReply {
                request_id: request_id.to_string(),
                answers,
            })
        };
        self.human_broker
            .reply_user(request_id, HumanReply::Question(reply))
    }

    pub fn reject_pending_user_questions(&self) {
        // ADR-0141: question cancels route through the broker so metrics and
        // exactly-once settlement stay uniform across the three protocols.
        // Only questions are drained here; permission/input teardowns have
        // their own callers. `false` means nothing was parked.
        self.human_broker.cancel_kind(HumanRequestKind::Question);
    }

    /// Resolve a parked interactive-input request (L3.5 β) with the operator's
    /// text, unblocking the `bash` dispatch that issued it. Returns `false` if
    /// no matching request is parked (e.g. already resolved or cancelled).
    /// An empty `text` is a valid "cancel" — the command then runs with
    /// closed stdin and fails fast.
    pub fn reply_input(&self, request_id: &str, text: String) -> bool {
        // ADR-0141: settles via the broker; wire replies are provenance-User.
        self.human_broker.reply_user(
            request_id,
            HumanReply::Stdin(Some(StdinReply {
                request_id: request_id.to_string(),
                text,
            })),
        )
    }

    /// Cancel every parked input request (e.g. on round end / interrupt),
    /// resolving each with `None` so the awaiting dispatch returns a
    /// cancelled result.
    pub fn reject_pending_inputs(&self) {
        // ADR-0141: routed through the broker for uniform metrics and
        // exactly-once settlement.
        self.human_broker.cancel_kind(HumanRequestKind::Stdin);
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

    /// Install (or reuse) the steering inbox and return a [`SubagentHandle`]
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
    pub fn install_inbox(self: &Arc<Self>) -> SubagentHandle {
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
        SubagentHandle {
            weak: Arc::downgrade(self),
            ops: tx,
        }
    }

    /// Submit a steering [`AgentOp`] without going through a handle. Equivalent
    /// to [`SubagentHandle::submit`] but usable when the caller already holds a
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

    /// Steering mode currently configured on this agent.
    pub fn steering_mode(&self) -> muta_contracts::QueueMode {
        *self.steering_mode.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Set the steering mode.
    pub fn set_steering_mode(&self, mode: muta_contracts::QueueMode) {
        *self
            .steering_mode
            .write()
            .unwrap_or_else(|e| e.into_inner()) = mode;
    }

    /// Follow-up mode currently configured on this agent.
    pub fn follow_up_mode(&self) -> muta_contracts::QueueMode {
        *self
            .follow_up_mode
            .read()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Set the follow-up mode.
    pub fn set_follow_up_mode(&self, mode: muta_contracts::QueueMode) {
        *self
            .follow_up_mode
            .write()
            .unwrap_or_else(|e| e.into_inner()) = mode;
    }

    /// Open fresh, cancellable steering and follow-up queues for one interactive round.
    /// Any stale entries are returned to the caller so it can surface them as
    /// unavailable instead of silently carrying them into a different round.
    pub fn begin_session_queues(
        &self,
        session_id: impl Into<String>,
        generation: u64,
    ) -> (
        Vec<muta_contracts::QueuedMessage>,
        Vec<muta_contracts::QueuedMessage>,
    ) {
        let steering_mode = self.steering_mode();
        let follow_up_mode = self.follow_up_mode();
        let previous = self
            .session_queues
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(SessionQueues {
                session_id: session_id.into(),
                generation,
                steering: PendingMessageQueue::new(steering_mode),
                follow_up: PendingMessageQueue::new(follow_up_mode),
            });
        if let Some(mut prev) = previous {
            (prev.steering.drain_all(), prev.follow_up.drain_all())
        } else {
            (Vec::new(), Vec::new())
        }
    }

    /// Queue human-authored steering input for the next safe turn boundary. Returns
    /// `false` once the round has atomically closed its admission gate.
    pub fn steer(&self, session_id: &str, input: muta_contracts::QueuedMessage) -> bool {
        let mut queues = self
            .session_queues
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(open) = queues.as_mut().filter(|q| q.session_id == session_id) else {
            return false;
        };
        open.steering.enqueue(input);
        true
    }

    /// Queue follow-up input to run when the agent finishes active work. Returns
    /// `false` once the round has atomically closed its admission gate.
    pub fn follow_up(&self, session_id: &str, input: muta_contracts::QueuedMessage) -> bool {
        let mut queues = self
            .session_queues
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(open) = queues.as_mut().filter(|q| q.session_id == session_id) else {
            return false;
        };
        open.follow_up.enqueue(input);
        true
    }

    /// Cancel a queued steer insert. Taking the same mutex as boundary admission
    /// makes the result definitive: `Some` means the input cannot be admitted;
    /// `None` means admission already won (or the id was unknown).
    pub fn cancel_steer(
        &self,
        session_id: &str,
        input_id: &str,
    ) -> Option<muta_contracts::QueuedMessage> {
        let mut queues = self
            .session_queues
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let open = queues.as_mut().filter(|q| q.session_id == session_id)?;
        open.steering.cancel(input_id)
    }

    /// Cancel a queued follow-up insert.
    pub fn cancel_follow_up(
        &self,
        session_id: &str,
        input_id: &str,
    ) -> Option<muta_contracts::QueuedMessage> {
        let mut queues = self
            .session_queues
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let open = queues.as_mut().filter(|q| q.session_id == session_id)?;
        open.follow_up.cancel(input_id)
    }

    /// Remove all queued steering messages for a session.
    pub fn clear_steering_queue(&self, session_id: &str) {
        let mut queues = self
            .session_queues
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(open) = queues.as_mut().filter(|q| q.session_id == session_id) {
            open.steering.clear();
        }
    }

    /// Remove all queued follow-up messages for a session.
    pub fn clear_follow_up_queue(&self, session_id: &str) {
        let mut queues = self
            .session_queues
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(open) = queues.as_mut().filter(|q| q.session_id == session_id) {
            open.follow_up.clear();
        }
    }

    /// Remove all queued messages for a session.
    pub fn clear_all_queues(&self, session_id: &str) {
        let mut queues = self
            .session_queues
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(open) = queues.as_mut().filter(|q| q.session_id == session_id) {
            open.steering.clear();
            open.follow_up.clear();
        }
    }

    /// Stop accepting inserts and return anything that never crossed a turn
    /// boundary. Used on interrupted/error/blocked terminal paths.
    pub fn close_session_queues(
        &self,
        generation: u64,
    ) -> (
        Vec<muta_contracts::QueuedMessage>,
        Vec<muta_contracts::QueuedMessage>,
    ) {
        let mut queues = self
            .session_queues
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if queues.as_ref().is_none_or(|q| q.generation != generation) {
            return (Vec::new(), Vec::new());
        }
        if let Some(mut open) = queues.take() {
            (open.steering.drain_all(), open.follow_up.drain_all())
        } else {
            (Vec::new(), Vec::new())
        }
    }

    pub(super) fn session_queue_generation(&self) -> Option<u64> {
        self.session_queues
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|q| q.generation)
    }

    /// Admit currently queued steering messages at a turn boundary.
    pub(super) fn drain_steering<F>(
        &self,
        generation: Option<u64>,
        messages: &mut Vec<Message>,
        on_event: &mut F,
    ) -> usize
    where
        F: FnMut(AgentEvent),
    {
        let inputs = {
            let mut queues = self
                .session_queues
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let Some(open) = queues.as_mut().filter(|q| Some(q.generation) == generation) else {
                return 0;
            };
            open.steering.drain()
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
            on_event(AgentEvent::SteerAdmitted(input));
        }
        admitted
    }

    /// Admit currently queued follow-up messages when the agent would otherwise stop.
    pub(super) fn drain_follow_up<F>(
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
            let mut queues = self
                .session_queues
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let Some(open) = queues.as_mut().filter(|q| Some(q.generation) == generation) else {
                return 0;
            };
            if open.follow_up.is_empty() {
                if close_if_empty {
                    *queues = None;
                }
                return 0;
            }
            open.follow_up.drain()
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
            on_event(AgentEvent::SteerAdmitted(input));
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
    pub(super) fn drain_inbox(
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
                AgentOp::Steer(text) => {
                    messages.push(crate::conversation_context::visible_user(
                        InjectionKind::SubagentSteer,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NoProvider;

    #[test]
    fn apply_role_switches_facets_appropriately() {
        let provider = Arc::new(NoProvider);
        let agent = Agent::new(
            provider,
            Vec::new(),
            crate::AgentIdentity::new("test", "test agent"),
        );

        // Applying developer role equips code intelligence facet
        assert!(agent.apply_role("developer").is_some());
        assert_eq!(agent.facets().len(), 1);
        assert_eq!(agent.facets()[0].name(), "code_intelligence");

        // Applying security role drops code intelligence facet (0 overhead)
        assert!(agent.apply_role("security").is_some());
        assert_eq!(agent.facets().len(), 0);

        // Applying code_analyst equips read-only code intelligence facet
        assert!(agent.apply_role("code_analyst").is_some());
        assert_eq!(agent.facets().len(), 1);
        assert_eq!(agent.facets()[0].name(), "code_intelligence");
    }
}
