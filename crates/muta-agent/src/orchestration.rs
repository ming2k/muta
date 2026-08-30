//! Turn-level orchestration policy on top of the `Agent` struct.
//!
//! `Agent` (in [`crate::agent`]) runs a single ReAct turn against a provider.
//! This module wraps every turn with the cross-cutting policy a frontend
//! cannot reasonably reimplement: context compaction (pre-turn and mid-turn
//! pruning), retry with exponential backoff, permission relay, and the
//! `/schedule` cron + countdown scheduler.
//!
//! Frontends drive the harness through [`execute_round`],
//! [`start_interactive_round`], and
//! [`start_schedule_scheduler`]. They own only the UI-specific input path (slash commands for the CLI, menus/dialogs for a
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
use muta_contracts::{
    AgentEvent, AgentRequest, AgentResponse, CronExpr, HarnessError, HarnessSnapshot, ImagePart,
    InjectionKind, LoopStatus, Message, ModelRequest, NoticeKind, NoticeSeverity, NoticeSource,
    NoticeSurface, Provider, ProviderStreamEvent, Role, RoundEvent, Schedule,
    repeat::DEFAULT_MAX_AGE_DAYS,
};
use muta_persistence::{
    CommitTurn,
    config::Config,
    session::{ContextProjectionCheckpoint, ContextProjectionResult, SessionStore, run_compaction},
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

/// Wall-clock now in Unix-epoch milliseconds. The timestamp source for
/// round-interrupt records (C11): it must be a payload field because event-log
/// compaction drops envelope timestamps.
pub(crate) fn unix_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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

    fn effort(&self) -> Option<muta_contracts::effort::Effort> {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .effort()
    }

    fn model_capabilities(&self) -> muta_contracts::ModelCapabilities {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .model_capabilities()
    }

    fn prompt_hints(&self) -> muta_contracts::ProviderPromptHints {
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

    fn take_last_usage(&self) -> Option<muta_contracts::TokenUsage> {
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
    if let Err(error) = muta_persistence::fsutil::atomic_write_bytes(&file, &bytes) {
        tracing::warn!(%error, file = %file.display(), "network capture write failed");
    }
    prune_capture_dir(&capture.dir);
}

/// How many capture files one directory keeps. A capture is the full request
/// context of one round-trip — on a long session each file is as big as the
/// context itself, so an armed `/debug trace` writing unbounded captures
/// grows the data dir faster than everything else combined. Debug data is
/// disposable by definition; the newest [`MAX_CAPTURE_FILES`] are plenty to
/// diagnose a provider issue.
const MAX_CAPTURE_FILES: usize = 50;

/// Delete the oldest captures beyond [`MAX_CAPTURE_FILES`]. Names sort
/// chronologically (timestamp first), so a name sort is an age sort.
fn prune_capture_dir(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (name.ends_with(".json") && e.file_type().is_ok_and(|t| !t.is_dir())).then_some(name)
        })
        .collect();
    if names.len() <= MAX_CAPTURE_FILES {
        return;
    }
    names.sort();
    let excess = names.len() - MAX_CAPTURE_FILES;
    for name in names.into_iter().take(excess) {
        let _ = std::fs::remove_file(dir.join(name));
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
    pub budget: muta_contracts::ContextBudget,
    pub preserve_rounds: usize,
    /// Use the active model to produce an anchored structured summary.
    pub summarize: bool,
    /// Enable cheap tool-result pruning (pre-turn and mid-turn).
    pub prune: bool,
    /// Token budget of the most recent tool results protected from pruning
    /// (ADR-0120: token-native; the config key was already tokens, the old
    /// `× CHARS_PER_TOKEN` conversion existed only to feed a char-space
    /// pruner).
    pub prune_protect_tokens: usize,
}

impl ContextProjectionSettings {
    /// Mid-turn pruning only fires when it can reclaim at least this many
    /// tokens, to avoid pruning churn for negligible gains.
    pub const PRUNE_MIN_RECLAIM_TOKENS: usize = 2_000;

    /// Resolve settings for the active model's context window. `window_tokens`
    /// is the live model's context window (tokens); `0` means unknown and the
    /// policy's fallback window is substituted.
    pub fn from_config(config: &Config, window_tokens: usize) -> Self {
        Self {
            budget: config.compaction.resolve(window_tokens),
            preserve_rounds: config.compaction.preserve_rounds,
            summarize: config.compaction.summarize,
            prune: config.compaction.prune,
            prune_protect_tokens: config.compaction.prune_protect_tokens,
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
            budget: muta_contracts::CompactionPolicy::default().resolve(200_000),
            preserve_rounds: 6,
            summarize: true,
            prune: true,
            prune_protect_tokens: 24_000,
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
    pub prune_protect_tokens: usize,
    /// Shared content-addressed weights cache (from the agent): the post-prune
    /// session-weight estimate walks it instead of re-tokenizing the whole
    /// window, and runs on the blocking pool — same discipline as
    /// `estimate_session_weight_off_executor`.
    pub weights: Arc<muta_contracts::MessageTokenWeights>,
}

#[async_trait]
impl crate::ContextProjectionGate for MidTurnPruneProjectionGate {
    async fn project_context(&self, messages: Vec<Message>) -> Option<Vec<Message>> {
        let mut messages = messages;
        let outcome = muta_contracts::prune_tool_results(
            &mut messages,
            self.prune_protect_tokens,
            ContextProjectionSettings::PRUNE_MIN_RECLAIM_TOKENS,
        )?;
        let window_tokens_after =
            estimate_session_weight_off_executor(Arc::clone(&self.weights), &messages).await;
        let checkpoint = ContextProjectionCheckpoint {
            operation: muta_persistence::session::ContextProjectionKind::Prune,
            archived_messages: outcome.originals.len(),
            active_messages: messages.len(),
            window_tokens_before: window_tokens_after + outcome.reclaimed_tokens,
            window_tokens_after,
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
/// status, delegated flag, retry affordance) to the UI.
///
/// `retry_pending` mirrors the session's durable `/retry` resume point when
/// the harness is idle (the only state in which `/retry` is answerable). For
/// a *running* snapshot the affordance is definitionally off — the parked
/// round is executing — so the flag is forced `false` regardless of what a
/// racing store read says, keeping the contract "running ⇒ no retry hint".
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
            delegated: agent.delegated(),
            unconfined: agent.is_unconfined(),
            workspace_security: agent.workspace_security(),
            retry_pending: false,
        }),
    ));
}

/// [`send_harness_state`] with the session in hand, so an idle snapshot can
/// carry the authoritative `/retry` affordance (`retry_pending`) straight
/// from the durable resume point. Called on the round-task tail and the
/// session-switch reconciles — the two sites that know both the agent and
/// the store it is currently bound to.
pub async fn send_harness_state_for_session(
    tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: &str,
    agent: &Agent,
    session: &SessionStore,
    loop_status: LoopStatus,
) {
    // Running snapshots are emitted after lifecycle admission but
    // immediately before `execute_round` performs the counter bump. Project
    // that admitted round here so frontends receive the authoritative display
    // value without locally guessing from transcript length.
    let round_counter = agent
        .round_count()
        .saturating_add(u64::from(!loop_status.is_idle()));
    let retry_pending = loop_status.is_idle() && session.retry_pending().await.is_some();
    let _ = tx.send(round_response(
        session_id,
        RoundEvent::HarnessState(HarnessSnapshot {
            loop_status,
            round_counter,
            delegated: agent.delegated(),
            unconfined: agent.is_unconfined(),
            workspace_security: agent.workspace_security(),
            retry_pending,
        }),
    ));
    if let Some(performance) =
        muta_contracts::latest_turn_performance(&session.request_usage_records().await)
    {
        let _ = tx.send(round_response(
            session_id,
            RoundEvent::TurnPerformance(performance),
        ));
    }
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

/// Which kind of round `execute_round` is about to run — a fresh round, or
/// the `/retry` resume of a stopped one. See ADR-0128.
#[derive(Clone, Debug)]
pub enum RoundDriver {
    /// A normal round: a new prompt is admitted, the round counter bumps, and
    /// turn numbering starts at 0.
    Fresh,
    /// `/retry`: the round that stopped before completing continues *as
    /// itself*. The counter must not bump, turns number onward from the
    /// committed count, and the history is re-seeded from the checkpoint.
    Resume {
        /// The durable resume point captured when the round stopped.
        point: muta_contracts::RetryPoint,
    },
}

pub struct RoundInput {
    pub prompt: String,
    pub hidden: bool,
    pub display_prompt: Option<String>,
    /// Exact TUI send time for user-authored messages, in Unix-epoch milliseconds.
    pub sent_at_ms: Option<u64>,
    /// Inline images pasted into the prompt, attached to the user message.
    pub images: Vec<ImagePart>,
    /// Which round this input drives: a fresh prompt or a `/retry` resume of
    /// the stopped round. Replaces the old boolean: a resume carries its
    /// checkpoint, which is what makes the round "complete itself" instead of
    /// starting over.
    pub driver: RoundDriver,
}

impl RoundInput {
    /// Shorthand for the common fresh-prompt construction sites.
    pub fn fresh(prompt: String) -> Self {
        Self {
            prompt,
            hidden: false,
            display_prompt: None,
            sent_at_ms: None,
            images: Vec::new(),
            driver: RoundDriver::Fresh,
        }
    }

    /// Shorthand for the `/retry` resume construction site.
    pub fn resume(point: muta_contracts::RetryPoint) -> Self {
        Self {
            prompt: String::new(),
            hidden: false,
            display_prompt: None,
            sent_at_ms: None,
            images: Vec::new(),
            driver: RoundDriver::Resume { point },
        }
    }

    /// Whether this input bypasses the UserPromptSubmit hook gate. A resume
    /// re-sends the already-admitted request; the gate already ran when the
    /// stopped round was admitted.
    pub fn is_retry(&self) -> bool {
        matches!(self.driver, RoundDriver::Resume { .. })
    }
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
    // Snapshot the counter *before* `begin`: this is the number the round
    // this task is about to run will be admitted under (fresh rounds bump in
    // `execute_round`; a `/retry` resume keeps the stopped round's frozen
    // number, which is also what `round_count` returns until the resume
    // re-admits it). Capturing it here keeps the superseded predecessor's
    // tail — which runs concurrently with this round's own bump — from
    // misattributing its interrupt record to whichever round owns the live
    // counter at tail time.
    let round_at_admission = context.agent.round_count();
    let RoundBegin {
        token,
        generation,
        previous,
    } = context.lifecycle.begin().await;
    if let Some(previous) = previous {
        // A newer round is replacing the still-live predecessor: park the
        // superseded reason *before* cancelling so the predecessor's tail can
        // label its own unwind (C11). Without this the stale round's
        // generation-guarded cleanup is silent and leaves no trace.
        //
        // The park is stamped with the *superseding input's* send time when
        // the user authored one: the stop the marker describes is anchored to
        // that send (the predecessor was still running when it left the
        // composer), and the resume seam-merge places the marker before the
        // first user message sent later than `at_ms` — a tail-time or
        // park-time clock read can land a few milliseconds *after* the send,
        // dropping the marker below the newer round's answer where it reads
        // as an interrupt of a round that completed normally. A hidden or
        // clock-less input falls back to the park moment.
        context.lifecycle.record_interrupt_at(
            muta_contracts::RoundInterruptReason::Superseded,
            input.sent_at_ms,
        );
        context.agent.reject_pending_permissions();
        context.agent.reject_pending_user_questions();
        context.agent.reject_pending_inputs();
        let _ = context.tx.send(AgentResponse::PermissionsCleared);
        previous.cancel();
    }
    let (stale_steer, stale_follow_up) = context
        .agent
        .begin_session_queues(context.session_id.clone(), generation);
    for stale in stale_steer.into_iter().chain(stale_follow_up) {
        let _ = context.tx.send(round_response(
            &context.session_id,
            RoundEvent::SteerUnavailable { input_id: stale.id },
        ));
    }
    let _ = context.tx.send(round_response(
        &context.session_id,
        RoundEvent::Activity("starting request".to_string()),
    ));
    // The spawned tail records the round-interrupt into its own store handle
    // (C11); `RoundContext` consumes `context.session`, so keep an extra Arc.
    let session_for_tail = Arc::clone(&context.session);

    tokio::spawn(async move {
        // Supervised round task: the tail below (close_user_input_round →
        // terminal event → lifecycle.finish → Idle) must run even when the
        // round body panics. Before this wrapper, a panic skipped all of it:
        // `RoundLifecycle::is_running()` stayed true forever (monitor rows
        // and `/btw` banners stuck on "Running") and parked user-input
        // requests were never resolved. The panic is converted into an
        // ordinary `HarnessError::Other` so the existing error mapping emits
        // a visible `RoundEvent::Error` instead of silence.
        send_harness_state(
            &context.tx,
            &context.session_id,
            &context.agent,
            LoopStatus::Running,
        );
        let result = {
            let round_fut = execute_round(
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
            );
            match futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(round_fut)).await {
                Ok(result) => result,
                Err(payload) => {
                    let detail = payload
                        .downcast_ref::<&str>()
                        .map(|s| (*s).to_string())
                        .or_else(|| payload.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "non-string payload".to_string());
                    tracing::error!(panic = %detail, "agent round panicked");
                    Err(HarnessError::Other(format!(
                        "internal error: agent round panicked: {detail}"
                    )))
                }
            }
        };
        let (pending_steer, pending_follow_up) = context.agent.close_session_queues(generation);
        for pending in pending_steer.into_iter().chain(pending_follow_up) {
            let _ = context.tx.send(round_response(
                &context.session_id,
                RoundEvent::SteerUnavailable {
                    input_id: pending.id,
                },
            ));
        }
        let is_current = context.lifecycle.is_current(generation);
        // Consume the reason parked by whichever stop site cancelled this
        // round (C11). Taken before the match so every interrupted arm —
        // including the generation-suppressed one below — sees it. But a
        // parked reason alone does not mean the round *stopped*: a stop site
        // parks unconditionally (even while idle), and a late Esc Esc can
        // land after the round already passed its last cancellation
        // checkpoint. Only an actually-stopped round keeps its record —
        // a natural completion (`Ok(Completed)`) and a hook-denied prompt
        // (`Ok(NotStarted)`) are successes, not interrupts, and must not be
        // projected back as `▲ interrupted · <reason>` on resume.
        let stopped = match &result {
            Ok(RoundCompletion::Completed) | Ok(RoundCompletion::NotStarted) => false,
            Ok(RoundCompletion::Unsent) | Err(_) => true,
        };
        let interrupt_record = if stopped {
            // Attribution: this round's own admitted number, not the live
            // agent counter — by the time a superseded round's tail runs
            // here, the superseding round has already bumped that counter,
            // and stamping `round N+1` over round N's stop read as an
            // interrupt of the wrong (normally completed) round. The counter
            // is only read for error outcomes: a round that never got far
            // enough to know its own number (and never will — it is dead)
            // has no honest number to claim.
            context
                .lifecycle
                .take_interrupt()
                .map(|parked| muta_contracts::RoundInterrupt {
                    reason: parked.reason,
                    at_ms: parked.at_ms,
                    round: result.as_ref().err().map(|_| round_at_admission),
                    detail: None,
                })
                .or_else(|| {
                    if let Err(error) = &result {
                        let at_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0);
                        Some(muta_contracts::RoundInterrupt {
                            reason: muta_contracts::RoundInterruptReason::Error,
                            at_ms,
                            round: Some(round_at_admission),
                            detail: Some(error.to_string()),
                        })
                    } else {
                        None
                    }
                })
        } else {
            // Success: drop whatever was parked so it cannot leak into a
            // later round either (defense in depth behind `begin`'s clear).
            context.lifecycle.take_interrupt();
            None
        };
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
        if let Some(record) = interrupt_record {
            // One record + one live event per *stopped* round. The visible
            // `[Interrupted]` arm above, the generation-suppressed supersede
            // arm (the silent `Err(_) => {}` above — previously no trace at
            // all), and the phase-1 unsend (which returns
            // `Ok(RoundCompletion::Unsent)` after emitting `UnsentInput`).
            // A round that completed naturally is deliberately excluded: its
            // history committed and `RoundCompleted` already told the story
            // — an interrupt record there would project a false
            // "▲ interrupted" marker into the resumed transcript. The record
            // is durable projection state; the live event lets every attached
            // frontend render the stop with its reason immediately.
            if let Err(error) = session_for_tail
                .record_round_interrupt(record.clone())
                .await
            {
                tracing::warn!(?error, "could not persist round interrupt record");
            }
            if record.reason != muta_contracts::RoundInterruptReason::Error {
                let _ = context.tx.send(round_response(
                    &context.session_id,
                    RoundEvent::RoundInterrupted(record),
                ));
            }
        }
        if context.lifecycle.finish(generation).await {
            // Idle snapshot with the session in hand: this is the exact
            // moment a just-failed round's `/retry` point (armed inside
            // `execute_round`'s error path, above) becomes visible to the
            // frontends, and equally the moment a just-completed round's
            // stale point stops being offered.
            send_harness_state_for_session(
                &context.tx,
                &context.session_id,
                &context.agent,
                &session_for_tail,
                LoopStatus::Idle,
            )
            .await;
        }
    });
}

