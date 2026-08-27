//! The streaming ReAct round loop of [`Agent`] plus the dispatch pipeline
//! that fans tool calls out and collects their outputs.

use super::*;

impl Agent {
    #[tracing::instrument(skip_all, name = "round", fields(streaming = true))]
    pub async fn run_streaming_with_events<F>(
        self: &Arc<Self>,
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
        // Reset the human-decision pause accumulator for this round so the
        // tokens/sec derived at the exit gate excludes only *this* round's
        // permission/ask_user waits.
        self.round_paused_ms
            .store(0, std::sync::atomic::Ordering::Relaxed);
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
            session_queue_generation: self.session_queue_generation(),
        }
    }

    /// Resume a previously stopped round as *the same round* — the `/retry`
    /// path (ADR-0128). Unlike [`Self::begin_streaming_round`] this does not
    /// restart the round from scratch: it re-seeds the ReAct state from the
    /// durable [`RetryPoint`] captured when the round stopped, so the resumed
    /// execution
    ///
    /// - keeps numbering turns from `turns_committed` (the transcript's
    ///   `round N · turn M` sequence stays unbroken — turn M+1 follows the
    ///   committed M, never a duplicate),
    /// - re-arms the pause accumulator from `paused_ms` so a later
    ///   tokens/sec stays honest across the stop, and
    /// - never re-runs turn-start hooks or side-effecting tools for turns
    ///   that already completed: `pending_request` stays `None`, so the first
    ///   request is assembled fresh from the checkpointed history.
    ///
    /// The inbox receiver is taken here exactly as in `begin_streaming_round`
    /// — between rounds it is returned to the agent (`RoundState::Drop` puts
    /// it back), so a mid-resume steering insert still has a drain target.
    pub(crate) fn resume_streaming_round(
        &self,
        point: &muta_contracts::RetryPoint,
    ) -> StreamingRoundState {
        self.round_paused_ms
            .store(point.paused_ms, std::sync::atomic::Ordering::Relaxed);
        StreamingRoundState {
            state: RoundState {
                guards: RoundState::guards_default(self.doom_guard_config()),
                ..RoundState::default()
            },
            turn_index: point.turns_committed,
            inbox_rx: self
                .inbox_rx
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take(),
            started_at: std::time::Instant::now(),
            pending_request: None,
            session_queue_generation: self.session_queue_generation(),
        }
    }

    /// Run or resume a streaming round from its last provider-request boundary.
    ///
    /// A [`HarnessError::Retryable`] leaves `round` reusable. If the failed
    /// request followed completed tool calls, their messages and the complete
    /// per-round state are already present, so the next invocation sends the
    /// same pending provider request instead of executing those tools again.
    pub(crate) async fn resume_streaming_with_events<F>(
        self: &Arc<Self>,
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
                if self.drain_steering(round.session_queue_generation, messages, &mut on_event) > 0
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
            let request_estimate = Self::estimate_model_request(request);
            let request_projection = request_estimate.total_tokens;
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
            on_event(AgentEvent::ContextTokens(
                muta_contracts::ContextTokenSnapshot::from_estimate(
                    request_estimate,
                    muta_contracts::ContextTokenSource::Projection,
                ),
            ));
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
                    Ok(Err(error)) => {
                        let err_msg = error.to_string();
                        request_accounting.record_error(&err_msg);
                        return Err(HarnessError::from(error));
                    }
                    Err(_elapsed) => {
                        tracing::warn!(
                            timeout_secs = STREAM_IDLE_TIMEOUT.as_secs(),
                            "stream request timed out before any response"
                        );
                        let err_msg = format!(
                            "Provider did not start streaming within {} seconds.",
                            STREAM_IDLE_TIMEOUT.as_secs()
                        );
                        request_accounting.record_error(&err_msg);
                        return Err(HarnessError::Retryable {
                            message: err_msg,
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
            let mut text_loop_detector = crate::stream_loop_detector::StreamLoopDetector::new(1024);
            let mut reasoning_loop_detector =
                crate::stream_loop_detector::StreamLoopDetector::new(1024);
            let mut loop_aborted: Option<crate::stream_loop_detector::DegeneratePattern> = None;
            // Finish-drain deadline (FINISH_DRAIN_GRACE). Once this turn's
            // stream has produced at least one delta, a cancellation no
            // longer wins the biased select outright: the stream gets one
            // short, strictly bounded window to reach its natural end. A
            // model that finished its answer right as the user sent the
            // next message (or hit Esc Esc) then commits that answer as a
            // completed round instead of unwinding as `Interrupted` after
            // every delta was already rendered — the false
            // "▲ interrupted · new message" marker over a round the user
            // watched finish. A stream that is still silent when the
            // deadline expires was not settling: the interrupt stands, so
            // an interrupt is delayed by at most FINISH_DRAIN_GRACE and a
            // genuinely mid-generation answer is still cut.
            let mut finish_drain: Option<std::time::Instant> = None;
            let mut turn_streamed = false;

            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled(), if finish_drain.is_none() => {
                        if !turn_streamed {
                            return Err(HarnessError::Interrupted);
                        }
                        // The stream already produced output this turn; open
                        // the bounded settle window instead of cutting now.
                        finish_drain = Some(std::time::Instant::now()
                            + crate::FINISH_DRAIN_GRACE);
                        continue;
                    }
                    // Guard against a stalled SSE stream: providers share a
                    // pooled client whose connect timeout covers the handshake
                    // but which deliberately sets no read timeout on streaming
                    // responses, so without this bound a connection that stays
                    // open but stops sending (common with overloaded
                    // reasoning-model endpoints) blocks the turn forever. The
                    // idle clock resets on every chunk, so a legitimately slow
                    // reasoning model that keeps trickling deltas is never cut
                    // off.
                    event = tokio::time::timeout(
                        finish_drain.map_or(STREAM_IDLE_TIMEOUT, |deadline| {
                            deadline.saturating_duration_since(std::time::Instant::now())
                        }),
                        stream.next(),
                    ) => {
                        let event = match event {
                            Ok(Some(event)) => event,
                            Ok(None) => break,
                            Err(_elapsed) if finish_drain.is_some() => {
                                // The settle window expired with the stream
                                // neither ended nor producing a chunk: the
                                // answer was not finishing after all. Honour
                                // the interrupt.
                                return Err(HarnessError::Interrupted);
                            }
                            Err(_elapsed) => {
                                tracing::warn!(
                                    idle_timeout_secs = STREAM_IDLE_TIMEOUT.as_secs(),
                                    "stream stalled: no data received within idle timeout"
                                );
                                let err_msg = format!(
                                    "Provider stream stalled — no data received \
                                     for {} seconds.",
                                    STREAM_IDLE_TIMEOUT.as_secs()
                                );
                                request_accounting.record_error(&err_msg);
                                return Err(HarnessError::Retryable {
                                    message: err_msg,
                                    retry_after_ms: None,
                                });
                            }
                        };
                        // Any chunk — text, reasoning, tool-call bytes, or the
                        // terminal `usage` event — proves this turn's stream
                        // was delivering, so a subsequent cancellation opens
                        // the settle window instead of discarding the turn.
                        turn_streamed = true;
                        match event? {
                            ProviderStreamEvent::TextDelta(delta) => {
                                request_accounting.observe_output(&delta);
                                content.push_str(&delta);
                                on_event(AgentEvent::AssistantDelta {
                                    delta: delta.clone(),
                                    start: !emitted_text,
                                });
                                emitted_text = true;
                                if let Some(pat) = text_loop_detector.push_and_check(&delta) {
                                    tracing::warn!(
                                        pattern = %pat.description(),
                                        "in-flight text stream loop detected; aborting stream early"
                                    );
                                    loop_aborted = Some(pat);
                                    break;
                                }
                            }
                            ProviderStreamEvent::ReasoningDelta(delta) => {
                                request_accounting.observe_output(&delta);
                                reasoning_content.push_str(&delta);
                                on_event(AgentEvent::ReasoningDelta {
                                    delta: delta.clone(),
                                    start: !emitted_reasoning,
                                });
                                emitted_reasoning = true;
                                if let Some(pat) = reasoning_loop_detector.push_and_check(&delta) {
                                    tracing::warn!(
                                        pattern = %pat.description(),
                                        "in-flight reasoning stream loop detected; aborting stream early"
                                    );
                                    loop_aborted = Some(pat);
                                    break;
                                }
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
                    let err_msg =
                        "Provider stream ended mid-tool-call; the response was likely truncated."
                            .to_string();
                    request_accounting.record_error(&err_msg);
                    return Err(HarnessError::Retryable {
                        message: err_msg,
                        retry_after_ms: None,
                    });
                }
            }

            if let Some(ref pat) = loop_aborted {
                content = crate::stream_loop_detector::StreamLoopDetector::trim_suffix(&content, pat);
                let steer_msg = format!(
                    "[System Directive: Output generation was truncated because it entered a {}. \
                     Please synthesize your current conclusions directly, proceed to your next tool action, \
                     or complete the task without repeating prior text.]",
                    pat.description()
                );
                messages.push(crate::conversation_context::hidden_user(
                    InjectionKind::LoopReviewNudge,
                    steer_msg,
                ));
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
                    let err_msg = format!(
                        "Provider stream ended with truncated arguments for tool \
                         call `{}`; the response was likely cut off.",
                        call.name
                    );
                    request_accounting.record_error(&err_msg);
                    return Err(HarnessError::Retryable {
                        message: err_msg,
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
                // Reasoning depth the channel actually ran this request at
                // (thinking-gated per protocol), so the transcript can label
                // each turn with the effort it truly used.
                effort: self.provider.effort().map(|e| e.as_str().to_string()),
                // Drain any provider-opaque wire credential the turn
                // accumulated (e.g. the Anthropic thinking signature) so the
                // next replay re-emits it verbatim. None for providers that
                // carry none; the map is opaque to this layer.
                provider_meta: self.provider.take_last_provider_meta(),
                hidden: false,
                children: None,
                runner_meta: None,
                origin: None,
                timestamp: Some(muta_contracts::todos::unix_now()),
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
                // Mid-round save point (ADR-0048): persist this turn's new
                // messages (the assistant response + all tool results) before
                // any further work, so a crash leaves the transcript in sync
                // with filesystem side effects.
                self.fire_turn_persist(messages).await?;
                let post_turn_estimate = self.estimate_next_request_tokens(messages);
                on_event(AgentEvent::ContextTokens(
                    muta_contracts::ContextTokenSnapshot::from_estimate(
                        post_turn_estimate,
                        muta_contracts::ContextTokenSource::Projection,
                    ),
                ));
                self.run_turn_hooks(messages, &round.state, round.turn_index)
                    .await;
                // Restore TurnEnd-scoped disables now that the ReAct turn is
                // over. RoundEnd-scoped disables survive until user-round end.
                self.restore_scoped_turn_end();
                continue;
            }

            // Round-exit gates. The insert drain happens after the provider
            // response commits, so an input typed during a would-be final
            // answer can still force one more turn in this same round.
            let duration_ms = round.started_at.elapsed().as_millis() as u64;
            let mut continue_round = false;
            if let Some((prompt, kind)) = self.stop_gate(&response).await {
                messages.push(crate::conversation_context::hidden_user(kind, prompt));
                continue_round = true;
            }
            let admitted = self.drain_follow_up(
                round.session_queue_generation,
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
                generation_ms: round.state.generation_ms,
                paused_ms: self
                    .round_paused_ms
                    .load(std::sync::atomic::Ordering::Relaxed),
            });
        }
    }

    /// Execute any tool calls carried by `response`, emitting events and
    /// appending tool results to `messages`. The single dispatch point of the
    /// streaming ReAct loop, so the dispatch contract — repeated-call guard,
    /// up-front `ToolCall` events, concurrent execution with FIFO-ordered
    /// results, and pursuit/mode updates — lives in exactly one place.
    ///
    /// `streamed_text` is true when the response text was already streamed to
    /// the UI, so a recognised text-fallback tool call retracts it with an
    /// `AssistantDiscard`. Returns `true` when a tool-carrying ReAct turn ran
    /// (the caller should loop again), `false` when the round is complete.
    ///
    /// `cancel` makes tool execution cooperative: if the turn is interrupted
    /// mid-flight, cooperatively-cancellable calls (runners) are drained and
    /// their partial results recorded into `messages` before this returns
    /// `Err(HarnessError::Interrupted)` — so interrupted runner work survives
    /// into the persisted transcript. Every other already-announced
    /// [`AgentEvent::ToolCall`] is paired with a terminal
    /// [`AgentEvent::ToolCancelled`] before this returns.
    async fn dispatch_tool_calls<F>(
        self: &Arc<Self>,
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
            // The four-stage pipeline (see `dispatch_pipeline`):
            //   1. preflight — turn classification, checkpoint-replay scan,
            //      doom-guard check, up-front ToolCall events, short-circuits;
            //   2. prepare — the per-call gate sequence, evaluated in-task
            //      inside `execute_tool` (never serialised across the batch);
            //   3. schedule — concurrent execution through the ToolScheduler;
            //   4. finalize — input-ordered recording, post-tool hooks, nudge.
            let prepared = self.dispatch_preflight(tool_calls, state, on_event);
            let outcome = if prepared.exec_indices.is_empty() {
                None
            } else {
                let exec_calls: Vec<ToolCall> = prepared
                    .exec_indices
                    .iter()
                    .map(|&i| tool_calls[i].clone())
                    .collect();
                let exec_ids: Vec<String> = prepared
                    .exec_indices
                    .iter()
                    .map(|&i| prepared.call_ids[i].clone())
                    .collect();
                Some(
                    self.schedule_tool_calls(&exec_calls, &exec_ids, cancel, on_event)
                        .await?,
                )
            };
            return self
                .dispatch_finalize(tool_calls, prepared, outcome, messages, state, on_event)
                .await;
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
                let outcome = self
                    .execute_tool_evented(&call, &call_id, cancel, on_event)
                    .await?;
                if outcome.interrupted {
                    // Record a drained result (an interrupted runner's partial
                    // transcript) before ending the round as interrupted.
                    if let Some(result) = outcome.result {
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
                    }
                    return Err(HarnessError::Interrupted);
                }
                match outcome.result {
                    Some(result) => result,
                    // Defensive: a non-interrupted outcome must carry a
                    // result — the executor guarantees it. Surface an
                    // internal error rather than panicking the harness.
                    None => {
                        return Err(HarnessError::Other(
                            "internal error: non-interrupted tool outcome lost its result"
                                .to_string(),
                        ));
                    }
                }
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
    pub(crate) fn record_tool_result<F>(
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
        // Cost attribution: an runner's true token consumption can be 100x
        // the byte-estimate of its final summary, so accumulate the real
        // `TokenUsage` it reported. For every other tool the byte-estimate
        // remains the only signal we have.
        if checkpoint_replay {
            // The short checkpoint reference is new model-visible context,
            // but the original tool (especially an runner) did no new work, so
            // do not attribute its nested usage a second time.
            state.token_usage.total_tokens += pressure::estimate_string_tokens(&text);
        } else if let Some((_sub_messages, sub_usage)) = result.runner_payload() {
            state.token_usage.total_tokens += sub_usage.total_tokens;
            state.token_usage.prompt_tokens += sub_usage.prompt_tokens;
            state.token_usage.completion_tokens += sub_usage.completion_tokens;
            // The runner's output tokens are in the numerator above; fold its
            // own generation time into the denominator too, so the round's
            // throughput stays scoped-consistent (no inflated tok/s for
            // delegating rounds). Tool execution inside the runner is already
            // excluded from this figure.
            state.generation_ms = state
                .generation_ms
                .saturating_add(result.runner_generation_ms());
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
        // For runner results, attach the nested transcript as `children` on
        // the persisted Tool-role message so resume can rebuild the runner
        // view without a live event stream. The nested `Message`s already
        // self-contain their own tool_calls / tool_call_id / children, so
        // arbitrarily deep runner trees round-trip through session.json.
        // Sidecar `runner_meta` captures what the live event stream knew but
        // the bare transcript cannot reconstruct on resume: duration, the
        // task description, the toolset size, and explicit failed /
        // interrupted flags. The runner result text is built by
        // `runner_result_text`, which appends a deterministic
        // role-reanchoring note at this single choke point (see its doc for
        // the "role bleed" rationale). For non-runner results the plain header
        // is used unchanged.
        let tool_message = match result.runner_payload() {
            Some((sub_messages, _)) => {
                let meta = crate::message::RunnerMeta {
                    duration_ms: Some(duration_ms),
                    failed: result.is_error(),
                    interrupted: result.runner_interrupted(),
                    ..Default::default()
                };
                Message::tool_result(
                    call,
                    runner_result_text(
                        &call.name,
                        &text,
                        result.is_error(),
                        result.runner_interrupted(),
                    ),
                )
                .with_children(sub_messages.to_vec())
                .with_runner_meta(meta)
            }
            None => Message::tool_result(call, format!("[{} result]:\n{}", call.name, text)),
        };
        messages.push(tool_message);

        // Image peel-out (mirrors opencode's OpenAI-Chat lowering). The tool
        // message only carries text (OpenAI Chat Completions requires tool
        // content to be a string), so the actual image is injected as a
        // follow-up user-role message with the image attached — the same
        // channel paste-up uses. The provider serialises it to `image_url`
        // (OpenAI-compat) / `inline_data` (Google), letting the model see the
        // pixels. A short textual link ties the two messages together.
        if let ToolOutput::Image { mime, data } = result {
            messages.push(crate::conversation_context::tool_image(
                &call.name,
                mime.clone(),
                data.clone(),
            ));
        }
    }
}
