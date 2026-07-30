//! Turn-level orchestration policy on top of the `Agent` struct.
//!
//! `Agent` (in [`crate::agent`]) runs a single ReAct turn against a provider.
//! This module wraps every turn with the cross-cutting policy a frontend
//! cannot reasonably reimplement: context compaction (pre-turn and mid-turn
//! pruning), retry with exponential backoff, permission relay, and the
//! `/repeat` cron scheduler.
//!
//! Frontends drive the harness through [`execute_round`],
//! [`start_interactive_round`], and
//! [`start_repeat_scheduler`]. They own only the UI-specific input path (slash commands for the CLI, menus/dialogs for a
//! future GUI); the actual round machinery is shared here.
//!
//! All items are `pub` because they are assembled by the binary, which knows
//! the concrete provider/tool instances and the frontend's request channel.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::task::{Context, Poll};
use std::time::Instant;

use async_trait::async_trait;
use futures::Stream;
use futures::stream::{BoxStream, StreamExt};
use serde::Serialize;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{Agent, RequestTokenEstimate, RoundBegin, RoundLifecycle};
use neenee_core::{
    AgentEvent, AgentRequest, AgentResponse, CronExpr, HarnessError, HarnessSnapshot, ImagePart,
    InjectionKind, LoopStatus, Message, ModelRequest, NoticeKind, NoticeSeverity, NoticeSource,
    NoticeSurface, Provider, ProviderStreamEvent, Role, RoundEvent, estimate_bytes,
    repeat::DEFAULT_MAX_AGE_DAYS,
};
use neenee_persistence::{
    config::Config,
    session::{
        ContextProjectionCheckpoint, ContextProjectionResult, SessionStore, run_compaction,
    },
};

/// Wrap a session-scoped [`RoundEvent`] in the [`AgentResponse::Round`]
/// envelope (ADR-0017). Every round-scoped emitter routes through this so the
/// session id is attached uniformly, letting the TUI key transcript buffers
/// by `session_id` and dispatch primary vs `/btw` side events correctly.
pub fn round_response(session_id: &str, event: RoundEvent) -> AgentResponse {
    AgentResponse::Round {
        session_id: session_id.to_string(),
        event,
    }
}

pub struct ProxyProvider {
    pub holder: Arc<RwLock<Arc<dyn Provider>>>,
    /// Whether `/debug trace` is armed. Read on every call so the
    /// toggle takes effect for the very next round-trip.
    debug_enabled: Arc<AtomicBool>,
    /// Dump directory while capture is on; `None` when off.
    debug_dir: Arc<std::sync::Mutex<Option<PathBuf>>>,
    /// Monotonic counter for unique filenames within the same millisecond.
    debug_seq: AtomicU64,
}

impl ProxyProvider {
    pub fn new(holder: Arc<RwLock<Arc<dyn Provider>>>) -> Self {
        Self {
            holder,
            debug_enabled: Arc::new(AtomicBool::new(false)),
            debug_dir: Arc::new(std::sync::Mutex::new(None)),
            debug_seq: AtomicU64::new(0),
        }
    }

    /// Resolve a capture record for the upcoming call, or `None` when capture
    /// is off. Clones the request messages once (only when armed) so the call
    /// can still move the originals into the inner provider.
    fn begin_capture(
        &self,
        provider: &str,
        model: &str,
        kind: &'static str,
        request: &ModelRequest,
    ) -> Option<PendingCapture> {
        if !self.debug_enabled.load(Ordering::SeqCst) {
            return None;
        }
        let dir = self
            .debug_dir
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()?;
        Some(PendingCapture {
            provider: provider.to_string(),
            model: model.to_string(),
            kind,
            dir,
            request: request.clone(),
            seq: self.debug_seq.fetch_add(1, Ordering::SeqCst),
        })
    }
}

#[async_trait]
impl Provider for ProxyProvider {
    /// Delegate to the currently active inner provider so attribution tracks
    /// the live provider even after a mid-session `/models` switch.
    fn provider_id(&self) -> String {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .provider_id()
    }

    fn model(&self) -> String {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .model()
    }

    fn model_capabilities(&self) -> neenee_core::ModelCapabilities {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .model_capabilities()
    }

    fn prompt_hints(&self) -> neenee_core::ProviderPromptHints {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .prompt_hints()
    }

    fn set_debug_capture(&self, enabled: bool, dir: PathBuf) {
        self.debug_enabled.store(enabled, Ordering::SeqCst);
        *self
            .debug_dir
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = if enabled { Some(dir) } else { None };
    }

    fn debug_capture_enabled(&self) -> bool {
        self.debug_enabled.load(Ordering::SeqCst)
    }

    async fn chat(&self, request: ModelRequest) -> Result<Message, String> {
        let p = self
            .holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let provider_id = p.provider_id();
        let model = p.model();
        let started = Instant::now();
        let capture = self.begin_capture(&provider_id, &model, "chat", &request);
        let result = p.chat(request).await;
        if let Some(capture) = capture {
            let item = match &result {
                Ok(message) => serde_json::json!({
                    "status": "ok",
                    "duration_ms": started.elapsed().as_millis() as u64,
                    "message": message,
                }),
                Err(error) => serde_json::json!({
                    "status": "error",
                    "duration_ms": started.elapsed().as_millis() as u64,
                    "error": error,
                }),
            };
            write_capture(&capture, &[item]);
        }
        result
    }
    async fn stream_chat(
        &self,
        request: ModelRequest,
    ) -> Result<BoxStream<'static, Result<String, String>>, String> {
        let p = self
            .holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let provider_id = p.provider_id();
        let model = p.model();
        let capture = self.begin_capture(&provider_id, &model, "stream_chat", &request);
        let stream = p.stream_chat(request).await;
        match (capture, stream) {
            (Some(capture), Err(error)) => {
                write_capture(
                    &capture,
                    &[serde_json::json!({ "status": "error", "error": error })],
                );
                Err(error)
            }
            (Some(capture), Ok(stream)) => Ok(CapturedStream {
                inner: stream,
                items: Vec::new(),
                capture,
            }
            .boxed()),
            (None, Ok(stream)) => Ok(stream),
            (None, Err(error)) => Err(error),
        }
    }
    async fn stream_chat_events(
        &self,
        request: ModelRequest,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent, String>>, String> {
        let p = self
            .holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let provider_id = p.provider_id();
        let model = p.model();
        let capture = self.begin_capture(&provider_id, &model, "stream_chat_events", &request);
        let stream = p.stream_chat_events(request).await;
        match (capture, stream) {
            (Some(capture), Err(error)) => {
                write_capture(
                    &capture,
                    &[serde_json::json!({ "status": "error", "error": error })],
                );
                Err(error)
            }
            (Some(capture), Ok(stream)) => Ok(CapturedStream {
                inner: stream,
                items: Vec::new(),
                capture,
            }
            .boxed()),
            (None, Ok(stream)) => Ok(stream),
            (None, Err(error)) => Err(error),
        }
    }

    /// Delegate usage support + drain to the live inner provider so attribution
    /// tracks the active provider even after a mid-session `/models` swap.
    fn usage_supported(&self) -> bool {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .usage_supported()
    }

    fn take_last_usage(&self) -> Option<neenee_core::TokenUsage> {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .take_last_usage()
    }

    fn take_last_provider_meta(&self) -> Option<serde_json::Map<String, serde_json::Value>> {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .take_last_provider_meta()
    }
}