/// Phase-1 unsend guard: is this interrupted round reversible at the
/// conversation layer?
///
/// The boundary is **the first observed content delta**, not the first
/// response packet on the wire. A round is unsendable exactly while it has
/// produced no observable commitment of its own — no streamed
/// text/reasoning delta (`streamed_text`) and no dispatched tool call
/// (`tool_activity`) — because only then can the harness restore the
/// conversation to its pre-send state without discarding committed content
/// or tool side effects. Both sentinels are monotonic across the round
/// (never reset between turns), so a guard that held for turn 1 continues
/// to hold through turns 2+; the moment either flips, the window closes
/// permanently.
///
/// Kept as a free function of raw sentinel values (not `&AtomicBool`) so
/// the invariant is directly unit-testable — see `phase1_guard_tests`.
/// Generic over the success payload so the guard is decoupled from what a
/// successful round carries (currently `RoundOutcome`).
fn is_phase1_unsend<T>(
    result: &Result<T, HarnessError>,
    streamed_text: bool,
    tool_activity: bool,
) -> bool {
    matches!(result, Err(HarnessError::Interrupted)) && !streamed_text && !tool_activity
}

/// How [`execute_round`] ended when it did **not** fail. The round task's
/// tail needs this distinction to decide whether a parked interrupt reason
/// describes a real stop: only [`RoundCompletion::Unsent`] is a round that
/// actually stopped (and was rewound); [`RoundCompletion::Completed`] is a
/// natural model convergence whose history committed, and
/// [`RoundCompletion::NotStarted`] never opened a round at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundCompletion {
    /// The round reached its terminal path naturally — the model stopped
    /// calling tools, the full history committed, and `RoundCompleted` was
    /// emitted. A stop site may still have parked a reason in the window
    /// after the last cancellation checkpoint (an Esc Esc that landed too
    /// late to change the outcome); that park describes a stop that never
    /// happened and must **not** produce an interrupt record.
    Completed,
    /// Phase-1 unsend: the round was interrupted before any observable
    /// commitment and the conversation was rewound to its pre-send state
    /// (`UnsentInput` emitted). This *is* a real stop and keeps its
    /// interrupt record.
    Unsent,
    /// A `UserPromptSubmit` hook denied the prompt: no round was opened, no
    /// model request was made. Nothing was interrupted, so no interrupt
    /// record applies.
    NotStarted,
}

