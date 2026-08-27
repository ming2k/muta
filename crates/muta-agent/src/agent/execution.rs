//! Tool-execution tail of [`Agent`] rounds: output normalization, image
//! plumbing, and the PostToolUse/PostToolUseFailure hook fire points.

use super::*;

impl Agent {
    /// Fire PostToolUse (success) or PostToolUseFailure (error) hooks and append
    /// any injected context as hidden user messages (ADR-0025). No-op when the
    /// registry is empty, which is the common case (runners, tests, no
    /// `[hooks]` config).
    pub(crate) async fn run_post_tool_hooks(
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
    /// `read_text`, `search_text`). Used to classify a turn as read-only for the
    /// turn-hook streak counter. An unknown tool name reads as `true`
    /// (unspecified), matching the trait default.
    pub(crate) fn tool_target_is_unspecified(&self, name: &str, arguments: &str) -> bool {
        match self
            .resolved_tools
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|t| t.name() == name)
        {
            Some(t) => matches!(
                t.scope_target(arguments),
                muta_contracts::ScopeTarget::Unspecified
            ),
            None => true,
        }
    }

    /// Fire user-configured `Turn` hooks at the turn boundary and fold any
    /// `Inject` context into hidden user messages. `Deny` is already discarded
    /// by [`HookRegistry::run_turn`], so a turn hook cannot abort the round.
    /// `ScopeTools` disables are applied to the scoped mask.
    pub(super) async fn run_turn_hooks(
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
    pub(super) async fn run_turn_start_hooks(
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
    async fn fire_permission_request_hooks(&self, request: &muta_contracts::PermissionRequest) {
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
    async fn fire_user_question_hooks(&self, request: &muta_contracts::UserQuestionRequest) {
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
    pub(super) fn check_hard_stop(&self, turns: usize) -> std::ops::ControlFlow<()> {
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
    pub(super) fn hard_stop_error(&self) -> HarnessError {
        let budget = self.get_hard_stop_turns();
        HarnessError::Other(format!(
            "Agent stopped: the configured hard-stop budget of {budget} ReAct \
             turns was reached. This budget is opt-in (`hard_stop_turns`); \
             raise it or set it to 0 (the default) for an uncapped round."
        ))
    }

    /// Emit a [`AgentEvent::TodosUpdated`] snapshot whenever a tool mutates
    /// the task list (`todo` full-replace or `todo_update` surgical edit).
    /// The TUI stores the snapshot and re-renders the sticky panel above the
    /// input box.
    pub(super) fn emit_todos_change<F>(&self, call: &ToolCall, on_event: &mut F)
    where
        F: FnMut(AgentEvent) + Send,
    {
        if matches!(
            call.name.as_str(),
            "write_todos" | "update_todo" | "todo" | "todo_update"
        ) {
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
            origin: None,
        };

        // ADR-0141 posture gate: an ask_user call only parks when a human
        // channel exists. Autonomous sessions (headless no-TTY, CI, runners
        // with `allow_user_interaction: false`) never fabricate a user —
        // they settle by the configured fallback policy, labeled as such.
        let posture = self.human_posture();
        if posture == muta_contracts::human_request::HumanChannelPosture::Autonomous {
            return match self.autonomous_fallback_policy() {
                muta_contracts::human_request::AutonomousFallbackPolicy::FailClosed => {
                    self.human_broker
                        .metrics_note_refused(HumanRequestKind::Question);
                    ToolOutput::Text(
                        "ask_user is unavailable: no human channel is attached to this \
                         session, so nobody can answer. Resolve the ambiguity yourself — \
                         choose the safest option, state the assumption in your reply, and \
                         continue. Do not call ask_user again this turn."
                            .to_string(),
                    )
                }
                muta_contracts::human_request::AutonomousFallbackPolicy::RecommendedLabeled => {
                    // Take each question's first option — the schema's
                    // "recommended" convention — and label the source so the
                    // model can never mistake it for a human decision.
                    let answers: Vec<Vec<String>> = request
                        .questions
                        .iter()
                        .map(|q| {
                            q.options
                                .first()
                                .map(|opt| vec![opt.label.clone()])
                                .unwrap_or_default()
                        })
                        .collect();
                    let reply = UserQuestionReply {
                        request_id: request.id.clone(),
                        answers,
                    };
                    let settled = self.human_broker.settle_by_policy_owned(
                        request.id.clone(),
                        HumanReply::Question(Some(reply.clone())),
                        muta_contracts::human_request::AutonomousFallbackPolicy::RecommendedLabeled,
                    );
                    debug_assert!(settled, "policy settlement on a fresh request must succeed");
                    let _ = settled;
                    let output = serde_json::to_string_pretty(&reply.answers)
                        .unwrap_or_else(|_| format!("{:?}", reply.answers));
                    ToolOutput::Text(format!(
                        "[answered by policy, not by user] No human channel is attached. \
                         Each question was answered with its first (recommended) option \
                         per the session's autonomous fallback policy:\n{}",
                        output
                    ))
                }
            };
        }

        let receiver = self
            .human_broker
            .park(request.id.clone(), HumanRequestKind::Question);
        tracing::info!(questions = request.questions.len(), "asking user");
        let _ = event_tx.send(AgentEvent::UserQuestionRequest(request.clone()));
        // Observe-only interrupt hook: fire notifications (desktop/bell) so the
        // user notices the agent is blocked on their input. No-op without
        // `[hooks]`. Outcomes are ignored — this never gates the question.
        let parked_at = std::time::Instant::now();
        self.fire_user_question_hooks(&request).await;

        let settled = receiver.await.ok().map(|s| s.reply);
        let reply = match settled {
            Some(HumanReply::Question(reply)) => reply,
            // Channel closed without a settlement (agent teardown) or a
            // mismatched kind (harness bug): treat as cancel.
            _ => None,
        };
        // Charge the human-thinking pause to the round so the exit gate can
        // subtract it for an honest tokens/sec.
        self.book_pause(parked_at.elapsed().as_millis() as u64);
        match reply {
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
        // ADR-0141 posture gate: with no human channel there is nobody to
        // type into the panel. Do not park — run the command with closed
        // stdin, exactly as if the operator had dismissed the prompt (the
        // caller's non-interactive remedy path).
        if self.human_posture() == HumanChannelPosture::Autonomous {
            self.human_broker
                .metrics_note_refused(HumanRequestKind::Stdin);
            tracing::info!("autonomous posture: interactive stdin refused, running closed");
            return None;
        }
        let receiver = self
            .human_broker
            .park(request.id.clone(), HumanRequestKind::Stdin);
        tracing::info!(%secret, "requesting operator input for interactive command");
        let _ = event_tx.send(AgentEvent::StdinRequest(request.clone()));
        let settled = receiver.await.ok().map(|s| s.reply);
        match settled {
            Some(HumanReply::Stdin(Some(reply))) if !reply.text.is_empty() => {
                Some(StdinPolicy::Prefilled { data: reply.text })
            }
            _ => None,
        }
    }

    /// Three-way stdin policy for a `bash` call (L3 + L3.5). See the decision
    /// block in [`Self::execute_tool`] for the contract. `arguments` is the
    /// raw JSON tool arguments.
    pub(super) async fn decide_command_stdin(
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
            // YOLO mode, or the operator has opted out of the interactive
            // input panel: no one is going to type into the prompt, so the
            // inline panel would either deadlock or just disrupt.
            // Close stdin instead — the command then fails fast with a non-interactive remedy.
            if self.get_yolo() {
                tracing::info!(command = %command, "interactive command stdin closed under yolo mode");
                return StdinPolicy::default();
            }
            if self.skip_interactive_input() {
                tracing::info!(command = %command, "interactive command stdin closed by skip_interactive_input");
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

    pub(crate) async fn execute_tool(
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
        // validation, operation-scope gate, bash policy, and the broker's
        // explicit-grant/development fast paths — runs
        // as one chain evaluation (see `permission_policy`). The chain is
        // async because some gates await (hooks, bash policy). Outcomes:
        //   • Deny    → short-circuit with the policy's output.
        //   • Approve → proceed under existing authority.
        //   • MissingAuthority → attended: ask once; autopilot: fail now.
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
            operation_scope,
            disabled: disabled_snapshot,
            scoped_disabled: scoped_snapshot,
            yolo: self.get_yolo(),
            ctx: self, // Agent: PermissionContext
        };

        match self.permission_chain().evaluate(&pctx).await {
            crate::permission_policy::PolicyDecision::Pass
            | crate::permission_policy::PolicyDecision::Approve => {}
            crate::permission_policy::PolicyDecision::Deny { output, .. } => {
                return output;
            }
            crate::permission_policy::PolicyDecision::MissingAuthority { request, rule } => {
                if self.get_yolo() {
                    // Under YOLO mode, missing authority is auto-approved.
                } else {
                    // The single interactive-park path. Both the broker (a
                    // write/execute the user must approve) and the bash
                    // dangerous-command confirm reach here; `request.one_off`
                    // distinguishes them. Fill the request id, emit, fire
                    // observe hooks, await the user's decision.
                    let one_off = request.one_off;
                    let request = muta_contracts::PermissionRequest {
                        id: format!("permission_{}", uuid::Uuid::new_v4()),
                        ..request
                    };
                    // ADR-0141 posture gate: permissions fail closed when no
                    // human channel exists — a missing human cannot grant
                    // authority. (Autopilot sessions reach this arm only with
                    // an interactive watcher attached, so this is the belt to
                    // that braces.)
                    if self.human_posture() == HumanChannelPosture::Autonomous {
                        self.human_broker
                            .metrics_note_refused(HumanRequestKind::Permission);
                        tracing::warn!(
                            tool = %request.tool,
                            "autonomous posture: permission refused (fail closed)"
                        );
                        return permission_required_output(&request);
                    }
                    let receiver = self
                        .human_broker
                        .park(request.id.clone(), HumanRequestKind::Permission);
                    let parked_at = std::time::Instant::now();
                    tracing::info!(tool = %request.tool, scope = %request.scope, one_off, "permission requested");
                    let _ = event_tx.send(AgentEvent::PermissionRequest(request.clone()));
                    self.fire_permission_request_hooks(&request).await;
                    let decision = match receiver.await.ok().map(|s| s.reply) {
                        Some(HumanReply::Permission(decision)) => decision,
                        _ => PermissionDecision::Reject,
                    };
                    // Charge the human-thinking pause to the round so the exit
                    // gate can subtract it for an honest tokens/sec.
                    self.book_pause(parked_at.elapsed().as_millis() as u64);
                    match decision {
                        PermissionDecision::Once => {
                            tracing::info!(tool = %tool.name(), decision = "once", "permission granted for single invocation");
                        }
                        PermissionDecision::Session => {
                            tracing::info!(tool = %tool.name(), decision = "session", "permission granted for current session");
                            self.permissions.add_session(rule);
                        }
                        PermissionDecision::Always => {
                            if one_off {
                                // A bash dangerous-command confirm: honour the
                                // grant for this one call but do NOT persist it.
                                // A dangerous-command confirmation is sharper
                                // than ordinary tool permission and must stay
                                // one-off unless the user writes an explicit
                                // `[bash_policy.rules] action = "allow"` override.
                                tracing::info!(
                                    tool = %tool.name(),
                                    decision = "always",
                                    "one-off permission granted (not persisted)"
                                );
                            } else {
                                tracing::info!(tool = %tool.name(), decision = "always", "permission granted permanently for workspace");
                                self.permissions.add_always(rule);
                            }
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
        }

        if call.name == "ask_user" {
            if self.get_yolo() {
                return ToolOutput::Text(
                    "ask_user is unavailable: this session is running in Delegated mode and no human \
                     is reachable to answer. Resolve the ambiguity yourself — pick the most \
                     reasonable default and proceed."
                        .to_string(),
                );
            }
            return self.execute_ask_user(call, call_id, event_tx).await;
        }

        // ── Stdin policy decision (L3 + L3.5) ──
        // Decided here, before spawn, for execute_command only. The three-way decision:
        //   1. opt-in model stdin (α): `allow_model_stdin` on AND the model
        //      supplied a `stdin` arg → Prefilled{model}. Structurally
        //      unreachable unless the flag exposed the schema field.
        //   2. human input (β, default): the interactive classifier matched →
        //      ask the operator; Prefilled{human} or Closed (if cancelled).
        //   3. closed (default hard floor): everything else.
        // For other tools, Closed is always correct (they ignore stdin).
        let stdin_policy = if call.name == "execute_command" {
            self.decide_command_stdin(&call.arguments, event_tx).await
        } else {
            StdinPolicy::default()
        };

        // The Runner / ToolStream events must carry the same id as the
        // up-front ToolCall event (the dispatch-generated `call_id`), not the
        // model's `call.id` — the UI keys its step off the ToolCall event id,
        // so using `call.id` here would orphan every runner child stream and
        // every live tool stream, leaving the runner view empty.
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
                    let _ = event_tx.send(AgentEvent::Runner {
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
    /// Cancellation-aware: if `cancel` fires while the tool is in flight, a
    /// cooperatively-cancellable tool (an runner) is given a bounded grace
    /// period to drain and return its terminal result — the interrupted
    /// result is carried in [`SingleToolOutcome`] so the caller can record the
    /// partial work before ending the round. A non-cancellable tool keeps the
    /// historical fast path: its in-flight call is paired with a terminal
    /// [`AgentEvent::ToolCancelled`] and the outcome reports no result.
    pub(crate) async fn execute_tool_evented<F>(
        &self,
        call: &ToolCall,
        call_id: &str,
        cancel: &CancellationToken,
        on_event: &mut F,
    ) -> Result<SingleToolOutcome, HarnessError>
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
                    let cancellable = self
                        .tool_manager
                        .find(&call.name)
                        .is_some_and(|sourced| sourced.tool.supports_cooperative_cancel());
                    if !cancellable {
                        while let Ok(event) = rx.try_recv() {
                            on_event(event);
                        }
                        on_event(AgentEvent::ToolCancelled {
                            id: call_id.to_string(),
                            name: call.name.clone(),
                        });
                        return Ok(SingleToolOutcome {
                            result: None,
                            interrupted: true,
                        });
                    }
                    // Cooperative drain: signal the tool, then race its
                    // future against a bounded grace period. The runner stops
                    // at its next safe boundary and returns its partial
                    // transcript as a terminal result.
                    if let Some(sourced) = self.tool_manager.find(&call.name) {
                        sourced.tool.request_cancel(call_id);
                    }
                    let grace = tokio::time::sleep(RUNNER_DRAIN_GRACE);
                    tokio::pin!(grace);
                    loop {
                        tokio::select! {
                            biased;
                            _ = &mut grace => {
                                while let Ok(event) = rx.try_recv() {
                                    on_event(event);
                                }
                                on_event(AgentEvent::ToolCancelled {
                                    id: call_id.to_string(),
                                    name: call.name.clone(),
                                });
                                return Ok(SingleToolOutcome {
                                    result: None,
                                    interrupted: true,
                                });
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
                                return Ok(SingleToolOutcome {
                                    result: Some(result),
                                    interrupted: true,
                                });
                            }
                        }
                    }
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
                    return Ok(SingleToolOutcome {
                        result: Some(result),
                        interrupted: false,
                    });
                }
            }
        }
    }

    /// Resolve `call`'s tool (resolved → dynamic fallback) and return its
    /// declared [`ToolAccesses`]. Used by the scheduler to arbitrate which
    /// calls of a batch may run concurrently. A tool that can't be resolved
    /// yields [`ToolAccesses::none`] (freely parallel) — it will report its
    /// own "not found" error inside `execute_tool`; there's no point
    /// serializing an error.
    pub(crate) fn accesses_for_call(&self, call: &ToolCall) -> muta_contracts::ToolAccesses {
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
            None => muta_contracts::ToolAccesses::none(),
        }
    }
}