// ── /debug trace ──────────────────────────────────────────────

/// A queued capture record awaiting its response. Held across the inner call
/// (for `chat`) or inside a [`CapturedStream`] (for the streaming paths) and
/// flushed once the round-trip is complete.
struct PendingCapture {
    provider: String,
    model: String,
    kind: &'static str,
    dir: PathBuf,
    request: ModelRequest,
    seq: u64,
}

/// Stream wrapper that tees every item into a buffer and flushes a single
/// capture file on drop — so one streaming round-trip yields one complete JSON
/// file, whether the stream ran to completion, errored, or was cancelled
/// mid-stream (a cancelled stream simply writes whatever was collected).
///
/// The wrapper is `Unpin`: its only pinned field (`inner: BoxStream`) is a
/// `Pin<Box<…>>`, which is itself `Unpin`, so `Pin::new(&mut self.inner)` is
/// sound. This keeps `poll_next` free of unsafe.
struct CapturedStream<S> {
    inner: S,
    items: Vec<serde_json::Value>,
    capture: PendingCapture,
}

impl<S> Stream for CapturedStream<S>
where
    S: Stream + Unpin,
    S::Item: Serialize,
{
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_next(cx) {
            Poll::Ready(Some(item)) => {
                this.items
                    .push(serde_json::to_value(&item).unwrap_or(serde_json::Value::Null));
                Poll::Ready(Some(item))
            }
            other => other,
        }
    }
}

impl<S> Drop for CapturedStream<S> {
    fn drop(&mut self) {
        write_capture(&self.capture, &self.items);
    }
}

/// Serialize one capture record and write it atomically to the dump directory.
/// Failures are logged and swallowed: debug capture must never break a real
/// turn. Files are owner-only (`0o600`) via `atomic_write_bytes` — request
/// messages can carry pasted secrets, the same privacy profile as `/export`.
fn write_capture(capture: &PendingCapture, items: &[serde_json::Value]) {
    let timestamp = chrono::Utc::now();
    let record = serde_json::json!({
        "timestamp": timestamp.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "provider": capture.provider,
        "model": capture.model,
        "kind": capture.kind,
        "request": {
            "messages": &capture.request.messages,
            "tools": &capture.request.tool_specs,
        },
        "response": { "items": items },
    });
    let bytes = match serde_json::to_vec_pretty(&record) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%error, "network capture serialize failed");
            return;
        }
    };
    let provider_slug = slug(&capture.provider);
    let model_slug = slug(&capture.model);
    let stamp = timestamp.format("%Y%m%d-%H%M%S%.3f");
    let file = capture.dir.join(format!(
        "{stamp}_{seq:04}_{provider_slug}_{model_slug}.json",
        seq = capture.seq,
    ));
    if let Err(error) = neenee_persistence::fsutil::atomic_write_bytes(&file, &bytes) {
        tracing::warn!(%error, file = %file.display(), "network capture write failed");
    }
}

/// Lowercase alnum/hyphen filename component, empty -> `"anon"`.
fn slug(value: &str) -> String {
    let mut out: String = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
        .map(|character| character.to_ascii_lowercase())
        .collect();
    if out.is_empty() {
        out.push_str("anon");
    }
    out
}

#[derive(Clone)]
pub struct ContextProjectionSettings {
    /// Token thresholds resolved against the active model's context window.
    /// Pressure (estimated in tokens) is compared against these to decide when
    /// to prune and when to run a full summarizing compaction.
    pub budget: neenee_core::ContextBudget,
    pub preserve_rounds: usize,
    /// Use the active model to produce an anchored structured summary.
    pub summarize: bool,
    /// Enable cheap tool-result pruning (pre-turn and mid-turn).
    pub prune: bool,
    /// Character budget of the most recent tool results protected from pruning.
    pub prune_protect_chars: usize,
}

impl ContextProjectionSettings {
    /// Mid-turn pruning only fires when it can reclaim at least this many chars,
    /// to avoid pruning churn for negligible gains.
    pub const PRUNE_MIN_RECLAIM_CHARS: usize = 8_000;

    /// Resolve settings for the active model's context window. `window_tokens`
    /// is the live model's context window (tokens); `0` means unknown and the
    /// policy's fallback window is substituted.
    pub fn from_config(config: &Config, window_tokens: usize) -> Self {
        Self {
            budget: config.compaction.resolve(window_tokens),
            preserve_rounds: config.compaction_preserve_rounds,
            summarize: config.compaction_summarize,
            prune: config.compaction_prune,
            prune_protect_chars: config.compaction_prune_protect_tokens
                * neenee_core::CHARS_PER_TOKEN,
        }
    }

    /// Adjust the post-compaction history target so the complete projected
    /// request — checkpoint, system prompt, injected context, and tool schemas
    /// — lands near `target_utilization`, not merely the durable transcript.
    pub fn for_request(&self, request: RequestTokenEstimate) -> Self {
        let mut resolved = self.clone();
        let floor = resolved.budget.target_tokens.clamp(1, 2_000);
        resolved.budget.target_tokens = resolved
            .budget
            .target_tokens
            .saturating_sub(request.overhead_tokens)
            .max(floor);
        resolved
    }
}

#[cfg(test)]
mod projection_settings_tests {
    use super::*;