pub async fn execute_round(
    context: RoundContext,
    mut input: RoundInput,
) -> Result<RoundCompletion, HarnessError> {
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
    // Bind accounting to the session that admitted this round. The master
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
    // context. Hidden control prompts and retries bypass the gate.
    if !input.hidden && !input.is_retry() {
        match agent.fire_user_prompt_submit(&input.prompt).await {
            crate::hooks::UserPromptVerdict::Deny(reason) => {
                let _ = tx.send(round_response(
                    &session_id,
                    RoundEvent::Text(format!("Prompt blocked by hook: {reason}")),
                ));
                return Ok(RoundCompletion::NotStarted);
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
    // A `/retry` resume is the exception: it continues the round that already
    // bumped (its number is frozen in the resume point), so the counter stays
    // put — the transcript keeps one contiguous `round N` band.
    let resumed_point = match input.driver {
        RoundDriver::Resume { point } => Some(point),
        RoundDriver::Fresh => None,
    };
    if resumed_point.is_none() {
        agent.bump_round();
        // A freshly admitted round supersedes whatever was parked: the user
        // moved on (new prompt, `/compact` follow-up, scheduled job), so any
        // older `/retry` point is stale by construction and must stop being
        // offered (ADR-0128).
        if let Err(error) = session.clear_retry_pending().await {
            tracing::warn!(%error, "could not clear stale retry point on fresh round");
        }
    }
    let admitted_round = resumed_point
        .as_ref()
        .map(|point| point.round)
        .unwrap_or_else(|| agent.round_count());

    let admitted_session_id = session.id().await;
    // Build `round_history` — the round's working scratch — from the session's
    // authoritative `model_window` plus the new user message (ADR-0048). A
    // `/retry` resume instead re-seeds from the stopped round's checkpoint
    // watermark (see the branch below) and never pushes a user message.
    let unsent_prompt = input.prompt.clone();
    let unsent_images = input.images.clone();

    let mut round_history = if let Some(point) = resumed_point.as_ref() {
        // `/retry` (ADR-0128): re-seed the round's history from the durable
        // checkpoint the stopped round left behind. The window may have moved
        // since (a compaction, a `/btw` aside sharing the store — none apply
        // to a parked round, but the clamp keeps the invariant anyway), so
        // the watermark is a *cap*: never re-send content the stopped round
        // never committed (a partially streamed response was already
        // discarded with `StreamDiscard` before the point was armed).
        let window = session.model_window().await;
        let watermark = point.history_watermark.min(window.len());
        window[..watermark].to_vec()
    } else {
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

    // Session digest (ADR-0022 evolution): the first admitted user round
    // starts Chronicler work immediately — the opening request alone names
    // the session's title and intent — and later rounds refresh the digest
    // once the transcript has grown past its stored anchor, so the picker's
    // detail view stays a faithful working-memory projection. Hidden
    // control rounds and `/retry` resumes never trigger it.
    if !input.hidden && resumed_point.is_none() {
        maybe_refresh_session_digest(agent.clone(), session.clone());
    }

    // Install the mid-round save point (ADR-0048) so every ReAct-turn boundary
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
                // One lock acquisition, one event batch, at most one snapshot
                // write per turn — the three mutations a turn produces (new
                // message tail, round counter, settled usage attempts) are a
                // single persistence transaction, not three full-snapshot
                // setters.
                let usage_records = ledger
                    .as_ref()
                    .map(|ledger| ledger.records_for_session(&session_id));
                let usage_slice = usage_records.as_deref().unwrap_or(&[]);
                session
                    .commit_turn(CommitTurn {
                        messages: &snapshot,
                        round_counter: Some(agent.round_count()),
                        usage_records: usage_slice,
                    })
                    .await
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
    //
    // The estimate is BPE tokenization over the whole prepared request —
    // real CPU-bound work — so it runs on the blocking pool, never on the
    // async executor (starving it stalls TUI rendering and stream forwarding).
    // First estimate of a session pays full price once; later passes reuse
    // the content-addressed weights cache (O(new bytes), not O(session)).
    let mut request_estimate = estimate_off_executor(&agent, &round_history).await;
    if projection.prune && request_estimate.total_tokens > projection.budget.prune_threshold_tokens
    {
        prune_and_commit(
            &mut round_history,
            &session,
            &projection,
            agent.token_weights_handle(),
        )
        .await?;
        request_estimate = estimate_off_executor(&agent, &round_history).await;
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
    let retry_limit = retry_max_attempts.clamp(1, 60);
    let mut compacted_after_overflow = false;
    // Keep the ReAct turn alive across network attempts. Completed prior turns
    // are already durably checkpointed above; retaining this state means a
    // retry resumes the pending provider request with the same history, guard
    // registry, hooks, and accounting instead of replaying side effects.
    //
    // A `/retry` resume (ADR-0128) re-seeds the state from the durable point
    // instead of starting at turn 0: the stopped round's committed turns stay
    // committed, so the resumed execution numbers its next turn M+1 and the
    // transcript's `round N · turn M` sequence is never broken. `attempt`
    // still restarts at 0 for the resume itself — the provider retry budget
    // is per user-visible attempt to complete the round, and the user asking
    // to retry is exactly a new budget.
    let mut streaming_round = match resumed_point.as_ref() {
        Some(point) => agent.resume_streaming_round(point),
        None => agent.begin_streaming_round(),
    };
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
            break Err(HarnessError::Other(message));
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
                muta_contracts::AgentNotice::new(
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
    if is_phase1_unsend(
        &result,
        streamed_text.load(Ordering::SeqCst),
        tool_activity.load(Ordering::SeqCst),
    ) {
        // The user message is the last entry in `round_history` (pushed before
        // the streaming round). Only a non-hidden round is unsendable: hidden
        // control prompts are harness-internal and should not be surfaced as
        // editable user input.
        if round_history
            .last()
            .is_some_and(|m| m.role == Role::User && !input.hidden)
        {
            round_history.pop();
            session.replace_messages(round_history).await?;
            agent.restore_round_count(previous_round);
            session.set_round_counter(previous_round).await?;
            // The round was unwound to its pre-send state — nothing of it
            // remains, so there is nothing for `/retry` to resume (ADR-0128).
            if let Err(error) = session.clear_retry_pending().await {
                tracing::warn!(%error, "could not clear retry point after unsend");
            }
            persist_request_usage(&agent, &session, &session_id).await?;
            send_context_projection(&tx, &session_id, &agent, &session.model_window().await);
            let _ = tx.send(round_response(
                &session_id,
                RoundEvent::UnsentInput {
                    prompt: unsent_prompt,
                    images: unsent_images,
                },
            ));
            return Ok(RoundCompletion::Unsent);
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
    // The round reached its terminal path with the full history committed.
    // Whatever stopped earlier no longer applies: a `/retry` resume that gets
    // here *completed* the round, so the parked point is retired (ADR-0128).
    // The phase-1 unsend already returned above; this is the natural-
    // completion branch. Best-effort — a persist failure must not fail the
    // round that already succeeded.
    if let Err(error) = session.clear_retry_pending().await {
        tracing::warn!(%error, "could not clear retry point after round completion");
    }
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(error) => {
            // The round failed terminally with committed history — this is
            // exactly the "/retry me" state (ADR-0128). Park the resume point
            // so the user's `/retry` continues *this* round: same number,
            // turns onward from what was committed, history at the watermark
            // the failed round durably left behind. Arming is unconditional
            // here (it overwrites any stale point for an older round), and
            // best-effort — a persist failure surfaces through the error
            // path below, not by failing the bookkeeping.
            let point = muta_contracts::RetryPoint {
                round: admitted_round,
                turns_committed: streaming_round.committed_turns(),
                history_watermark: session.model_window().await.len(),
                paused_ms: agent.round_paused_ms(),
                at_ms: unix_epoch_ms(),
            };
            if let Err(persist_error) = session.arm_retry_pending(point).await {
                tracing::warn!(%persist_error, "could not arm retry point");
            }
            return Err(error);
        }
    };

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
            RoundEvent::TodosUpdated(muta_contracts::TodoList::default()),
        ));
        if let Err(err) = session.set_todos(muta_contracts::TodoList::default()).await {
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
            RoundEvent::RoundCompleted(muta_contracts::RoundSummary {
                round: agent_round,
                output_tokens: outcome.token_usage.completion_tokens.max(0) as u64,
                duration_ms: outcome.duration_ms,
                paused_ms: outcome.paused_ms,
                generation_ms: outcome.generation_ms,
            }),
        ));
    }
    Ok(RoundCompletion::Completed)
}

/// Spawn the first-turn title generation when the session qualifies (ADR-0022
/// §Decision 1: only once, only when unlocked). Best-effort in every
/// direction: no title slot, provider failure, or a persist error all just
/// leave things as they were.
/// Transcript growth (in chars) that must accumulate since the last digest
/// before a later user round refreshes it. Small enough that a long
/// session's digest stays representative between resume points; large
/// enough that the Chronicler is not consulted on every message.
const DIGEST_REFRESH_DELTA_CHARS: usize = 8_000;

/// Pure refresh decision, split out so the throttle is unit-testable.
/// A session with no digest always needs one; a digest with a missing
/// anchor (legacy data) refreshes once to establish the watermark; an
/// anchored digest refreshes only after [`DIGEST_REFRESH_DELTA_CHARS`] of
/// new transcript.
fn digest_refresh_needed(
    digest: Option<&muta_contracts::SessionDigest>,
    anchor: Option<u64>,
    transcript_chars: usize,
) -> bool {
    match (digest, anchor) {
        (None, _) => true,
        (Some(_), None) => true,
        (Some(_), Some(anchor)) => {
            transcript_chars >= anchor.saturating_add(DIGEST_REFRESH_DELTA_CHARS as u64) as usize
        }
    }
}

/// Fire-and-forget digest maintenance, spawned off the round path (the
/// Chronicler carries its own 2.5s timeout and never blocks a round). Reads
/// the session's digest + anchor, consults [`digest_refresh_needed`], and
/// persists the revision plus the new anchor. A manual title lock only pins
/// the *title*: the digest's intent/history still refresh. Any failure keeps
/// the previous digest.
fn maybe_refresh_session_digest(agent: Arc<Agent>, session: Arc<SessionStore>) {
    tokio::spawn(async move {
        let (digest, anchor) = session.digest().await;
        let transcript = session.full_transcript().await;
        let transcript_chars = transcript
            .iter()
            .map(|message| message.content.chars().count())
            .sum::<usize>();
        if !digest_refresh_needed(digest.as_ref(), anchor, transcript_chars) {
            return;
        }
        let Some(next) = agent.generate_digest(&transcript, digest.as_ref()).await else {
            return; // provider unavailable/timeout: keep the previous digest
        };
        if let Err(error) = session
            .set_digest(Some(next.clone()), transcript_chars as u64)
            .await
        {
            tracing::warn!(%error, "could not persist session digest");
            return;
        }
        // The picker's title row mirrors the digest unless the user locked a
        // manual title (ADR-0022's lock rule, applied to the title field
        // only — the digest itself was already stored above).
        let (_, manual) = session.title().await;
        if !manual && let Err(error) = session.set_title(Some(next.title), false).await {
            tracing::warn!(%error, "could not persist digest-derived session title");
        }
    });
}

fn send_context_projection(
    tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: &str,
    agent: &Agent,
    messages: &[Message],
) {
    let estimate = agent.estimate_next_request_tokens(messages);
    let _ = tx.send(round_response(
        session_id,
        RoundEvent::ContextTokens(muta_contracts::ContextTokenSnapshot::from_estimate(
            estimate,
            muta_contracts::ContextTokenSource::Projection,
        )),
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

/// Run one full request estimate on the blocking pool. `Agent` is `Send +
/// Sync`; the model-request assembly and BPE tokenization are pure CPU work
/// over immutable inputs, so `spawn_blocking` is safe and keeps the async
/// executor free for stream forwarding and UI. Falls back to inline execution
/// only if the runtime is shutting down (the round is tearing down anyway).
async fn estimate_off_executor(agent: &Arc<Agent>, messages: &[Message]) -> RequestTokenEstimate {
    let agent = Arc::clone(agent);
    let snapshot = messages.to_vec();
    tokio::task::spawn_blocking(move || agent.estimate_next_request_tokens(&snapshot))
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "estimate task aborted; treating pressure as zero");
            RequestTokenEstimate::new(0, 0)
        })
}

/// Session-weight estimate (nested runner children included — the
/// pressure/prune number, **not** the wire estimate) on the blocking pool,
/// through the shared content-addressed weights cache. Companion to
/// [`estimate_off_executor`]: BPE tokenization never runs on the async
/// executor, and repeated passes pay O(new bytes), not O(session). A
/// panicked/aborted task reads as `0` (no pressure), matching
/// `estimate_off_executor`'s fallback.
async fn estimate_session_weight_off_executor(
    weights: Arc<muta_contracts::MessageTokenWeights>,
    messages: &[Message],
) -> usize {
    let snapshot = messages.to_vec();
    tokio::task::spawn_blocking(move || {
        muta_contracts::estimate_tokens_weighted(&snapshot, &weights)
    })
    .await
    .unwrap_or_else(|error| {
        tracing::warn!(%error, "session-weight estimate task aborted; treating as zero");
        0
    })
}

pub fn retry_delay_ms(
    attempt: usize,
    retry_after_ms: Option<u64>,
    base_ms: u64,
    max_ms: u64,
) -> u64 {
    let exponent = attempt.saturating_sub(1).min(20) as u32;
    let exp_backoff = base_ms.saturating_mul(2u64.saturating_pow(exponent));
    match retry_after_ms {
        Some(ms) => ms.max(base_ms).min(max_ms.max(1)),
        None => exp_backoff.min(max_ms.max(1)),
    }
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

pub fn public_retry_reason(message: &str) -> String {
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
                RoundEvent::ContextTokens(muta_contracts::ContextTokenSnapshot::new(
                    context_tokens,
                    muta_contracts::ContextTokenSource::Projection,
                )),
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
        AgentEvent::TurnPerformance(performance) => {
            round_response(session_id, RoundEvent::TurnPerformance(performance))
        }
        AgentEvent::ContextTokens(snapshot) => {
            round_response(session_id, RoundEvent::ContextTokens(snapshot))
        }
        AgentEvent::SteerAdmitted(input) => {
            round_response(session_id, RoundEvent::SteerAdmitted(input))
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
        AgentEvent::DelegatedChanged(enabled) => {
            round_response(session_id, RoundEvent::DelegatedChanged(enabled))
        }
        AgentEvent::UnconfinedChanged(enabled) => {
            round_response(session_id, RoundEvent::UnconfinedChanged(enabled))
        }
        AgentEvent::PermissionRequest(request) => {
            round_response(session_id, RoundEvent::PermissionRequest(request))
        }
        AgentEvent::UserQuestionRequest(request) => {
            round_response(session_id, RoundEvent::UserQuestionRequest(request))
        }
        AgentEvent::StdinRequest(request) => {
            round_response(session_id, RoundEvent::StdinRequest(request))
        }
        AgentEvent::Runner {
            parent_call_id,
            event,
        } => round_response(
            session_id,
            RoundEvent::EnvoyCompat {
                parent_call_id,
                event,
            },
        ),
        AgentEvent::BackgroundJobStarted(info) => {
            round_response(session_id, RoundEvent::BackgroundJobStarted(info))
        }
        AgentEvent::BackgroundJobProgress { job_id, line } => round_response(
            session_id,
            RoundEvent::BackgroundJobProgress { job_id, line },
        ),
        AgentEvent::BackgroundJobCompleted(outcome) => {
            round_response(session_id, RoundEvent::BackgroundJobCompleted(outcome))
        }
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
///
/// The before/after session-weight estimates (children included — the pressure
/// number, not the wire estimate) run through the shared weights cache on the
/// blocking pool, per the same executor discipline as `estimate_session_weight_off_executor`.
pub async fn prune_and_commit(
    history: &mut [Message],
    session: &SessionStore,
    settings: &ContextProjectionSettings,
    weights: Arc<muta_contracts::MessageTokenWeights>,
) -> Result<(), String> {
    let window_tokens_before =
        estimate_session_weight_off_executor(Arc::clone(&weights), history).await;
    let Some(outcome) = muta_contracts::prune_tool_results(
        history,
        settings.prune_protect_tokens,
        ContextProjectionSettings::PRUNE_MIN_RECLAIM_TOKENS,
    ) else {
        return Ok(());
    };
    let window_tokens_after =
        estimate_session_weight_off_executor(Arc::clone(&weights), history).await;
    let checkpoint = ContextProjectionCheckpoint {
        operation: muta_persistence::session::ContextProjectionKind::Prune,
        archived_messages: outcome.originals.len(),
        active_messages: history.len(),
        window_tokens_before,
        window_tokens_after,
    };
    tracing::debug!(
        pruned_tool_results = checkpoint.archived_messages,
        window_tokens_before,
        window_tokens_after,
        reclaimed_tokens = outcome.reclaimed_tokens,
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
            window_tokens_before: checkpoint.window_tokens_before,
            window_tokens_after: checkpoint.window_tokens_after,
        },
    ));
}

// ── /repeat scheduler ─────────────────────────────────────────────────

/// One scheduler tick over the session's scheduled-prompt list
/// (`/schedule` and the legacy `/repeat`):
///
/// - prune recurring jobs created more than `DEFAULT_MAX_AGE_DAYS` ago;
/// - dispatch every job whose `next_fire` is due; for **cron** jobs advance the
///   schedule *before* enqueueing (so a slow turn cannot cause a double-fire),
///   and for **once** jobs drop the job (it has fired).
///
/// Jobs are **session-scoped**: this ticks the one session the harness is
/// driving. Resume/fork carries the schedule because it lives on the session.
pub async fn run_schedule_tick(
    session: &SessionStore,
    tx: &mpsc::UnboundedSender<AgentRequest>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<usize, String> {
    let cutoff = now - chrono::Duration::days(DEFAULT_MAX_AGE_DAYS);
    let mut jobs = session.scheduled_jobs().await;
    let initial_len = jobs.len();
    // Prune expired *recurring* jobs (created too long ago). Once-jobs are
    // their own expiry — a past once-job is dropped when it fires below — so
    // they are exempt from the age cutoff.
    jobs.retain(|j| j.trigger.is_once() || j.created_at >= cutoff);

    let mut dispatched = 0;
    let mut keep = Vec::with_capacity(jobs.len());
    for mut job in jobs {
        if job.next_fire > now {
            keep.push(job);
            continue;
        }
        match &job.trigger {
            Schedule::Cron { cron } => {
                let next = match CronExpr::parse(cron) {
                    Ok(parsed) => parsed
                        .next_fire(now)
                        .unwrap_or(now + chrono::Duration::days(1)),
                    Err(err) => {
                        tracing::warn!(
                            "scheduled job {} has unparseable cron '{cron}': {err}; skipping",
                            job.id
                        );
                        // Keep the broken job so the user can see/cancel it.
                        keep.push(job);
                        continue;
                    }
                };
                // Deliver first, mutate second (ADR-0125): if the driver's
                // channel is gone (session suspended/killed/daemon draining),
                // the job must stay armed on disk instead of silently
                // consuming its fire. A dropped send used to advance
                // `next_fire` — and a dropped once-job is unrecoverable.
                if tx
                    .send(AgentRequest::Prompt {
                        text: job.prompt.clone(),
                        images: Vec::new(),
                        sent_at_ms: None,
                    })
                    .is_err()
                {
                    tracing::warn!(
                        job = %job.id,
                        "schedule dispatch failed (session harness gone); job stays armed"
                    );
                    keep.push(job);
                    continue;
                }
                job.last_fire = Some(now);
                job.next_fire = next;
                keep.push(job);
                dispatched += 1;
            }
            Schedule::Once { .. } => {
                // One-shot: deliver first, drop second — same ordering
                // invariant as the cron arm. An undeliverable once-job
                // stays armed for the next harness (a re-attached session
                // or a rehosting daemon) instead of vanishing.
                if tx
                    .send(AgentRequest::Prompt {
                        text: job.prompt.clone(),
                        images: Vec::new(),
                        sent_at_ms: None,
                    })
                    .is_err()
                {
                    tracing::warn!(
                        job = %job.id,
                        "schedule dispatch failed (session harness gone); once-job stays armed"
                    );
                    keep.push(job);
                    continue;
                }
                tracing::info!(job = %job.id, "scheduled once-job fired and removed");
                dispatched += 1;
            }
        }
    }

    // Only persist if the schedule actually mutated (job pruned/fired/dropped).
    if initial_len != keep.len() || dispatched > 0 {
        session.set_scheduled_jobs(keep).await?;
    }
    Ok(dispatched)
}

/// Spawn the scheduled-prompt scheduler bound to `session`. Every
/// `tick_interval` it prunes expired jobs and fires any that are due,
/// dispatching each prompt as a normal `AgentRequest::Chat` round through `tx`.
/// Drives both recurring `/schedule <cron>` jobs and one-shot
/// `/schedule <countdown|absolute-time>` jobs.
///
/// The loop runs until `teardown` fires (or forever when `None` is passed —
/// the process-lifetime shape a single-session frontend uses). The daemon
/// passes the session's own cancellation token so suspension/kill stops the
/// tick: previously the task leaked past teardown and kept ticking against a
/// dead channel every 30s for as long as the daemon lived.
pub fn start_schedule_scheduler(
    session: Arc<SessionStore>,
    tx: mpsc::UnboundedSender<AgentRequest>,
    tick_interval: std::time::Duration,
    teardown: Option<tokio_util::sync::CancellationToken>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(tick_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = ticker.tick() => {}
                _ = teardown_cancelled(&teardown) => break,
            }
            let now = chrono::Utc::now();
            if let Err(err) = run_schedule_tick(&session, &tx, now).await {
                tracing::warn!("schedule scheduler tick failed: {err}");
            }
        }
    })
}

/// Resolves the optional teardown future for the tick-loop `select!` arm.
/// `None` means "run for the process lifetime": a future that never resolves.
async fn teardown_cancelled(teardown: &Option<tokio_util::sync::CancellationToken>) {
    match teardown {
        Some(token) => token.cancelled().await,
        None => std::future::pending().await,
    }
}

/// Backoff schedule (ms) for supervised scheduler restarts after a panic.
const SCHEDULER_RESTART_BACKOFF_MS: [u64; 4] = [250, 1_000, 4_000, 15_000];

/// After this many supervised restarts the scheduler gives up (a job that
/// panics every tick is a bug; hot-restarting forever would spam the log).
const SCHEDULER_RESTART_LIMIT: usize = SCHEDULER_RESTART_BACKOFF_MS.len();

/// Panic-supervised variant of [`start_schedule_scheduler`]. The daemon hosts
/// long-lived sessions whose scheduled jobs (crons, countdowns) must survive
/// an internal error: before supervision, a panic inside `run_schedule_tick`
/// killed the scheduler task silently and every job in that session stopped
/// firing with nothing in the UI to say why. The supervised wrapper restarts
/// the tick loop with bounded backoff; a persistently panicking tick (the
/// same job blowing up every tick, say) gives up after
/// `SCHEDULER_RESTART_LIMIT` attempts rather than hot-restarting forever.
pub fn start_supervised_schedule_scheduler(
    session: Arc<SessionStore>,
    tx: mpsc::UnboundedSender<AgentRequest>,
    tick_interval: std::time::Duration,
    teardown: Option<tokio_util::sync::CancellationToken>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let session = std::sync::Arc::new(session);
        let tx = std::sync::Arc::new(tx);
        let mut attempt = 0usize;
        loop {
            let outcome = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(
                tick_loop(&session, &tx, tick_interval, teardown.clone()),
            ))
            .await;
            if outcome.is_ok() {
                return; // cancelled or otherwise finished cleanly
            }
            attempt += 1;
            if attempt > SCHEDULER_RESTART_LIMIT {
                tracing::error!(
                    attempts = attempt,
                    "schedule scheduler kept panicking; giving up (scheduled jobs in this session will no longer fire)"
                );
                return;
            }
            let backoff_ms = SCHEDULER_RESTART_BACKOFF_MS
                [(attempt - 1).min(SCHEDULER_RESTART_BACKOFF_MS.len() - 1)];
            tracing::warn!(
                attempt,
                backoff_ms,
                "schedule scheduler panicked; restarting with backoff"
            );
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)) => {}
                _ = teardown_cancelled(&teardown) => return,
            }
        }
    })
}