    #[test]
    fn compaction_target_accounts_for_projected_request_overhead() {
        let settings = ContextProjectionSettings {
            budget: neenee_core::CompactionPolicy::default().resolve(200_000),
            preserve_rounds: 6,
            summarize: true,
            prune: true,
            prune_protect_chars: 24_000,
        };
        let resolved = settings.for_request(RequestTokenEstimate {
            history_tokens: 100_000,
            overhead_tokens: 12_000,
            total_tokens: 112_000,
        });

        assert_eq!(settings.budget.target_tokens, 50_000);
        assert_eq!(resolved.budget.target_tokens, 38_000);
        assert_eq!(resolved.budget.compaction_threshold_tokens, 170_000);
    }
}

/// Mid-round model-context projection gate: prunes old tool results durably
/// when the active round is approaching the model's context budget.
pub struct MidTurnPruneProjectionGate {
    pub session: Arc<SessionStore>,
    pub prune_protect_chars: usize,
}

#[async_trait]
impl crate::ContextProjectionGate for MidTurnPruneProjectionGate {
    async fn project_context(&self, messages: Vec<Message>) -> Option<Vec<Message>> {
        let mut messages = messages;
        let outcome = neenee_core::prune_tool_results(
            &mut messages,
            self.prune_protect_chars,
            ContextProjectionSettings::PRUNE_MIN_RECLAIM_CHARS,
        )?;
        let after_chars = estimate_bytes(&messages);
        let checkpoint = ContextProjectionCheckpoint {
            operation: neenee_persistence::session::ContextProjectionKind::Prune,
            archived_messages: outcome.originals.len(),
            active_messages: messages.len(),
            before_chars: after_chars + outcome.reclaimed_chars,
            after_chars,
        };
        let result = ContextProjectionResult {
            model_window: messages.clone(),
            archived_originals: outcome.originals,
            checkpoint,
        };
        if let Err(error) = self.session.commit_context_projection(result).await {
            tracing::warn!(?error, "mid-turn prune commit failed");
        }
        Some(messages)
    }
}

/// Emit the current harness snapshot (mode, round counter, loop
/// status, autopilot) to the UI.
pub fn send_harness_state(
    tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: &str,
    agent: &Agent,
    loop_status: LoopStatus,
) {
    // Running snapshots are emitted after lifecycle admission but
    // immediately before `execute_round` performs the counter bump. Project
    // that admitted round here so frontends receive the authoritative display
    // value without locally guessing from transcript length.
    let round_counter = agent
        .round_count()
        .saturating_add(u64::from(!loop_status.is_idle()));
    let _ = tx.send(round_response(
        session_id,
        RoundEvent::HarnessState(HarnessSnapshot {
            loop_status,
            round_counter,
            autopilot: agent.get_autopilot(),
        }),
    ));
}

#[derive(Clone)]
pub struct RoundContext {
    pub agent: Arc<Agent>,
    pub tx: mpsc::UnboundedSender<AgentResponse>,
    pub token: CancellationToken,
    pub session: Arc<SessionStore>,
    /// Session id this round belongs to (ADR-0017). Tags every emitted
    /// [`RoundEvent`] so the TUI routes primary vs `/btw` side events correctly.
    pub session_id: String,
    pub projection: ContextProjectionSettings,
    pub retry_max_attempts: usize,
    pub retry_base_ms: u64,
    pub retry_max_ms: u64,
    /// Emit the frontend's natural-completion signal. Repeat drivers
    /// call `execute_round` internally and must not release a user's paused
    /// next-round outbox between their own continuation iterations.
    pub emit_round_completed: bool,
}

pub struct RoundInput {
    pub prompt: String,
    pub hidden: bool,
    pub display_prompt: Option<String>,
    /// Exact TUI send time for user-authored messages, in Unix-epoch milliseconds.
    pub sent_at_ms: Option<u64>,
    /// Inline images pasted into the prompt, attached to the user message.
    pub images: Vec<ImagePart>,
}

#[derive(Clone)]
pub struct InteractiveRoundContext {
    pub agent: Arc<Agent>,
    pub tx: mpsc::UnboundedSender<AgentResponse>,
    pub lifecycle: Arc<RoundLifecycle>,
    pub session: Arc<SessionStore>,
    /// Session id this round belongs to (ADR-0017). Tags every emitted
    /// [`RoundEvent`] so the TUI routes primary vs `/btw` side events correctly.
    pub session_id: String,
    pub projection: ContextProjectionSettings,
    pub retry_max_attempts: usize,
    pub retry_base_ms: u64,
    pub retry_max_ms: u64,
}

pub async fn start_interactive_round(context: InteractiveRoundContext, input: RoundInput) {
    let RoundBegin {
        token,
        generation,
        previous,
    } = context.lifecycle.begin().await;
    if let Some(previous) = previous {
        context.agent.reject_pending_permissions();
        context.agent.reject_pending_user_questions();
        context.agent.reject_pending_inputs();
        let _ = context.tx.send(AgentResponse::PermissionsCleared);
        previous.cancel();
    }
    for stale in context
        .agent
        .begin_user_input_round(context.session_id.clone(), generation)
    {
        let _ = context.tx.send(round_response(
            &context.session_id,
            RoundEvent::UserInputUnavailable { input_id: stale.id },
        ));
    }
    let _ = context.tx.send(round_response(
        &context.session_id,
        RoundEvent::Activity("starting request".to_string()),
    ));

    tokio::spawn(async move {
        send_harness_state(
            &context.tx,
            &context.session_id,
            &context.agent,
            LoopStatus::Running,
        );
        let result = execute_round(
            RoundContext {
                agent: context.agent.clone(),
                tx: context.tx.clone(),
                token: token.clone(),
                session: context.session,
                session_id: context.session_id.clone(),
                projection: context.projection,
                retry_max_attempts: context.retry_max_attempts,
                retry_base_ms: context.retry_base_ms,
                retry_max_ms: context.retry_max_ms,
                emit_round_completed: true,
            },
            input,
        )
        .await;
        for pending in context.agent.close_user_input_round(generation) {
            let _ = context.tx.send(round_response(
                &context.session_id,
                RoundEvent::UserInputUnavailable {
                    input_id: pending.id,
                },
            ));
        }
        let is_current = context.lifecycle.is_current(generation);
        match result {
            Ok(_) => {}
            Err(HarnessError::Interrupted) if is_current => {
                let _ = context.tx.send(round_response(
                    &context.session_id,
                    RoundEvent::Text("... [Interrupted]".to_string()),
                ));
            }
            Err(error) if is_current => {
                let _ = context.tx.send(round_response(
                    &context.session_id,
                    RoundEvent::Error(error.to_string()),
                ));
            }
            Err(_) => {}
        }
        if context.lifecycle.finish(generation).await {
            send_harness_state(
                &context.tx,
                &context.session_id,
                &context.agent,
                LoopStatus::Idle,
            );
        }
    });
}

pub async fn execute_round(
    context: RoundContext,
    mut input: RoundInput,
) -> Result<(), HarnessError> {
    let RoundContext {
        agent,
        tx,
        token,
        session,
        session_id,
        projection,
        retry_max_attempts,
        retry_base_ms,
        retry_max_ms,
        emit_round_completed,
    } = context;
    // Bind accounting to the session that admitted this round. The principal
    // agent survives `/session open` and `/resume`, so its construction-time
    // thread id is not sufficient for attribution.
    agent.set_thread_id(session_id.clone());
    if let Some(ledger) = agent.token_ledger() {
        ledger.set_active_session(session_id.clone());
    }
    let previous_round = agent.round_count();
    let _ = tx.send(round_response(
        &session_id,
        RoundEvent::Activity("saving request".to_string()),
    ));

    // UserPromptSubmit hooks (ADR-0025): a hook may deny the prompt or prepend
    // context. Hidden control prompts are harness-internal and bypass the gate.
    if !input.hidden {
        match agent.fire_user_prompt_submit(&input.prompt).await {
            crate::hooks::UserPromptVerdict::Deny(reason) => {
                let _ = tx.send(round_response(
                    &session_id,
                    RoundEvent::Text(format!("Prompt blocked by hook: {reason}")),
                ));
                return Ok(());
            }
            crate::hooks::UserPromptVerdict::Prepend(context) => {
                input.prompt = format!("{context}\n\n{}", input.prompt);
            }
            crate::hooks::UserPromptVerdict::Allow => {}
        }
    }

    // The prompt is now admitted. Bump exactly once before request assembly so
    // hooks, token accounting, todos, and emitted positions share one round
    // number. A prompt rejected by UserPromptSubmit never opens a round.
    agent.bump_round();
    let admitted_round = agent.round_count();

    let admitted_session_id = session.id().await;
    // Build `round_history` — the round's working scratch — from the session's
    // authoritative `model_window` plus the new user message (ADR-0048). The
    // session is the single source of truth for message truth; this clone is
    // the only transient copy, and it is committed back to the session before
    // the round ends, so the wire body (a projection of this scratch, which is
    // itself a projection of the session) can never diverge from the durable
    // state. The user message is pushed here *before* the durable commit so a
    // mid-round crash is recoverable (ADR-0035); on an unrecoverable Phase-1
    // failure the unsend path below pops it back out and reverts the session.
    // Snapshot the user's prompt and images before they are moved into the
    // user message. If the round is interrupted in Phase 1 (request sent but no
    // response bytes received), we unsend the message: pop it back out of the
    // context and restore these to the TUI input box for re-editing.
    let unsent_prompt = input.prompt.clone();
    let unsent_images = input.images.clone();

    let mut round_history = {
        let mut th = session.model_window().await;
        th.push(if input.hidden {
            crate::conversation_context::hidden_user(InjectionKind::HiddenRoundInput, input.prompt)
        } else {
            let message = Message::new(Role::User, input.prompt);
            let message = match input.display_prompt {
                Some(display) => message.with_display_content(display),
                None => message,
            };
            let message = match input.sent_at_ms {
                Some(sent_at_ms) => message.with_sent_at_ms(sent_at_ms),
                None => message,
            };
            if input.images.is_empty() {
                message
            } else {
                message.with_images(input.images)
            }
        });
        th
    };
    session.replace_messages(round_history.clone()).await?;
    // Persist admission immediately. Mid-round crash recovery must not restore
    // the transcript from round N while leaving the session counter at N-1.
    session.set_round_counter(admitted_round).await?;

    // Install the mid-round save point (ADR-0035) so every ReAct-turn boundary
    // durably appends its new messages to the session log. This is the fix for
    // the resume-after-crash gap: without it, a round that ran side-effecting
    // tools and then crashed rewinds the transcript to the previous round,
    // leaving it out of sync with the filesystem. The closure clones the
    // session `Arc` and the message slice (the `BoxFuture` is `'static`), then
    // delegates to `SessionStore::append_turn`, which writes only the delta.
    {
        let session_for_round = Arc::clone(&session);
        let agent_for_round = Arc::clone(&agent);
        let accounting_ledger = agent.token_ledger();
        let accounting_session_id = session_id.clone();
        agent.set_turn_persist(Arc::new(move |messages: &[Message]| {
            let session = Arc::clone(&session_for_round);
            let agent = Arc::clone(&agent_for_round);
            let snapshot = messages.to_vec();
            let ledger = accounting_ledger.clone();
            let session_id = accounting_session_id.clone();
            Box::pin(async move {
                session.append_turn(&snapshot).await?;
                let round_counter = agent.round_count();
                if round_counter != session.round_counter().await {
                    session.set_round_counter(round_counter).await?;
                }
                if let Some(ledger) = ledger {
                    session
                        .set_request_usage_records(ledger.records_for_session(&session_id))
                        .await?;
                }
                Ok(())
            })
        }));
    }
    let _ = tx.send(round_response(
        &session_id,
        RoundEvent::Activity("preparing context".to_string()),
    ));
    // Cheap tool-result pruning to relieve pressure before considering a full
    // compaction. Gated by the model-relative `prune_utilization` threshold
    // (ADR-0019) so it engages only once pressure crosses that fraction of the
    // window — not every turn — mirroring the mid-turn gate. Pruning also
    // self-limits to runs that reclaim meaningful space.
    let mut request_estimate = agent.estimate_next_request_tokens(&round_history);
    if projection.prune && request_estimate.total_tokens > projection.budget.prune_threshold_tokens
    {
        prune_and_commit(&mut round_history, &session, &projection).await?;
        request_estimate = agent.estimate_next_request_tokens(&round_history);
    }
    if request_estimate.total_tokens > projection.budget.compaction_threshold_tokens {
        let _ = tx.send(round_response(
            &session_id,
            RoundEvent::Activity("compacting context".to_string()),
        ));
        let extra = agent.fire_pre_compact().await;
        let compaction_settings = projection.for_request(request_estimate);
        if let Some(checkpoint) = compact_round_history(
            &mut round_history,
            &session,
            &compaction_settings,
            Some(agent.provider.clone()),
            extra,
        )
        .await?
        {
            send_compaction(&tx, &session_id, &checkpoint);
        }
        agent.fire_post_compact().await;
        let _ = tx.send(round_response(
            &session_id,
            RoundEvent::Activity("preparing context".to_string()),
        ));
    }

    let tool_activity = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let streamed_text = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut attempt: usize = 0;
    let retry_limit = retry_max_attempts.clamp(1, 10);
    let mut compacted_after_overflow = false;
    // Keep the ReAct turn alive across network attempts. Completed prior turns
    // are already durably checkpointed above; retaining this state means a
    // retry resumes the pending provider request with the same history, guard
    // registry, hooks, and accounting instead of replaying side effects.
    let mut streaming_round = agent.begin_streaming_round();
    let result = loop {
        attempt += 1;
        let activity_for_run = tool_activity.clone();
        let streamed_for_run = streamed_text.clone();
        let accounting_ledger = agent.token_ledger();
        let accounting_session = Arc::clone(&session);
        let accounting_session_id = session_id.clone();
        let result = agent
            .resume_streaming_with_events(
                &mut round_history,
                &token,
                &mut streaming_round,
                |event| {
                    if matches!(event, AgentEvent::ToolCall { .. }) {
                        activity_for_run.store(true, Ordering::SeqCst);
                    }
                    if matches!(event, AgentEvent::ModelRequestStarted { .. })
                        && let Some(ledger) = accounting_ledger.clone()
                    {
                        // Persist the in-flight state without blocking streaming.
                        // The task reads the ledger when it runs, so if completion
                        // races ahead it writes the newer terminal record rather
                        // than overwriting it with a stale in-flight snapshot.
                        let session = Arc::clone(&accounting_session);
                        let session_id = accounting_session_id.clone();
                        tokio::spawn(async move {
                            let records = ledger.records_for_session(&session_id);
                            if let Err(error) = session.set_request_usage_records(records).await {
                                tracing::warn!(
                                    %error,
                                    "could not persist in-flight request usage"
                                );
                            }
                        });
                    }
                    relay_agent_event(&tx, &session_id, event, &streamed_for_run);
                },
            )
            .await;

        let Err(error) = result else {
            break result;
        };
        if let Err(error) = persist_request_usage(&agent, &session, &session_id).await {
            tracing::warn!(%error, "could not persist request usage after failed attempt");
        }
        if matches!(error, HarnessError::ContextOverflow(_))
            && !compacted_after_overflow
            && !tool_activity.load(Ordering::SeqCst)
        {
            let overflow_settings = ContextProjectionSettings {
                preserve_rounds: projection.preserve_rounds.max(1),
                ..projection.clone()
            }
            .for_request(agent.estimate_next_request_tokens(&round_history));
            if compact_round_history(
                &mut round_history,
                &session,
                &overflow_settings,
                Some(agent.provider.clone()),
                Vec::new(),
            )
            .await?
            .is_some()
            {
                compacted_after_overflow = true;
                if streamed_text.swap(false, Ordering::SeqCst) {
                    let _ = tx.send(round_response(&session_id, RoundEvent::StreamDiscard));
                }
                if let Some(checkpoint) = session.last_projection().await {
                    send_compaction(&tx, &session_id, &checkpoint);
                }
                attempt = attempt.saturating_sub(1);
                continue;
            }
        }

        let HarnessError::Retryable {
            message,
            retry_after_ms,
        } = error
        else {
            break Err(error);
        };
        if attempt >= retry_limit {
            break Err(HarnessError::Other(format!(
                "{message}\n\nGave up after {retry_limit} attempt(s); the upstream \
                 service appears overloaded. Resend the message to try again, or \
                 raise `provider_retry_max_attempts` for more attempts."
            )));
        }
        if streamed_text.swap(false, Ordering::SeqCst) {
            let _ = tx.send(round_response(&session_id, RoundEvent::StreamDiscard));
        }
        let base_ms = retry_delay_ms(attempt, retry_after_ms, retry_base_ms, retry_max_ms);
        // Apply equal jitter (half fixed, half random) to de-synchronise
        // clients that fail in unison — e.g. many sessions behind one load
        // balancer all hitting a flaky upstream at once. The jittered value
        // stays within `[base/2, base]`, so the configured cap and any
        // server `Retry-After` are still honoured. `apply_jitter_ms` is a
        // pure function; the RNG is injected here so it stays out of tests.
        let delay_ms = apply_jitter_ms(base_ms, |_| fastrand::u64(0..base_ms));
        tracing::warn!(
            attempt = attempt + 1,
            max_attempts = retry_limit,
            delay_ms,
            base_ms,
            resumed_after_tools = tool_activity.load(Ordering::SeqCst),
            "retrying after transient provider error"
        );
        let checkpoint_note = if tool_activity.load(Ordering::SeqCst) {
            " Completed tool results are preserved; this retries only the pending model request."
        } else {
            ""
        };
        let _ = tx.send(round_response(
            &session_id,
            RoundEvent::Notice(
                neenee_core::AgentNotice::new(
                    NoticeKind::ProviderRetry,
                    NoticeSeverity::Warning,
                    format!("Retrying provider request ({}/{retry_limit})", attempt + 1),
                    NoticeSource::Harness,
                )
                .with_body(format!(
                    "Waiting {}s before retrying: {}{}",
                    delay_ms.div_ceil(1_000),
                    public_retry_reason(&message),
                    checkpoint_note,
                ))
                .with_surface(NoticeSurface::Toast),
            ),
        ));
        let _ = tx.send(round_response(
            &session_id,
            RoundEvent::RetryScheduled {
                attempt: attempt + 1,
                max_attempts: retry_limit,
                delay_ms,
                message,
            },
        ));
        tokio::select! {
            _ = token.cancelled() => return Err(HarnessError::Interrupted),
            _ = tokio::time::sleep(std::time::Duration::from_millis(delay_ms)) => {}
        }
    };
    // Phase-1 unsend: if the user interrupted before any model output reached
    // the client (no streamed text) and no tool has executed this round, the
    // round is reversible at the conversation layer. Pop the user message back
    // out of the context, revert the session store to its pre-round state, and
    // hand the prompt back to the TUI for re-editing. Returning `Ok(false)`
    // (rather than propagating `Err(Interrupted)`) keeps
    // `start_interactive_round`'s interrupt handler from emitting the generic
    // "... [Interrupted]" notice — the unsend is the user's intent here.
    //
    // Billing caveat (see docs/explanation/interrupt-semantics.md): the
    // network request was already on the wire, so the provider may still bill
    // the input tokens of the cancelled request. The unsend only guarantees a
    // clean conversation context and zero output tokens — it cannot un-send
    // the packet.
    if matches!(result, Err(HarnessError::Interrupted))
        && !streamed_text.load(Ordering::SeqCst)
        && !tool_activity.load(Ordering::SeqCst)
    {
        // The user message is the last entry in `round_history` (pushed before
        // the streaming round). Only a non-hidden round is unsentable: hidden
        // control prompts are harness-
        // internal and should not be surfaced as editable user input.
        if round_history
            .last()
            .is_some_and(|m| m.role == Role::User && !input.hidden)
        {
            round_history.pop();
            session.replace_messages(round_history).await?;
            agent.restore_round_count(previous_round);
            session.set_round_counter(previous_round).await?;
            persist_request_usage(&agent, &session, &session_id).await?;
            send_context_projection(&tx, &session_id, &agent, &session.model_window().await);
            let _ = tx.send(round_response(
                &session_id,
                RoundEvent::UnsentInput {
                    prompt: unsent_prompt,
                    images: unsent_images,
                },
            ));
            return Ok(());
        }
    }
    if session.id().await != admitted_session_id {
        return Err(HarnessError::Interrupted);
    }
    let _ = tx.send(round_response(
        &session_id,
        RoundEvent::Activity("saving response".to_string()),
    ));
    session.replace_messages(round_history).await?;
    persist_request_usage(&agent, &session, &session_id).await?;
    // Publish from the final committed history on every terminal path. This
    // reconciles the pre-wire estimate after interruption, tool cancellation,
    // or response commit instead of leaving the meter anchored to a request
    // shape that is no longer AI-visible.
    send_context_projection(&tx, &session_id, &agent, &session.model_window().await);
    let outcome = result?;

    let visible = outcome.message.content.trim().to_string();
    if !visible.is_empty() && !streamed_text.load(Ordering::SeqCst) {
        let _ = tx.send(round_response(&session_id, RoundEvent::Text(visible)));
    }

    // Mirror the unified task list so resume restores the sticky panel. The
    // value is compared against the session's current list to skip the write
    // (and avoid an event-log entry) on turns where nothing changed — the
    // common case.
    //
    // Auto-clear: once every item reaches a terminal status (completed or
    // cancelled), the task is finished and the list is dropped so a done list
    // does not linger in the panel (and the prompt) indefinitely. An empty
    // list is a no-op here.
    let agent_todos = agent.todos();
    if !agent_todos.items.is_empty() && agent_todos.is_all_done() {
        agent.clear_todos();
        let _ = tx.send(round_response(
            &session_id,
            RoundEvent::TodosUpdated(neenee_core::TodoList::default()),
        ));
        if let Err(err) = session.set_todos(neenee_core::TodoList::default()).await {
            tracing::warn!(error = %err, "could not clear todos");
        }
    } else {
        let stored_todos = session.todos().await;
        if agent_todos != stored_todos
            && let Err(err) = session.set_todos(agent_todos).await
        {
            tracing::warn!(error = %err, "could not persist todos");
        }
    }

    // Mirror session-scoped runtime state to the durable session (ADR-0048
    // Phase 2): the disabled-tool mask and the round counter. Each is compared
    // against the durable value and skipped on a match to avoid a no-op
    // event-log entry (mirroring the todos
    // diff above).
    let agent_disabled = agent.disabled_tools_snapshot();
    if agent_disabled != session.disabled_tools().await
        && let Err(err) = session.set_disabled_tools(agent_disabled).await
    {
        tracing::warn!(error = %err, "could not persist disabled tools");
    }
    let agent_round = agent.round_count();
    if agent_round != session.round_counter().await
        && let Err(err) = session.set_round_counter(agent_round).await
    {
        tracing::warn!(error = %err, "could not persist round counter");
    }

    if emit_round_completed {
        let _ = tx.send(round_response(
            &session_id,
            RoundEvent::RoundCompleted(neenee_core::RoundSummary {
                round: agent_round,
                output_tokens: outcome.token_usage.completion_tokens.max(0) as u64,
                duration_ms: outcome.duration_ms,
                paused_ms: outcome.paused_ms,
                generation_ms: outcome.generation_ms,
            }),
        ));
    }
    Ok(())
}