/// The scheduler's tick loop, factored out so the supervised wrapper can
/// re-enter it after a panic. Returns on teardown-cancellation; any other
/// exit is a panic unwinding through it.
async fn tick_loop(
    session: &Arc<SessionStore>,
    tx: &Arc<mpsc::UnboundedSender<AgentRequest>>,
    tick_interval: std::time::Duration,
    teardown: Option<tokio_util::sync::CancellationToken>,
) {
    let mut ticker = tokio::time::interval(tick_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = teardown_cancelled(&teardown) => return,
        }
        let now = chrono::Utc::now();
        if let Err(err) = run_schedule_tick(session, tx, now).await {
            tracing::warn!("schedule scheduler tick failed: {err}");
        }
    }
}

#[cfg(test)]
mod schedule_tests {
    use super::*;
    use chrono::TimeZone;
    use muta_contracts::ScheduledJob;

    /// Build an isolated in-memory session for scheduler tests.
    async fn fresh_session() -> SessionStore {
        let dir = std::env::temp_dir().join(format!(
            "muta-schedule-session-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // `for_path` pins the fresh session file (and its blobs) under the
        // throwaway dir — `load_for_project` would resolve the real XDG
        // project bucket and mint files under ~/.local/share/muta.
        SessionStore::for_path(dir.join("session.json"))
    }

    /// Build a cron `ScheduledJob` with an explicit `next_fire` (so the test
    /// controls exactly when it is due, independent of when the cron would
    /// naturally next fire). Works even for an intentionally-bad cron string.
    fn cron_job(
        cron: &str,
        prompt: &str,
        next_fire: chrono::DateTime<chrono::Utc>,
    ) -> ScheduledJob {
        ScheduledJob {
            id: uuid::Uuid::new_v4().to_string(),
            trigger: Schedule::Cron {
                cron: cron.to_string(),
            },
            prompt: prompt.to_string(),
            created_at: chrono::Utc::now(),
            next_fire,
            last_fire: None,
        }
    }

    #[tokio::test]
    async fn tick_dispatches_and_advances_due_cron_jobs() {
        let session = fresh_session().await;
        let now = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        // A job already due (next_fire == now).
        session
            .set_scheduled_jobs(vec![cron_job("* * * * *", "run tests", now)])
            .await
            .unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel::<AgentRequest>();

        let dispatched = run_schedule_tick(&session, &tx, now).await.unwrap();
        assert_eq!(dispatched, 1);

        // The prompt was enqueued as a prompt round.
        match rx.recv().await {
            Some(AgentRequest::Prompt { text, .. }) => assert_eq!(text, "run tests"),
            other => panic!("expected Prompt, got {other:?}"),
        }
        // The cron job survives and is no longer due at `now`.
        let after = session.scheduled_jobs().await;
        assert_eq!(after.len(), 1);
        assert!(after.iter().all(|j| j.next_fire > now));
    }

    #[tokio::test]
    async fn tick_fires_once_job_and_drops_it() {
        let session = fresh_session().await;
        let now = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let once = ScheduledJob::once(
            "once1".into(),
            now,
            "one-shot reminder".into(),
            chrono::Utc::now(),
        );
        session.set_scheduled_jobs(vec![once]).await.unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel::<AgentRequest>();

        let dispatched = run_schedule_tick(&session, &tx, now).await.unwrap();
        assert_eq!(dispatched, 1);
        match rx.recv().await {
            Some(AgentRequest::Prompt { text, .. }) => assert_eq!(text, "one-shot reminder"),
            other => panic!("expected Prompt, got {other:?}"),
        }
        // The once-job is removed after firing.
        assert!(session.scheduled_jobs().await.is_empty());
    }

    #[tokio::test]
    async fn tick_keeps_future_once_job() {
        let session = fresh_session().await;
        let now = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let future = now + chrono::Duration::hours(2);
        let once = ScheduledJob::once(
            "once1".into(),
            future,
            "later reminder".into(),
            chrono::Utc::now(),
        );
        session.set_scheduled_jobs(vec![once]).await.unwrap();
        let (tx, _rx) = mpsc::unbounded_channel::<AgentRequest>();

        let dispatched = run_schedule_tick(&session, &tx, now).await.unwrap();
        assert_eq!(dispatched, 0);
        // Still armed.
        assert_eq!(session.scheduled_jobs().await.len(), 1);
    }

    #[tokio::test]
    async fn tick_skips_unparseable_cron() {
        let session = fresh_session().await;
        let now = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        // A bogus cron can land here; the tick must skip it rather than panic.
        session
            .set_scheduled_jobs(vec![cron_job("not a cron", "p", now)])
            .await
            .unwrap();
        let (tx, _rx) = mpsc::unbounded_channel::<AgentRequest>();
        let dispatched = run_schedule_tick(&session, &tx, now).await.unwrap();
        assert_eq!(dispatched, 0);
    }

    /// Regression (ADR-0125): dispatch is deliver-first, mutate-second. A
    /// dead driver channel (session suspended/killed, daemon draining) used
    /// to consume the fire anyway — the cron advanced its `next_fire` and
    /// the once-job was dropped outright, so the prompt silently never ran
    /// and no later harness could recover it.
    #[tokio::test]
    async fn tick_keeps_jobs_armed_when_the_driver_channel_is_dead() {
        let session = fresh_session().await;
        let now = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let once = ScheduledJob::once(
            "once1".into(),
            now,
            "unrecoverable if dropped".into(),
            chrono::Utc::now(),
        );
        session
            .set_scheduled_jobs(vec![cron_job("* * * * *", "run tests", now), once])
            .await
            .unwrap();
        // Drop the receiver: the sender reports a closed channel, exactly
        // like a torn-down session harness whose driver is gone.
        let (tx, rx) = mpsc::unbounded_channel::<AgentRequest>();
        drop(rx);

        let dispatched = run_schedule_tick(&session, &tx, now).await.unwrap();
        assert_eq!(dispatched, 0, "nothing was delivered");
        // Both jobs stay armed with their original due time: the cron did
        // not advance and the once-job was not consumed.
        let after = session.scheduled_jobs().await;
        assert_eq!(after.len(), 2, "both jobs must stay armed");
        for job in &after {
            assert!(
                job.next_fire <= now,
                "job {} must keep its original due time",
                job.id
            );
        }
        assert!(
            after.iter().all(|j| j.last_fire.is_none()),
            "no fire may be recorded for an undelivered prompt"
        );
    }

    /// The scheduler loop stops when the session's teardown token fires —
    /// the leak this closes: an orphaned tick task kept touching the store
    /// every 30s for the daemon's remaining lifetime after suspension.
    #[tokio::test]
    async fn scheduler_stops_on_teardown() {
        let session = fresh_session().await;
        let (tx, _rx) = mpsc::unbounded_channel::<AgentRequest>();
        let teardown = tokio_util::sync::CancellationToken::new();
        let handle = start_schedule_scheduler(
            Arc::new(session),
            tx,
            std::time::Duration::from_millis(10),
            Some(teardown.clone()),
        );
        assert!(!handle.is_finished());
        teardown.cancel();
        // The loop observes the token on its next select arm (≤ a tick).
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(handle.is_finished());
    }
}

#[cfg(test)]
mod digest_tests {
    use super::*;
    use crate::AgentIdentity;
    use async_trait::async_trait;
    use muta_contracts::{Message, ModelRequest, ProviderStreamEvent, Role, SessionDigest};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A provider that answers only the non-streaming `chat` (the Chronicler's
    /// path) with a fixed digest JSON, counting consults so tests can assert
    /// the refresh throttle.
    struct DigestProvider {
        consults: AtomicUsize,
    }

    #[async_trait]
    impl muta_contracts::Provider for DigestProvider {
        async fn chat(&self, _request: ModelRequest) -> Result<Message, String> {
            self.consults.fetch_add(1, Ordering::SeqCst);
            Ok(Message::new(
                Role::Assistant,
                "{\"title\":\"Fixing the build\",\"intent\":\"User wants CI green.\",\"history\":[\"Reproduced the failing test\"]}",
            ))
        }
        async fn stream_chat(
            &self,
            _request: ModelRequest,
        ) -> Result<futures::stream::BoxStream<'static, Result<String, String>>, String> {
            Ok(Box::pin(futures::stream::empty()))
        }
        async fn stream_chat_events(
            &self,
            _request: ModelRequest,
        ) -> Result<futures::stream::BoxStream<'static, Result<ProviderStreamEvent, String>>, String>
        {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    async fn fresh_digest_session() -> Arc<SessionStore> {
        let dir = std::env::temp_dir().join(format!(
            "muta-digest-session-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(SessionStore::for_path(dir.join("session.json")))
    }

    /// The digest trigger is fire-and-forget (spawned); poll until it lands.
    async fn await_digest(session: &SessionStore) -> SessionDigest {
        for _ in 0..200 {
            let (digest, _) = session.digest().await;
            if let Some(digest) = digest {
                return digest;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("digest never landed");
    }

    /// The first-request trigger: a session with no digest gets one
    /// immediately, and the picker title mirrors it (not manual).
    #[tokio::test]
    async fn digest_generated_on_first_request_and_title_mirrors() {
        let session = fresh_digest_session().await;
        session
            .replace_messages(vec![Message::new(Role::User, "hello there")])
            .await
            .unwrap();
        let provider = Arc::new(DigestProvider {
            consults: AtomicUsize::new(0),
        });
        let agent = Arc::new(Agent::new(
            provider.clone(),
            Vec::new(),
            AgentIdentity::default(),
        ));

        maybe_refresh_session_digest(agent.clone(), session.clone());
        let digest = await_digest(&session).await;
        assert_eq!(digest.title, "Fixing the build");
        assert_eq!(digest.intent, "User wants CI green.");
        assert_eq!(digest.history.len(), 1);
        let (title, manual) = session.title().await;
        assert_eq!(title.as_deref(), Some("Fixing the build"));
        assert!(!manual);

        // No transcript growth since the anchor → the throttle skips the
        // Chronicler entirely.
        maybe_refresh_session_digest(agent, session.clone());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(provider.consults.load(Ordering::SeqCst), 1);
    }

    /// A manually locked title is never overwritten (ADR-0022 lock rule),
    /// but the digest's intent/history still refresh.
    #[tokio::test]
    async fn manual_title_lock_pins_only_the_title() {
        let session = fresh_digest_session().await;
        session
            .replace_messages(vec![Message::new(Role::User, "hello")])
            .await
            .unwrap();
        session
            .set_title(Some("My own title".into()), true)
            .await
            .unwrap();
        let agent = Arc::new(Agent::new(
            Arc::new(DigestProvider {
                consults: AtomicUsize::new(0),
            }),
            Vec::new(),
            AgentIdentity::default(),
        ));
        maybe_refresh_session_digest(agent, session.clone());
        let digest = await_digest(&session).await;
        assert_eq!(
            digest.title, "Fixing the build",
            "the digest itself stores its own title"
        );
        let (title, manual) = session.title().await;
        assert_eq!(title.as_deref(), Some("My own title"), "manual title wins");
        assert!(manual, "manual flag survives");
    }

    /// Growth past the stored anchor refreshes the digest (and only then).
    #[tokio::test]
    async fn digest_refreshes_after_growth_threshold() {
        let session = fresh_digest_session().await;
        session
            .replace_messages(vec![Message::new(Role::User, "start")])
            .await
            .unwrap();
        let provider = Arc::new(DigestProvider {
            consults: AtomicUsize::new(0),
        });
        let agent = Arc::new(Agent::new(
            provider.clone(),
            Vec::new(),
            AgentIdentity::default(),
        ));

        maybe_refresh_session_digest(agent.clone(), session.clone());
        await_digest(&session).await;
        assert_eq!(provider.consults.load(Ordering::SeqCst), 1);

        // Below-threshold growth: no refresh.
        session
            .replace_messages(vec![
                Message::new(Role::User, "start"),
                Message::new(Role::Assistant, "small step"),
            ])
            .await
            .unwrap();
        maybe_refresh_session_digest(agent.clone(), session.clone());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(provider.consults.load(Ordering::SeqCst), 1);

        // Past-threshold growth: exactly one refresh.
        let big = "x".repeat(DIGEST_REFRESH_DELTA_CHARS + 100);
        session
            .replace_messages(vec![
                Message::new(Role::User, "start"),
                Message::new(Role::Assistant, big),
            ])
            .await
            .unwrap();
        maybe_refresh_session_digest(agent, session.clone());
        // The digest from round one is already stored; wait for the second
        // consult itself.
        for _ in 0..200 {
            if provider.consults.load(Ordering::SeqCst) == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(provider.consults.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn refresh_decision_is_pure_and_predictable() {
        let digest = SessionDigest {
            title: "T".to_string(),
            intent: "I".to_string(),
            history: Vec::new(),
        };
        // No digest yet → always.
        assert!(digest_refresh_needed(None, None, 0));
        // Anchored digest → growth-gated.
        assert!(!digest_refresh_needed(Some(&digest), Some(1_000), 1_000));
        assert!(!digest_refresh_needed(
            Some(&digest),
            Some(1_000),
            DIGEST_REFRESH_DELTA_CHARS
        ));
        assert!(digest_refresh_needed(
            Some(&digest),
            Some(1_000),
            DIGEST_REFRESH_DELTA_CHARS + 1_000
        ));
        // Missing anchor (legacy data) → refresh once to establish it.
        assert!(digest_refresh_needed(Some(&digest), None, 0));
    }
}
#[cfg(test)]
mod phase1_guard_tests {
    use super::is_phase1_unsend;
    use muta_contracts::HarnessError;

    const OK: Result<(), HarnessError> = Ok(());
    const INTERRUPTED: Result<(), HarnessError> = Err(HarnessError::Interrupted);

    /// Only an interrupt opens the window at all: any other outcome (success
    /// or a provider error) is not an unsend, regardless of the sentinels.
    #[test]
    fn non_interrupt_outcomes_never_unsend() {
        assert!(!is_phase1_unsend(&OK, false, false));
        let other: Result<(), HarnessError> = Err(HarnessError::Other("boom".into()));
        assert!(!is_phase1_unsend(&other, false, false));
        let retryable: Result<(), HarnessError> = Err(HarnessError::Retryable {
            message: "overload".into(),
            retry_after_ms: None,
        });
        assert!(!is_phase1_unsend(&retryable, false, false));
    }

    /// The happy path: interrupted with neither sentinel flipped — the
    /// request was in flight, no content delta ever reached the client.
    #[test]
    fn interrupted_before_any_output_unsends() {
        assert!(is_phase1_unsend(&INTERRUPTED, false, false));
    }

    /// The first streamed content delta closes the window: once the client
    /// has observed model output, the round is no longer conversation-
    /// reversible and must fall through to the Phase-2 drop path instead.
    #[test]
    fn first_content_delta_closes_the_window() {
        assert!(!is_phase1_unsend(&INTERRUPTED, true, false));
    }

    /// A dispatched tool call closes the window even with no streamed text:
    /// tool execution is a real-world side effect the unsend cannot undo.
    #[test]
    fn any_tool_activity_closes_the_window() {
        assert!(!is_phase1_unsend(&INTERRUPTED, false, true));
        assert!(!is_phase1_unsend(&INTERRUPTED, true, true));
    }
}

#[cfg(test)]
mod capture_prune_tests {
    use super::*;

    /// An armed `/debug trace` writes one full-context capture per round-trip
    /// and nothing ever removed them — on a long session the capture dir grew
    /// faster than every other data path combined. Retention keeps the newest
    /// [`MAX_CAPTURE_FILES`] and deletes older ones in age order.
    #[test]
    fn capture_prune_keeps_newest_max_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for i in 0..(MAX_CAPTURE_FILES + 5) {
            let name = format!("20260101-0000{i:02}.000_0001_anon_m.json");
            std::fs::write(root.join(name), b"x").unwrap();
        }
        // A foreign file must not count toward or be affected by retention.
        std::fs::write(root.join("notes.txt"), b"x").unwrap();

        prune_capture_dir(root);

        let mut remaining: Vec<String> = std::fs::read_dir(root)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        remaining.sort();
        assert_eq!(remaining.len(), MAX_CAPTURE_FILES + 1);
        assert!(remaining.contains(&"notes.txt".to_string()));
        // The five oldest captures are gone; the newest survives.
        assert!(!remaining.contains(&"20260101-000000.000_0001_anon_m.json".to_string()));
        assert!(remaining.contains(&format!(
            "20260101-0000{}.000_0001_anon_m.json",
            MAX_CAPTURE_FILES + 4
        )));
    }

    #[test]
    fn capture_prune_is_noop_at_or_below_limit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("20260101-000000.000_0001_anon_m.json"),
            b"x",
        )
        .unwrap();
        prune_capture_dir(dir.path());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