fn send_context_projection(
    tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: &str,
    agent: &Agent,
    messages: &[Message],
) {
    let tokens = agent.estimate_next_request_tokens(messages).total_tokens;
    let _ = tx.send(round_response(
        session_id,
        RoundEvent::ContextTokens(neenee_core::ContextTokenSnapshot {
            tokens,
            source: neenee_core::ContextTokenSource::Projection,
        }),
    ));
}

async fn persist_request_usage(
    agent: &Agent,
    session: &SessionStore,
    session_id: &str,
) -> Result<(), String> {
    let Some(ledger) = agent.token_ledger() else {
        return Ok(());
    };
    session
        .set_request_usage_records(ledger.records_for_session(session_id))
        .await
}

pub fn retry_delay_ms(
    attempt: usize,
    retry_after_ms: Option<u64>,
    base_ms: u64,
    max_ms: u64,
) -> u64 {
    let exponent = attempt.saturating_sub(1).min(20) as u32;
    retry_after_ms
        .unwrap_or_else(|| base_ms.saturating_mul(2u64.saturating_pow(exponent)))
        .min(max_ms.max(1))
}

/// Apply "equal jitter" to a backoff delay, the variant recommended for
/// client-side retries: half the delay is fixed, the other half is randomised.
/// Unlike "full jitter" (`[0, base]`) it never collapses to a near-zero delay,
/// so a retry never fires immediately; unlike "no jitter" it still de-synchronises
/// clients that fail in unison (e.g. behind the same load balancer). The result
/// is always in `[base/2, base]`, so the configured upper bound is respected and
/// a server-supplied `Retry-After` (which bypasses `retry_delay_ms`) is honoured
/// while still being jittered to avoid thundering-herd on its expiry.
///
/// `roll` is an injected `[0, base] -> u64` closure so this stays a pure,
/// deterministic, unit-testable function; the only call site (`execute_round`)
/// supplies `fastrand`. A `base` of 0 is degenerate and passed through unchanged.
pub fn apply_jitter_ms(base: u64, roll: impl Fn(u64) -> u64) -> u64 {
    if base == 0 {
        return 0;
    }
    let half = base / 2;
    half + roll(base - half).min(base - half)
}

fn public_retry_reason(message: &str) -> String {
    let first = message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("transient provider error");
    const MAX_CHARS: usize = 96;
    if first.chars().count() <= MAX_CHARS {
        first.to_string()
    } else {
        let mut compact: String = first.chars().take(MAX_CHARS.saturating_sub(1)).collect();
        compact.push('…');
        compact
    }
}

pub fn relay_agent_event(
    tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: &str,
    event: AgentEvent,
    streamed_text: &std::sync::atomic::AtomicBool,
) {
    let response = match event {
        AgentEvent::Notice(notice) => round_response(session_id, RoundEvent::Notice(notice)),
        AgentEvent::ModelRequestStarted {
            round,
            turn,
            context_tokens,
        } => {
            // The projection is session-scoped and was computed at the exact
            // pre-wire boundary, after hooks and request preparation.
            let _ = tx.send(round_response(
                session_id,
                RoundEvent::ContextTokens(neenee_core::ContextTokenSnapshot {
                    tokens: context_tokens,
                    source: neenee_core::ContextTokenSource::Projection,
                }),
            ));
            // Structured turn signal first, so the Activity modal can show
            // `round N · turn M · waiting for model` with the turn as a
            // first-class field rather than text-mining it out of the status
            // string. The bare status follows as the `Activity` below.
            let _ = tx.send(round_response(
                session_id,
                RoundEvent::TurnStarted { round, turn },
            ));
            round_response(
                session_id,
                RoundEvent::Activity("waiting for model".to_string()),
            )
        }
        AgentEvent::ContextTokens(snapshot) => {
            round_response(session_id, RoundEvent::ContextTokens(snapshot))
        }
        AgentEvent::UserInputInserted(input) => {
            round_response(session_id, RoundEvent::UserInputInserted(input))
        }
        AgentEvent::AssistantDelta { delta, start } => {
            if start {
                let _ = tx.send(round_response(session_id, RoundEvent::StreamStart));
            }
            streamed_text.store(true, Ordering::SeqCst);
            round_response(session_id, RoundEvent::StreamDelta(delta))
        }
        AgentEvent::AssistantEnd(content) => round_response(
            session_id,
            RoundEvent::StreamEnd(content.trim().to_string()),
        ),
        AgentEvent::AssistantDiscard => round_response(session_id, RoundEvent::StreamDiscard),
        AgentEvent::ReasoningDelta { delta, start } => {
            if start {
                let _ = tx.send(round_response(session_id, RoundEvent::StreamStart));
            }
            streamed_text.store(true, Ordering::SeqCst);
            round_response(session_id, RoundEvent::StreamReasoningDelta(delta))
        }
        AgentEvent::ReasoningEnd(content) => {
            round_response(session_id, RoundEvent::StreamReasoningEnd(content))
        }
        AgentEvent::ToolCall {
            id,
            name,
            arguments,
        } => round_response(
            session_id,
            RoundEvent::ToolCall {
                id,
                name,
                arguments,
            },
        ),
        AgentEvent::ToolResult {
            id,
            name,
            output,
            structured,
            duration_ms,
        } => round_response(
            session_id,
            RoundEvent::ToolResult {
                id,
                name,
                output,
                structured,
                duration_ms,
            },
        ),
        AgentEvent::ToolCancelled { id, name } => {
            round_response(session_id, RoundEvent::ToolCancelled { id, name })
        }
        AgentEvent::ToolStream { id, stream } => {
            round_response(session_id, RoundEvent::ToolStream { id, stream })
        }
        AgentEvent::TodosUpdated(todos) => {
            round_response(session_id, RoundEvent::TodosUpdated(todos))
        }
        AgentEvent::AutopilotChanged(enabled) => {
            round_response(session_id, RoundEvent::AutopilotChanged(enabled))
        }
        AgentEvent::SessionReview { alert } => {
            if !alert.trim().is_empty() {
                let _ = tx.send(round_response(
                    session_id,
                    RoundEvent::Notice(
                        neenee_core::AgentNotice::new(
                            neenee_core::NoticeKind::ReviewAlert,
                            neenee_core::NoticeSeverity::Warning,
                            "Session review needs attention",
                            neenee_core::NoticeSource::Review,
                        )
                        .with_body(alert.clone())
                        .with_surface(neenee_core::NoticeSurface::Banner),
                    ),
                ));
            }
            round_response(session_id, RoundEvent::SessionReview { alert })
        }
        AgentEvent::PermissionRequest(request) => {
            round_response(session_id, RoundEvent::PermissionRequest(request))
        }
        AgentEvent::UserQuestionRequest(request) => {
            round_response(session_id, RoundEvent::UserQuestionRequest(request))
        }
        AgentEvent::InputRequest(request) => {
            round_response(session_id, RoundEvent::InputRequest(request))
        }
        AgentEvent::Envoy {
            parent_call_id,
            event,
        } => round_response(
            session_id,
            RoundEvent::Envoy {
                parent_call_id,
                event,
            },
        ),
    };
    let _ = tx.send(response);
}

pub async fn compact_round_history(
    history: &mut Vec<Message>,
    session: &SessionStore,
    settings: &ContextProjectionSettings,
    provider: Option<Arc<dyn Provider>>,
    extra_context: Vec<String>,
) -> Result<Option<ContextProjectionCheckpoint>, String> {
    // Skip the model call entirely when summarization is disabled; the excerpt
    // fallback inside `run_compaction` still produces a checkpoint.
    let provider = if settings.summarize { provider } else { None };
    let Some(result) = run_compaction(
        history,
        settings.budget.target_tokens,
        settings.preserve_rounds,
        provider,
        extra_context,
    )
    .await?
    else {
        return Ok(None);
    };
    let checkpoint = result.checkpoint.clone();
    session.commit_context_projection(result).await?;
    Ok(Some(checkpoint))
}

/// Prune old tool results in place and durably commit the change. Pruning is an
/// implicit model-context projection step: it keeps the conversation and the
/// `tool_call_id` chain intact (only stale tool *bodies* are cleared), so unlike
/// a summarizing compaction it does **not** surface a transcript notice — it
/// only records a durable checkpoint and a `debug` trace for observability.
pub async fn prune_and_commit(
    history: &mut [Message],
    session: &SessionStore,
    settings: &ContextProjectionSettings,
) -> Result<(), String> {
    let before_chars = estimate_bytes(history);
    let Some(outcome) = neenee_core::prune_tool_results(
        history,
        settings.prune_protect_chars,
        ContextProjectionSettings::PRUNE_MIN_RECLAIM_CHARS,
    ) else {
        return Ok(());
    };
    let after_chars = estimate_bytes(history);
    let checkpoint = ContextProjectionCheckpoint {
        operation: neenee_persistence::session::ContextProjectionKind::Prune,
        archived_messages: outcome.originals.len(),
        active_messages: history.len(),
        before_chars,
        after_chars,
    };
    tracing::debug!(
        pruned_tool_results = checkpoint.archived_messages,
        before_chars,
        after_chars,
        "pruned stale tool results"
    );
    session
        .commit_context_projection(ContextProjectionResult {
            model_window: history.to_owned(),
            archived_originals: outcome.originals,
            checkpoint,
        })
        .await
}

pub fn send_compaction(
    tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: &str,
    checkpoint: &ContextProjectionCheckpoint,
) {
    let _ = tx.send(round_response(
        session_id,
        RoundEvent::Compacted {
            archived_messages: checkpoint.archived_messages,
            before_chars: checkpoint.before_chars,
            after_chars: checkpoint.after_chars,
        },
    ));
}

// ── /repeat scheduler ─────────────────────────────────────────────────

/// One scheduler tick over the session's `/repeat` schedule: drop jobs older
/// than the max age, then dispatch every job whose `next_fire` is due,
/// advancing its schedule before enqueueing so a slow turn cannot cause a
/// double-fire.
///
/// Jobs are **session-scoped**: this ticks the one session the harness is
/// driving. Resume/fork carries the schedule because it lives on the session.
pub async fn run_repeat_tick(
    session: &SessionStore,
    tx: &mpsc::UnboundedSender<AgentRequest>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<usize, String> {
    let cutoff = now - chrono::Duration::days(DEFAULT_MAX_AGE_DAYS);
    let mut jobs = session.repeat_jobs().await;
    let initial_len = jobs.len();
    // Prune expired jobs (created too long ago) in place.
    jobs.retain(|j| j.created_at >= cutoff);

    let mut dispatched = 0;
    for job in jobs.iter_mut() {
        if job.next_fire > now {
            continue;
        }
        let next = match CronExpr::parse(&job.cron) {
            Ok(cron) => cron
                .next_fire(now)
                .unwrap_or(now + chrono::Duration::days(1)),
            Err(err) => {
                tracing::warn!(
                    "repeat job {} has unparseable cron '{}': {err}; skipping",
                    job.id,
                    job.cron
                );
                continue;
            }
        };
        job.last_fire = Some(now);
        job.next_fire = next;
        let _ = tx.send(AgentRequest::Chat {
            text: job.prompt.clone(),
            images: Vec::new(),
            sent_at_ms: None,
        });
        dispatched += 1;
    }
    // Only persist if the schedule actually mutated (job pruned or fired).
    if initial_len != jobs.len() || dispatched > 0 {
        session.set_repeat_jobs(jobs).await?;
    }
    Ok(dispatched)
}

/// Spawn the `/repeat` scheduler bound to `session`. Every `tick_interval` it
/// prunes expired jobs and fires any that are due, dispatching each prompt as
/// a normal `AgentRequest::Chat` round through `tx`.
pub fn start_repeat_scheduler(
    session: Arc<SessionStore>,
    tx: mpsc::UnboundedSender<AgentRequest>,
    tick_interval: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(tick_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let now = chrono::Utc::now();
            if let Err(err) = run_repeat_tick(&session, &tx, now).await {
                tracing::warn!("repeat scheduler tick failed: {err}");
            }
        }
    })
}

#[cfg(test)]
mod repeat_tests {
    use super::*;
    use chrono::TimeZone;
    use neenee_core::repeat::RepeatJob;

    /// Build an isolated in-memory session for scheduler tests.
    async fn fresh_session() -> SessionStore {
        let dir = std::env::temp_dir().join(format!(
            "neenee-repeat-session-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // `load_for_project` pins a fresh session file under the project dir.
        SessionStore::load_for_project(dir)
    }

    fn job(cron: &str, prompt: &str, next_fire: chrono::DateTime<chrono::Utc>) -> RepeatJob {
        RepeatJob {
            id: uuid::Uuid::new_v4().to_string(),
            cron: cron.to_string(),
            prompt: prompt.to_string(),
            created_at: chrono::Utc::now(),
            next_fire,
            last_fire: None,
        }
    }

    #[tokio::test]
    async fn tick_dispatches_and_advances_due_jobs() {
        let session = fresh_session().await;
        let now = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        // A job already due (next_fire == now).
        session
            .set_repeat_jobs(vec![job("* * * * *", "run tests", now)])
            .await
            .unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel::<AgentRequest>();

        let dispatched = run_repeat_tick(&session, &tx, now).await.unwrap();
        assert_eq!(dispatched, 1);

        // The prompt was enqueued as a chat round.
        match rx.recv().await {
            Some(AgentRequest::Chat { text, .. }) => assert_eq!(text, "run tests"),
            other => panic!("expected Chat, got {other:?}"),
        }
        // The job is no longer due at `now` (advanced to the next minute).
        let still_due = session.repeat_jobs().await;
        assert!(still_due.iter().all(|j| j.next_fire > now));
    }

    #[tokio::test]
    async fn tick_skips_unparseable_cron() {
        let session = fresh_session().await;
        let now = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        // A bogus cron can land here; the tick must skip it rather than panic.
        session
            .set_repeat_jobs(vec![job("not a cron", "p", now)])
            .await
            .unwrap();
        let (tx, _rx) = mpsc::unbounded_channel::<AgentRequest>();
        let dispatched = run_repeat_tick(&session, &tx, now).await.unwrap();
        assert_eq!(dispatched, 0);
    }
}
