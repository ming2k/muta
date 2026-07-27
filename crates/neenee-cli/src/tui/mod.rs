//! Terminal UI frontend, in three layers:
//!
//! - `neenee-tui-engine` — the in-house grid engine (a retained cell grid
//!   with dirty tracking and a back/front diff; ADR-0038) plus the crossterm
//!   backend.
//! - the view modules under this one — the drawing tree + semantic document
//!   model, painting `neenee_core` domain types into the engine grid:
//!   [`model`] (document, layout map for hit-testing, selection state),
//!   [`view`] (the transcript-area renderer the shell drives each frame), the
//!   drawing sub-trees ([`components`] / [`overlays`] / [`tools`] /
//!   [`disclosure`]), layout strategies ([`layout`]), and drawing leaves /
//!   shared tokens ([`theme`], [`design`], [`chrome`], [`composer`],
//!   [`primitives`], …).
//! - the app shell (this module's remaining submodules): application state
//!   ([`app`]), input mapping ([`input`]), and the event/render loop
//!   ([`event_loop`]). [`start_tui`] is the entry point wired by `main`.
//!
//! The seam between shell and view is the borrowed [`view::TranscriptView`]
//! the event loop fills in each frame; the view modules never reach back into
//! the shell.

pub mod app;
pub mod clipboard;
pub mod clipboard_ops;
pub mod completion;
pub mod composer_attachments;
pub mod config;
mod event_loop;
pub mod input;
pub mod interaction;
pub mod keymap;
pub mod question_model;
pub mod step_interaction;
mod terminal;
mod transcript;
mod versioned;

// ── View layer (merged from the former `neenee-tui-view` crate) ─────────────

// Semantic data model.
pub(crate) mod model;

// Drawing tree.
pub(crate) mod components;
pub(crate) mod disclosure;
pub(crate) mod layout;
pub(crate) mod overlays;
pub(crate) mod tools;

// Drawing leaves + shared tokens.
pub(crate) mod chrome;
pub(crate) mod composer;
pub(crate) mod design;
pub(crate) mod empty_state;
pub(crate) mod markdown_table;
pub(crate) mod message_body;
pub(crate) mod notice;
pub(crate) mod page_header;
pub(crate) mod primitives;
pub(crate) mod text_layout;
pub(crate) mod theme;
pub(crate) mod time;

// Transcript-area renderer (the entry point the shell drives each frame).
pub(crate) mod view;
// Re-export the transcript renderer's surface at the `tui` root: the drawing
// leaves used to reach these via the view crate's root namespace (its lib.rs
// glob), so this module now stands in as that parent.
pub(crate) use view::*;

// Misc helpers shared with the shell.
pub(crate) mod fuzzy;
pub(crate) mod modal;
pub(crate) mod providers;

#[cfg(test)]
mod snapshot_tests;

pub(crate) use app::{App, CaretOwner, ProviderDeleteChoice};
pub(crate) use completion::CompletionKind;
pub(crate) use modal::{ActivityTab, Modal, Recess};
pub(crate) use providers::{
    CustomField, PROVIDER_TEMPLATES, model_display_name, protocol_model_candidates,
    provider_template_label_for,
};

use crossterm::{
    event::{
        DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use neenee_core::{
    AgentRequest, AgentResponse, HarnessSnapshot, LoopStatus, Message, ParentStatus,
    PermissionRequest, ProviderPickerSnapshot, Role, RoundEvent, SessionContextSnapshot,
    SessionOverview, TodoList, UserQuestionRequest,
};
use neenee_tui_engine::{Backend, Terminal};
use std::{
    collections::{HashMap, VecDeque},
    error::Error,
    io,
    sync::Arc,
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::sync::{Mutex, mpsc};

use crate::tui::model::document::{
    MessageKind, NoticeSeverity, TranscriptMessage, UserMessageOrigin, notice_severity_from_core,
};
use crate::tui::model::layout::LayoutMap;
use crate::tui::model::selection::{SelectionDrag, SelectionState};
use crate::tui::transcript::{
    finalize_streaming_reasoning, rebase_transcript_rounds, transcript_messages_from_core,
};
use crate::tui::view::Theme;

use neenee_persistence::session::SessionStore;

/// Where the session this TUI drives actually lives.
///
/// The standalone path assembles the harness in-process and hands the TUI the
/// real [`SessionStore`]; attach mode (`neenee --attach`) drives a session
/// hosted by a separate `neenee-server` process over WebSocket and only knows
/// the session's id (learned from the handshake). Every TUI read of session
/// state goes through this enum so the two paths stay explicit.
pub(crate) enum SessionSource {
    /// Standalone: the in-process session store, shared with the driver.
    Local(Arc<SessionStore>),
    /// Attached: the session lives on the server; only its id is known here.
    Remote {
        /// The hosted session's id, learned from the WS handshake.
        session_id: String,
    },
}

impl SessionSource {
    /// The primary session id: awaited from the store when local, the
    /// handshake id when attached. Called once per frame by the event loop.
    pub(crate) async fn session_id(&self) -> String {
        match self {
            SessionSource::Local(store) => store.id().await,
            SessionSource::Remote { session_id } => session_id.clone(),
        }
    }

    /// The local store, when this TUI owns the session in-process. `None` in
    /// attach mode — features that need the store itself (`/serve`) are
    /// standalone-only and must gate on this before reaching for it.
    pub(crate) fn local_store(&self) -> Option<Arc<SessionStore>> {
        match self {
            SessionSource::Local(store) => Some(store.clone()),
            SessionSource::Remote { .. } => None,
        }
    }
}

/// Whether an inbound response is a high-frequency visual update that can wait
/// for the active 10fps render heartbeat. The listener still applies it and
/// marks the UI dirty immediately; it merely avoids waking the event loop for
/// every token. Starts, ends, errors, permissions, and tool lifecycle changes
/// remain immediate so the UI never feels unresponsive at a state boundary.
fn is_coalescible_stream_update(response: &AgentResponse) -> bool {
    matches!(
        response,
        AgentResponse::Round {
            event: RoundEvent::StreamDelta(_)
                | RoundEvent::StreamReasoningDelta(_)
                | RoundEvent::ToolStream { .. }
                | RoundEvent::Envoy {
                    event: neenee_core::EnvoyEvent::StreamDelta(_),
                    ..
                },
            ..
        }
    )
}

#[allow(clippy::too_many_arguments)]
pub async fn run_tui(
    tx: mpsc::UnboundedSender<AgentRequest>,
    mut rx: mpsc::UnboundedReceiver<AgentResponse>,
    initial_provider: String,
    initial_model: String,
    input_history: Vec<String>,
    initial_messages: Vec<Message>,
    initial_round_count: u64,
    custom_commands: Vec<(String, String)>,
    tui_config: config::TuiConfig,
    session: SessionSource,
    token_ledger: Option<Arc<neenee_core::TokenSourceLedger>>,
) -> Result<Vec<String>, Box<dyn Error>> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // Request the Kitty enhanced-keyboard protocol so modifier-bearing keys
    // that collide with legacy control bytes (notably Ctrl+M == Enter) are
    // reported distinctly. crossterm only emits the request when the terminal
    // advertises support, so this is a no-op elsewhere.
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )?;
    // The neenee-tui-engine engine owns its grid + diff + crossterm I/O directly. No
    // No ratatui, no WideHealBackend wrapper — the engine's retained grid writes
    // wide-glyph trailing cells with the glyph's own background at write time,
    // so ghost cells cannot occur regardless of terminal or multiplexer
    // (ADR-0038).
    let backend = Backend::new(stdout);
    let mut terminal = Terminal::new(backend);
    // Install the signal guard after the terminal enters raw mode + alt screen
    // so any later SIGTERM/SIGINT/SIGHUP restores it instead of stranding it.
    terminal::spawn_signal_guard();
    let tui_config = Arc::new(tui_config);
    let mut restored = transcript_messages_from_core(initial_messages, &tui_config);
    rebase_transcript_rounds(&mut restored, initial_round_count);
    let messages = Arc::new(versioned::Versioned::new(restored));
    let messages_clone = messages.clone();
    // Stage 3 redraw signal: the listener flips this on every handled response
    // so the event loop knows shared state changed and a frame is due. Starts
    // `true` so the very first frame always renders.
    let dirty = Arc::new(AtomicBool::new(true));
    let dirty_clone = dirty.clone();
    // Stage 4 wakeup: the listener notifies this so the loop's `select!` wakes
    // immediately on a response instead of waiting out a poll interval.
    let dirty_notify = Arc::new(tokio::sync::Notify::new());
    let dirty_notify_clone = dirty_notify.clone();
    let should_quit = Arc::new(AtomicBool::new(false));
    let should_quit_clone = should_quit.clone();

    let current_provider = Arc::new(Mutex::new(initial_provider.clone()));
    let current_model = Arc::new(Mutex::new(initial_model.clone()));
    let cp_clone = current_provider.clone();
    let cm_clone = current_model.clone();
    // Session-scoped AI context snapshot. The listener updates it only from
    // harness projection/API events; the rendered transcript is never used.
    let context_tokens = Arc::new(Mutex::new(HashMap::<
        String,
        neenee_core::ContextTokenSnapshot,
    >::new()));
    let context_tokens_clone = context_tokens.clone();

    // Per-session throughput summary for the most recent natural round, shown
    // in the TokenReport modal as an honest tokens/sec.
    let round_tps = Arc::new(Mutex::new(
        HashMap::<String, neenee_core::RoundSummary>::new(),
    ));
    let round_tps_clone = round_tps.clone();

    let is_responding = Arc::new(AtomicBool::new(false));
    let ir_clone = is_responding.clone();
    let harness = Arc::new(Mutex::new(HarnessSnapshot {
        loop_status: LoopStatus::Idle,
        round_counter: initial_round_count,
        unattended: false,
    }));
    let harness_clone = harness.clone();
    // Unified task list, mirrored from `AgentResponse::TodosUpdated`. Empty
    // (`None`) hides the panel.
    let todos: Arc<Mutex<Option<TodoList>>> = Arc::new(Mutex::new(None));
    let todos_clone = todos.clone();
    let round_count: Arc<Mutex<u64>> = Arc::new(Mutex::new(initial_round_count));
    let round_count_clone = round_count.clone();
    // Current ReAct turn within the active round. Reset to 0 at each round
    // boundary and bumped from `RoundEvent::TurnStarted`. The Activity
    // modal renders it as `turn M` alongside the round number.
    let current_turn: Arc<Mutex<u64>> = Arc::new(Mutex::new(0));
    let current_turn_clone = current_turn.clone();
    // Session-review alert (ADR-0016). Updated when a `SessionReview`
    // response lands; cleared (empty) on round reset so the activity bar's
    // `⚠ <alert>` segment clears between rounds.
    let review_alert: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let review_alert_clone = review_alert.clone();
    // Wall-clock instant the current round started. Stamped on a "running"
    // HarnessState so the activity bar can render a live `<elapsed>` segment.
    let round_started_at: Arc<Mutex<Option<std::time::Instant>>> = Arc::new(Mutex::new(None));
    let round_started_at_clone = round_started_at.clone();
    let activity_status = Arc::new(Mutex::new(String::new()));
    let activity_clone = activity_status.clone();
    let pending_permission = Arc::new(Mutex::new(VecDeque::<PermissionRequest>::new()));
    let pending_permission_clone = pending_permission.clone();
    let pending_question = Arc::new(Mutex::new(VecDeque::<UserQuestionRequest>::new()));
    let pending_question_clone = pending_question.clone();
    let pending_input = Arc::new(Mutex::new(VecDeque::<neenee_core::InputRequest>::new()));
    let pending_input_clone = pending_input.clone();
    // Full-duplex (ADR-0029): side-tables recording which envoy (by parent
    // tool-call id) surfaced a given permission / ask_user request, so the
    // modal's reply can be tagged with `parent_call_id` for down-routing.
    let envoy_permission_parent = Arc::new(Mutex::new(HashMap::<String, String>::new()));
    let subtask_permission_parent_clone = envoy_permission_parent.clone();
    let envoy_question_parent = Arc::new(Mutex::new(HashMap::<String, String>::new()));
    let subtask_question_parent_clone = envoy_question_parent.clone();
    let key_status = Arc::new(Mutex::new(HashMap::<String, bool>::new()));
    let key_status_clone = key_status.clone();
    let provider_picker = Arc::new(Mutex::new(ProviderPickerSnapshot::default()));
    let provider_picker_clone = provider_picker.clone();
    let sessions_overview = Arc::new(Mutex::new(Vec::<SessionOverview>::new()));
    let sessions_overview_clone = sessions_overview.clone();
    let open_sessions = Arc::new(AtomicBool::new(false));
    let open_sessions_clone = open_sessions.clone();
    let oauth_add_signal = Arc::new(Mutex::new(None::<event_loop::OauthAddSignal>));
    let oauth_add_signal_clone = oauth_add_signal.clone();
    // Mirror of `App::awaiting_oauth_add` so the response listener can tell the
    // add-flow (URL shown in the modal) from a reconnect (URL shown in the
    // transcript) and avoid duplicating the OAuth URL into the transcript.
    let awaiting_oauth_add = Arc::new(AtomicBool::new(false));
    let awaiting_oauth_add_clone = awaiting_oauth_add.clone();
    // Latest session-context snapshot for the Tools / Mcp / Skills /
    // Permissions managers (model / tools / permissions / skills / mcp).
    // Refreshed whenever a manager opens (the event loop sends
    // `QuerySessionContext`) and after any mutation the harness applies
    // (revoke / toggle). `None` until the first response lands.
    let session_context = Arc::new(Mutex::new(None::<SessionContextSnapshot>));
    let session_context_clone = session_context.clone();
    // Global tool-step density (true = Comfortable: new tool steps spawn
    // expanded). Shared with the response listener so steps created mid-turn
    // respect the user's last Ctrl+T choice (ADR-0001 Step 8).
    let tool_density = Arc::new(AtomicBool::new(false));
    let tool_density_clone = tool_density.clone();
    // TUI display config shared with the response listener so live tool steps
    // and reasoning traces honor the per-step-kind default expand state.
    let tui_config_clone = tui_config.clone();
    // `/btw` side-conversation shared state (ADR-0017). The side transcript
    // buffer, the parent-status mirror, and the one-shot view-transition
    // signal all cross the listener → loop boundary here.
    let side_messages = Arc::new(versioned::Versioned::new(Vec::<TranscriptMessage>::new()));
    let side_messages_clone = side_messages.clone();
    let parent_status = Arc::new(Mutex::new(ParentStatus::Idle));
    let parent_status_clone = parent_status.clone();
    let side_view_signal = Arc::new(Mutex::new(None::<event_loop::SideViewSignal>));
    let side_view_signal_clone = side_view_signal.clone();
    // Phase-1 unsend signal: set by the listener when the harness reports an
    // `UnsentInput`, drained by the event loop to restore the composer.
    let unsent_input_signal = Arc::new(Mutex::new(None::<event_loop::UnsentInput>));
    let unsent_input_signal_clone = unsent_input_signal.clone();
    let outbox_signals = Arc::new(Mutex::new(VecDeque::<event_loop::OutboxSignal>::new()));
    let outbox_signals_clone = outbox_signals.clone();

    // `/serve` hot-attach tap (ADR-0037 §7). `None` until `/serve <port>`
    // activates it. The response listener clones each `AgentResponse` into the
    // broadcast sender while it is `Some`; the event loop writes to it when the
    // user types `/serve`.
    let serve_tap: Arc<tokio::sync::Mutex<Option<tokio::sync::broadcast::Sender<AgentResponse>>>> =
        Arc::new(tokio::sync::Mutex::new(None));
    let serve_tap_for_listener = serve_tap.clone();
    let serve_tap_for_app = serve_tap.clone();

    // Spawn response listener
    tokio::spawn(async move {
        let mut reasoning_start: Option<std::time::Instant> = None;
        // Listener-local side routing key: the side `session_id` learned from
        // `SideViewOpened`. Kept here (not in `UiRuntime`) because only the
        // listener routes per-turn events; the loop reads the already-routed
        // `side_messages` buffer.
        let mut listener_side_id: Option<String> = None;
        // Per-session `(round, turn)` position. The primary and `/btw` side
        // sessions can stream concurrently, so a single global counter cannot
        // reliably stamp transcript components for semantic spacing.
        let mut positions_by_session = HashMap::<String, (u64, u64)>::new();
        // A session switch replaces the transcript before its authoritative
        // idle HarnessState arrives. Rebase the reconstructed tail exactly
        // once when that snapshot supplies the persisted round counter.
        let mut needs_round_rebase = false;
        while let Some(resp) = rx.recv().await {
            // Stage 3/4: any handled response can change shared state the loop
            // renders from, so signal a redraw. High-frequency stream deltas
            // deliberately do not wake the loop one-by-one: while responding,
            // its 10fps heartbeat coalesces them into a smooth stream without
            // repeatedly cloning and laying out a long transcript.
            dirty_clone.store(true, Ordering::Release);
            // A side conversation can receive stream deltas while the primary
            // activity indicator is idle. In that case there is no 10fps
            // heartbeat to flush the dirty bit, so retain the immediate wake.
            let defer_stream_wakeup =
                is_coalescible_stream_update(&resp) && ir_clone.load(Ordering::SeqCst);
            if !defer_stream_wakeup {
                dirty_notify_clone.notify_one();
            }
            // `/serve` hot-attach: clone the response into the broadcast
            // channel so WebSocket clients see the live stream. No-op when
            // serve is inactive (the lock holds None).
            if let Some(tx) = serve_tap_for_listener.lock().await.as_ref() {
                let _ = tx.send(resp.clone());
            }
            match resp {
                // ADR-0017: per-turn events arrive tagged with the session
                // they belong to. The listener routes each event to the side
                // buffer when its `session_id` matches the live side session,
                // and to the primary transcript otherwise. Permission and
                // user-question requests stay global so their modals surface
                // regardless of which view is focused.
                AgentResponse::Round { session_id, event } => {
                    let routes_to_side = listener_side_id.as_deref() == Some(session_id.as_str());
                    // Select the transcript buffer for this event (ADR-0017):
                    // the side buffer when the event's `session_id` matches the
                    // live side session, the primary buffer otherwise. Global
                    // responding/activity/harness state below is gated on
                    // `!routes_to_side` so a concurrent side round never
                    // clobbers the primary view's chrome; the side view reads
                    // its own buffer + the parent-status banner instead.
                    // Permission and user-question requests stay global
                    // regardless of origin so their modals always surface.
                    let buf = if routes_to_side {
                        &side_messages_clone
                    } else {
                        &messages_clone
                    };
                    match event {
                        RoundEvent::ContextTokens(snapshot) => {
                            context_tokens_clone
                                .lock()
                                .await
                                .insert(session_id.clone(), snapshot);
                        }
                        RoundEvent::UserInputInserted(input) => {
                            let input_id = input.id.clone();
                            let visible = input.display_text.unwrap_or(input.text);
                            let mut message = TranscriptMessage::new(Role::User, visible)
                                .with_origin(UserMessageOrigin::Insert);
                            message.sent_at_ms = input.sent_at_ms;
                            message.round = positions_by_session
                                .get(&session_id)
                                .copied()
                                .map(|(round, _)| round);
                            buf.write().await.push(message);
                            outbox_signals_clone.lock().await.push_back(
                                event_loop::OutboxSignal::Inserted {
                                    session_id,
                                    input_id,
                                },
                            );
                        }
                        RoundEvent::UserInputUnavailable { input_id } => {
                            outbox_signals_clone.lock().await.push_back(
                                event_loop::OutboxSignal::Unavailable {
                                    session_id,
                                    input_id,
                                },
                            );
                        }
                        RoundEvent::UserInputCancelled { input_id } => {
                            outbox_signals_clone.lock().await.push_back(
                                event_loop::OutboxSignal::Cancelled {
                                    session_id,
                                    input_id,
                                },
                            );
                        }
                        RoundEvent::UserInputCancelFailed { input_id } => {
                            outbox_signals_clone.lock().await.push_back(
                                event_loop::OutboxSignal::CancelFailed {
                                    session_id,
                                    input_id,
                                },
                            );
                        }
                        RoundEvent::NextRoundStarted(input) => {
                            let input_id = input.id.clone();
                            let visible = input.display_text.unwrap_or(input.text);
                            let mut message = TranscriptMessage::new(Role::User, visible);
                            message.sent_at_ms = input.sent_at_ms;
                            buf.write().await.push(message);
                            outbox_signals_clone.lock().await.push_back(
                                event_loop::OutboxSignal::NextRoundStarted {
                                    session_id,
                                    input_id,
                                },
                            );
                        }
                        RoundEvent::RoundCompleted(summary) => {
                            round_tps_clone
                                .lock()
                                .await
                                .insert(session_id.clone(), summary);
                            outbox_signals_clone
                                .lock()
                                .await
                                .push_back(event_loop::OutboxSignal::RoundCompleted { session_id });
                        }
                        RoundEvent::Notice(notice) => {
                            // Provider retry has a dedicated, self-refreshing
                            // transcript disclosure driven by RetryScheduled.
                            // Do not also degrade its toast into an appended
                            // inline notice on every failed attempt.
                            if notice.kind != neenee_core::NoticeKind::ProviderRetry {
                                let mut msgs = buf.write().await;
                                push_core_notice(&mut msgs, &notice);
                            }
                        }
                        RoundEvent::Text(t) => {
                            let (provider, model) =
                                event_loop::attribution(&cp_clone, &cm_clone).await;
                            let mut msgs = buf.write().await;
                            clear_provider_retry(&mut msgs);
                            let mut message = TranscriptMessage::new(Role::Assistant, t)
                                .with_attribution(provider, model);
                            if let Some((round, turn)) =
                                positions_by_session.get(&session_id).copied()
                            {
                                message.round = Some(round);
                                message.turn = Some(turn);
                            }
                            msgs.push(message);
                            if !routes_to_side {
                                ir_clone.store(false, Ordering::SeqCst);
                                activity_clone.lock().await.clear();
                            }
                        }
                        RoundEvent::Activity(status) => {
                            if !routes_to_side {
                                *activity_clone.lock().await = status;
                                ir_clone.store(true, Ordering::SeqCst);
                            }
                        }
                        RoundEvent::TurnStarted { round, turn } => {
                            let turn = turn as u64 + 1;
                            positions_by_session.insert(session_id.clone(), (round, turn));
                            {
                                // The composer cannot know the authoritative
                                // round until admission. Stamp its latest
                                // unpositioned driving prompt at this event.
                                let mut msgs = buf.write().await;
                                if let Some(prompt) = msgs.iter_mut().rev().find(|message| {
                                    message.role == Role::User
                                        && message.origin == UserMessageOrigin::Chat
                                        && message.round.is_none()
                                }) {
                                    prompt.round = Some(round);
                                }
                            }
                            if !routes_to_side {
                                *round_count_clone.lock().await = round;
                                // 1-indexed for display: turn 0 is the first
                                // model request, shown as `turn 1`.
                                *current_turn_clone.lock().await = turn;
                            }
                        }
                        RoundEvent::StreamStart => {
                            // A stream lifecycle event is not visible transcript content.
                            // Do not create an empty assistant placeholder here: reasoning-
                            // only streams (notably hidden-chain GPT models) may never emit
                            // visible text, and a zero-height message would still create a
                            // semantic layout boundary. The first visible delta lazily creates
                            // its own typed component instead. A successful stream does retire
                            // any transient provider-retry disclosure, independently of whether
                            // the model's first payload is visible.
                            {
                                let mut msgs = buf.write().await;
                                begin_stream(&mut msgs);
                            }
                            if !routes_to_side {
                                ir_clone.store(true, Ordering::SeqCst);
                                *activity_clone.lock().await = "responding".to_string();
                            }
                        }
                        RoundEvent::StreamDelta(delta) => {
                            let position = positions_by_session.get(&session_id).copied();
                            let round = position.map(|(round, _)| round);
                            let turn = position.map(|(_, turn)| turn);
                            let mut msgs = buf.write_streaming().await;
                            if let Some(id) =
                                append_stream_text_delta(&mut msgs, round, turn, &delta)
                            {
                                msgs.invalidate_message_height(id);
                                msgs.record_text_delta(id, delta);
                            } else {
                                // This is the first visible text in the ReAct turn.
                                // Upgrade to a structural write and create the transcript item
                                // from real content, never from a transport-level start signal.
                                drop(msgs);
                                let (provider, model) =
                                    event_loop::attribution(&cp_clone, &cm_clone).await;
                                let mut msgs = buf.write().await;
                                clear_provider_retry(&mut msgs);
                                let mut message = TranscriptMessage::new(Role::Assistant, delta)
                                    .with_attribution(provider, model);
                                if let Some((round, turn)) = position {
                                    message.round = Some(round);
                                    message.turn = Some(turn);
                                }
                                msgs.push(message);
                            }
                        }
                        RoundEvent::StreamEnd(final_content) => {
                            if !routes_to_side {
                                ir_clone.store(true, Ordering::SeqCst);
                                *activity_clone.lock().await = "finalizing response".to_string();
                            }
                            let position = positions_by_session.get(&session_id).copied();
                            let round = position.map(|(round, _)| round);
                            let turn = position.map(|(_, turn)| turn);
                            let mut msgs = buf.write().await;
                            clear_provider_retry(&mut msgs);
                            if let Some(message) = msgs.last_mut().filter(|message| {
                                message.role == Role::Assistant
                                    && matches!(&message.kind, MessageKind::Text)
                                    && message.round == round
                                    && message.turn == turn
                            }) {
                                message.raw = final_content;
                                message.reparse();
                            } else if !final_content.is_empty() {
                                // Defensive fallback for providers that deliver only a final
                                // payload without any preceding text delta.
                                let (provider, model) =
                                    event_loop::attribution(&cp_clone, &cm_clone).await;
                                let mut message =
                                    TranscriptMessage::new(Role::Assistant, final_content)
                                        .with_attribution(provider, model);
                                if let Some((round, turn)) = position {
                                    message.round = Some(round);
                                    message.turn = Some(turn);
                                }
                                msgs.push(message);
                            }
                        }
                        RoundEvent::StreamDiscard => {
                            let position = positions_by_session.get(&session_id).copied();
                            let round = position.map(|(round, _)| round);
                            let turn = position.map(|(_, turn)| turn);
                            let mut msgs = buf.write().await;
                            // With lazy stream-item creation, a hidden reasoning stream may
                            // have no visible message to discard. Never pop an assistant item
                            // from an earlier round merely because it happens to be last.
                            if msgs.last().is_some_and(|message| {
                                message.role == Role::Assistant
                                    && message.round == round
                                    && message.turn == turn
                            }) {
                                msgs.pop();
                            }
                        }
                        RoundEvent::UnsentInput { prompt, images } => {
                            // Phase-1 unsend: the harness cancelled the turn
                            // before any model output arrived and reverted the
                            // conversation context. Pop the user message we
                            // pushed into the transcript at send time and hand
                            // the prompt back to the loop via the unsend signal
                            // so it can restore the composer for re-editing.
                            {
                                let mut msgs = buf.write().await;
                                clear_provider_retry(&mut msgs);
                                if msgs.last().is_some_and(|m| m.role == Role::User) {
                                    msgs.pop();
                                }
                            }
                            if !routes_to_side {
                                // The round counter was bumped optimistically at
                                // send time (and again on the "running"
                                // HarnessState). Since nothing committed to the
                                // transcript, roll the counter back so the
                                // re-send reuses the same round number instead of
                                // skipping ahead. Matches the harness, which only
                                // `bump_round`s a number that actually ran.
                                let mut rc = round_count_clone.lock().await;
                                *rc = rc.saturating_sub(1);
                                *unsent_input_signal_clone.lock().await =
                                    Some(event_loop::UnsentInput { prompt, images });
                                ir_clone.store(false, Ordering::SeqCst);
                                activity_clone.lock().await.clear();
                            }
                        }
                        RoundEvent::StreamReasoningDelta(delta) => {
                            // Hidden-chain models (GPT-5.x, `ReasoningSummary`)
                            // surface only a reasoning summary, never their full
                            // chain. Disclosing even that summary as a
                            // `MessageKind::Thinking` message would leave a
                            // phantom entry that layout counts (`is_thinking()`)
                            // and selection math still see. Gate at message
                            // creation — the canonical point — so such models
                            // never produce a thinking message at all. The raw
                            // summary text is intentionally dropped: it is not a
                            // disclosed chain, and persisting it would resurrect
                            // the phantom on restore (see `transcript.rs`).
                            let hidden_chain = {
                                let model_id = cm_clone.lock().await.clone();
                                // `model_by_id` (not `resolve`) so unrecognized
                                // ids default to disclosed — `resolve` falls
                                // back to `ThinkingSupport::None` (chain not
                                // disclosed), which would drop reasoning deltas
                                // for local/user-defined models that reason.
                                // Only known `ReasoningSummary` models are gated.
                                !neenee_core::model_by_id(&model_id)
                                    .map(|m| m.thinking.chain_disclosed())
                                    .unwrap_or(true)
                            };
                            if hidden_chain {
                                continue;
                            }
                            // Reasoning traces do not have a `HeightCache`
                            // entry, so their high-frequency deltas can retain
                            // the ordinary text-message entries unchanged.
                            let mut msgs = buf.write_streaming().await;
                            let position = positions_by_session.get(&session_id).copied();
                            let round = position.map(|(round, _)| round);
                            let turn = position.map(|(_, turn)| turn);
                            let changed = if let Some(last) = msgs.last_mut().filter(|message| {
                                message.is_thinking()
                                    && message.round == round
                                    && message.turn == turn
                            }) {
                                last.push_stream(&delta);
                                if let MessageKind::Thinking { content, .. } = &mut last.kind {
                                    content.push_str(&delta);
                                }
                                Some(last.id)
                            } else {
                                // The first disclosed reasoning delta creates the visible
                                // reasoning component directly. `StreamStart` intentionally
                                // creates no transcript placeholder, so hidden-chain models
                                // cannot leave phantom spacing behind.
                                let (provider, model) =
                                    event_loop::attribution(&cp_clone, &cm_clone).await;
                                let mut thinking = TranscriptMessage::thinking(delta.clone())
                                    .with_attribution(provider, model);
                                if let Some((round, turn)) = position {
                                    thinking.round = Some(round);
                                    thinking.turn = Some(turn);
                                }
                                // A reasoning trace's default disclosure honors the
                                // `[tui.default_expanded] thinking` config (collapsed by
                                // default). On completion the transition leaves it as-is
                                // (no auto-collapse), so the user keeps what they were
                                // reading.
                                thinking.set_thinking_expanded(config::thinking_default_expanded(
                                    &tui_config_clone,
                                ));
                                msgs.push(thinking);
                                reasoning_start = Some(std::time::Instant::now());
                                None
                            };
                            if let Some(id) = changed {
                                msgs.record_reasoning_delta(id, delta);
                            } else {
                                msgs.require_transcript_snapshot();
                            }
                        }
                        RoundEvent::StreamReasoningEnd(content) => {
                            let duration_ms = reasoning_start
                                .take()
                                .map(|started| started.elapsed().as_millis() as u64);
                            let mut msgs = buf.write().await;
                            // The round closes with `AssistantEnd` *before* `ReasoningEnd`
                            // (see golden_reasoning_precedes_text_in_the_same_turn), so by
                            // the time this arrives the assistant's text message is usually
                            // the literal last message. Scan backward for the most recent
                            // Thinking message that is still streaming (`duration_ms: None`)
                            // instead of relying on it being last — otherwise the trace's
                            // duration never gets stamped and the spinner runs forever.
                            let target = msgs.iter_mut().rfind(|message| {
                                matches!(
                                    &message.kind,
                                    MessageKind::Thinking {
                                        duration_ms: None,
                                        ..
                                    }
                                )
                            });
                            if let Some(last) = target {
                                last.raw = content.clone();
                                last.reparse();
                                if let MessageKind::Thinking {
                                    content: current,
                                    duration_ms: d,
                                    ..
                                } = &mut last.kind
                                {
                                    *current = content;
                                    if d.is_none() {
                                        *d = Some(duration_ms.unwrap_or(0));
                                    }
                                }
                            }
                        }
                        RoundEvent::ToolCall {
                            id,
                            name,
                            arguments,
                        } => {
                            if !routes_to_side {
                                *activity_clone.lock().await =
                                    event_loop::tool_activity_status(&name).to_string();
                            }
                            let (provider, model) =
                                event_loop::attribution(&cp_clone, &cm_clone).await;
                            // Stamp the current ReAct turn so this step
                            // joins its compact sibling tool batch;
                            // `TurnStarted` has already populated the session
                            // position map.
                            let position = positions_by_session.get(&session_id).copied();
                            let sent_at_ms = event_loop::now_epoch_ms();
                            let mut msgs = buf.write().await;
                            clear_provider_retry(&mut msgs);
                            // A tool step starts collapsed: there's no result to show
                            // yet. The lifecycle-aware default (see `step_interaction`)
                            // expands it on completion — Ok follows per-tool density,
                            // Failed/Denied force-expand to surface the error.
                            let mut message = TranscriptMessage::tool_step(id, name, arguments)
                                .with_attribution(provider, model)
                                .with_sent_at_ms(sent_at_ms);
                            if let Some((round, turn)) = position {
                                message = message.with_round(round).with_turn(turn);
                            }
                            msgs.push(message);
                            if !routes_to_side {
                                ir_clone.store(true, Ordering::SeqCst);
                            }
                        }
                        RoundEvent::ToolResult {
                            id,
                            name,
                            output,
                            structured,
                            duration_ms,
                        } => {
                            if !routes_to_side {
                                *activity_clone.lock().await = "thinking".to_string();
                            }
                            let (provider, model) =
                                event_loop::attribution(&cp_clone, &cm_clone).await;
                            let density = tool_density_clone.load(Ordering::SeqCst);
                            let mut msgs = buf.write().await;
                            let mut finished = false;
                            for existing in msgs.iter_mut() {
                                if existing.finish_tool_step(
                                    &id,
                                    output.clone(),
                                    structured.clone(),
                                    duration_ms,
                                ) {
                                    // Apply the lifecycle-aware default disclosure: Ok
                                    // follows per-tool density, Failed/Denied force-
                                    // expand to surface the error. Respects any user
                                    // pin via the system setter.
                                    if let Some(status) = existing.tool_step_status() {
                                        let default = step_interaction::default_tool_expanded(
                                            status,
                                            &name,
                                            &tui_config_clone,
                                            density,
                                        );
                                        existing.set_tool_step_expanded(default);
                                    }
                                    finished = true;
                                    break;
                                }
                            }
                            if !finished {
                                // No matching in-flight call (e.g. turn restored from
                                // history): synthesize a finished step with its default
                                // disclosure applied directly.
                                let mut message =
                                    TranscriptMessage::tool_step(id.clone(), name.clone(), "{}")
                                        .with_attribution(provider, model);
                                if let Some((round, turn)) =
                                    positions_by_session.get(&session_id).copied()
                                {
                                    message.round = Some(round);
                                    message.turn = Some(turn);
                                }
                                message.finish_tool_step(&id, output, structured, duration_ms);
                                if let Some(status) = message.tool_step_status() {
                                    let default = step_interaction::default_tool_expanded(
                                        status,
                                        &name,
                                        &tui_config_clone,
                                        density,
                                    );
                                    message.set_tool_step_expanded(default);
                                }
                                msgs.push(message);
                            }
                        }
                        RoundEvent::ToolCancelled { id, .. } => {
                            // Convergence: an in-flight call was aborted by an
                            // interrupt. Flip its step (and any nested envoy
                            // children) to Cancelled so it never stays "running".
                            let mut msgs = buf.write().await;
                            let mut cancelled = false;
                            for message in msgs.iter_mut() {
                                if message.cancel_tool_step(&id) {
                                    // Cancelled reads as inert → collapse (respecting
                                    // any user pin via the system setter).
                                    message.set_tool_step_expanded(false);
                                    cancelled = true;
                                    break;
                                }
                            }
                            if !cancelled {
                                // The ToolCall event may have been dropped with the
                                // aborted turn; synthesize a minimal cancelled step so
                                // the user still sees the call was abandoned.
                                let mut message =
                                    TranscriptMessage::tool_step(id.clone(), "tool", "{}");
                                if let Some((round, turn)) =
                                    positions_by_session.get(&session_id).copied()
                                {
                                    message.round = Some(round);
                                    message.turn = Some(turn);
                                }
                                message.cancel_tool_step(&id);
                                message.set_tool_step_expanded(false);
                                msgs.push(message);
                            }
                        }
                        RoundEvent::ToolStream { id, stream } => {
                            // Live partial output from a running tool (e.g. bash
                            // stdout). Accumulate into the running step so it updates
                            // in place instead of freezing on a spinner.
                            // Running tool steps are not height-cached, so do not
                            // evict the cached plain-text history for every stdout
                            // line.
                            let mut msgs = buf.write_streaming().await;
                            let applied = msgs
                                .iter_mut()
                                .any(|message| message.push_tool_stream(&id, &stream));
                            if applied {
                                msgs.record_tool_stream(id, stream);
                            } else {
                                // Unknown id: drop silently — the matching ToolCall may
                                // have been dropped with an aborted turn.
                            }
                        }
                        RoundEvent::Envoy {
                            parent_call_id,
                            event,
                        } => {
                            // Full-duplex (ADR-0029): an envoy's permission broker
                            // or `ask_user` request bubbles up nested under this
                            // `parent_call_id`. Surface it in the SAME modal the
                            // top-level path uses (so the user answers it inline) and
                            // record the parent so the reply gets tagged for
                            // down-routing into the child. Falls through to the nested
                            // transcript rendering below for the ordinary
                            // stream/tool-call events.
                            match &event {
                                neenee_core::EnvoyEvent::PermissionRequest(req) => {
                                    subtask_permission_parent_clone
                                        .lock()
                                        .await
                                        .insert(req.id.clone(), parent_call_id.clone());
                                    pending_permission_clone.lock().await.push_back(req.clone());
                                    if !routes_to_side {
                                        *activity_clone.lock().await =
                                            "awaiting permission".to_string();
                                        ir_clone.store(true, Ordering::SeqCst);
                                    }
                                }
                                neenee_core::EnvoyEvent::UserQuestionRequest(req) => {
                                    subtask_question_parent_clone
                                        .lock()
                                        .await
                                        .insert(req.id.clone(), parent_call_id.clone());
                                    pending_question_clone.lock().await.push_back(req.clone());
                                    if !routes_to_side {
                                        *activity_clone.lock().await =
                                            "awaiting user input".to_string();
                                        ir_clone.store(true, Ordering::SeqCst);
                                    }
                                }
                                _ => {}
                            }
                            // Nested assistant deltas mutate a child of the
                            // enclosing tool step. Like top-level tool streams,
                            // they have no standalone height-cache entry.
                            let mut msgs =
                                if matches!(&event, neenee_core::EnvoyEvent::StreamDelta(_)) {
                                    buf.write_streaming().await
                                } else {
                                    buf.write().await
                                };
                            let applied = msgs
                                .iter_mut()
                                .find(|m| m.is_tool_step() && matches!(&m.kind, crate::tui::model::document::MessageKind::ToolStep { id, .. } if id == &parent_call_id))
                                .is_some_and(|message| message.push_envoy_event(&event));
                            if applied && matches!(&event, neenee_core::EnvoyEvent::StreamDelta(_))
                            {
                                msgs.record_envoy_event(parent_call_id, event);
                            }
                        }
                        RoundEvent::PermissionRequest(request) => {
                            // A single model response can carry several write tool
                            // calls, each emitting its own request before blocking on
                            // its reply. Queue them FIFO so none is lost; the UI shows
                            // one sheet at a time and hands off as each is resolved.
                            // Stays global regardless of session so the modal always
                            // surfaces (ADR-0017: the side runs unattended, so in
                            // practice only the primary ever reaches here).
                            pending_permission_clone.lock().await.push_back(request);
                            if !routes_to_side {
                                *activity_clone.lock().await = "awaiting permission".to_string();
                                ir_clone.store(true, Ordering::SeqCst);
                            }
                        }
                        RoundEvent::UserQuestionRequest(request) => {
                            pending_question_clone.lock().await.push_back(request);
                            if !routes_to_side {
                                *activity_clone.lock().await = "awaiting user input".to_string();
                                ir_clone.store(true, Ordering::SeqCst);
                            }
                        }
                        RoundEvent::InputRequest(request) => {
                            pending_input_clone.lock().await.push_back(request);
                            if !routes_to_side {
                                *activity_clone.lock().await = "awaiting command input".to_string();
                                ir_clone.store(true, Ordering::SeqCst);
                            }
                        }
                        RoundEvent::Compacted {
                            archived_messages,
                            before_chars,
                            after_chars,
                        } => {
                            let mut msgs = buf.write().await;
                            push_local_notice(
                                &mut msgs,
                                NoticeSeverity::Info,
                                format!(
                                    "Compacted {} messages: {} -> {} bytes.",
                                    archived_messages, before_chars, after_chars
                                ),
                            );
                        }
                        RoundEvent::HarnessState(snapshot) => {
                            let running = !snapshot.loop_status.is_idle();
                            outbox_signals_clone.lock().await.push_back(
                                event_loop::OutboxSignal::HarnessState {
                                    session_id: session_id.clone(),
                                    idle: !running,
                                },
                            );
                            if !routes_to_side {
                                let round_counter = snapshot.round_counter;
                                if !running && needs_round_rebase {
                                    rebase_transcript_rounds(
                                        &mut messages_clone.write().await,
                                        round_counter,
                                    );
                                    needs_round_rebase = false;
                                }
                                *round_count_clone.lock().await = round_counter;
                                *harness_clone.lock().await = snapshot;
                                if running {
                                    // A new round resets the turn counter; it stays 0
                                    // until the first `TurnStarted` of the round lands.
                                    *current_turn_clone.lock().await = 0;
                                    // Reset the review alert and stamp the round timer so the
                                    // activity bar can render a live `<elapsed>` segment.
                                    *review_alert_clone.lock().await = String::new();
                                    *round_started_at_clone.lock().await =
                                        Some(std::time::Instant::now());
                                }
                                ir_clone.store(running, Ordering::SeqCst);
                                if !running {
                                    activity_clone.lock().await.clear();
                                    *current_turn_clone.lock().await = 0;
                                    *review_alert_clone.lock().await = String::new();
                                    *round_started_at_clone.lock().await = None;
                                }
                            }
                            // A harness state change is always a round boundary
                            // (idle at the end of a round, "running"/"loop N/M" at the
                            // start of a new one). If the previous round ended mid-
                            // reasoning — e.g. the user interrupted, the provider
                            // errored, or a fresh turn superseded a still-streaming
                            // one — `StreamReasoningEnd` never arrives, so the
                            // in-flight Thinking message keeps `duration_ms: None`.
                            // That is exactly the state the renderer uses to decide
                            // the reasoning marker is "running" and should keep
                            // breathing its spinner, which would flash forever after
                            // an interrupt. Freeze any such orphaned trace by
                            // stamping its elapsed time (or 0 if the start instant
                            // was already consumed) so the spinner stops.
                            let duration_ms = reasoning_start
                                .take()
                                .map(|started| started.elapsed().as_millis() as u64);
                            let mut msgs = buf.write().await;
                            if !running {
                                clear_provider_retry(&mut msgs);
                            }
                            finalize_streaming_reasoning(&mut msgs, duration_ms);
                        }
                        RoundEvent::TodosUpdated(list) => {
                            if !routes_to_side {
                                *todos_clone.lock().await = Some(list);
                            }
                        }
                        RoundEvent::UnattendedChanged(enabled) => {
                            if !routes_to_side {
                                harness_clone.lock().await.unattended = enabled;
                            }
                        }
                        RoundEvent::RetryScheduled {
                            attempt,
                            max_attempts,
                            delay_ms,
                            message,
                        } => {
                            let mut msgs = buf.write().await;
                            upsert_provider_retry(
                                &mut msgs,
                                attempt,
                                max_attempts,
                                delay_ms,
                                message,
                            );
                            if !routes_to_side {
                                *activity_clone.lock().await = "waiting to retry".to_string();
                                ir_clone.store(true, Ordering::SeqCst);
                            }
                        }
                        RoundEvent::Error(e) => {
                            let mut msgs = buf.write().await;
                            clear_provider_retry(&mut msgs);
                            push_local_notice(&mut msgs, NoticeSeverity::Error, e);
                            if !routes_to_side {
                                ir_clone.store(false, Ordering::SeqCst);
                                activity_clone.lock().await.clear();
                            }
                        }
                        RoundEvent::SessionReview { alert } => {
                            if !routes_to_side {
                                // Mirror the latest review verdict into the runtime cell
                                // so the activity bar's `⚠ <alert>` segment shows the
                                // diagnostic's summary (or clears it when `alert` is
                                // empty — a healthy review). The frame loop copies this
                                // into `App::review_alert`, which `draw_activity_bar`
                                // reads.
                                *review_alert_clone.lock().await = alert;
                            }
                        }
                    } // end inner `match event`
                }
                AgentResponse::ParentStatus(status) => {
                    // ADR-0017: primary-session status for the `/btw` side
                    // banner. Mirrored into `App::parent_status` each frame.
                    *parent_status_clone.lock().await = status;
                }
                AgentResponse::SideViewOpened { side_id, .. } => {
                    // ADR-0017: enter the side view. Record the routing key so
                    // subsequent per-turn events stream into the side buffer,
                    // and queue the view transition for the event loop.
                    listener_side_id = Some(side_id.clone());
                    side_messages_clone.write().await.clear();
                    *side_view_signal_clone.lock().await =
                        Some(event_loop::SideViewSignal::Opened { side_id });
                }
                AgentResponse::SideViewClosed => {
                    // ADR-0017: leave the side view. Drop the routing key so
                    // events route back to the primary buffer.
                    listener_side_id = None;
                    *side_view_signal_clone.lock().await = Some(event_loop::SideViewSignal::Closed);
                }
                AgentResponse::PermissionsCleared => {
                    pending_permission_clone.lock().await.clear();
                    activity_clone.lock().await.clear();
                }
                AgentResponse::ProviderKeys(status) => {
                    *key_status_clone.lock().await = status.into_iter().collect();
                }
                AgentResponse::ProviderPicker(snapshot) => {
                    *provider_picker_clone.lock().await = snapshot;
                }
                AgentResponse::ConversationCleared => {
                    messages_clone.write().await.clear();
                    *round_count_clone.lock().await = 0;
                    needs_round_rebase = false;
                    context_tokens_clone.lock().await.clear();
                }
                AgentResponse::ConversationReplaced(messages) => {
                    *messages_clone.write().await =
                        transcript_messages_from_core(messages, &tui_config_clone);
                    needs_round_rebase = true;
                    // The model-window revision changed; do not reuse an API
                    // anchor from the previous session/projection.
                    context_tokens_clone.lock().await.clear();
                }
                AgentResponse::SessionsOverview(sessions) => {
                    *sessions_overview_clone.lock().await = sessions;
                    open_sessions_clone.store(true, Ordering::SeqCst);
                }
                AgentResponse::SessionContext(snapshot) => {
                    *session_context_clone.lock().await = Some(snapshot);
                }
                AgentResponse::Exit => {
                    should_quit_clone.store(true, Ordering::SeqCst);
                }
                AgentResponse::ProviderSwitched { provider, model } => {
                    let mut msgs = messages_clone.write().await;
                    push_local_notice(
                        &mut msgs,
                        NoticeSeverity::Info,
                        format!("Provider switched to {} ({})", provider, model),
                    );
                    *cp_clone.lock().await = provider;
                    *cm_clone.lock().await = model;
                }
                AgentResponse::ConnectStatus(status) => {
                    let mut msgs = messages_clone.write().await;
                    match status {
                        neenee_core::ConnectStatus::Pending {
                            url,
                            user_code,
                            message,
                            ..
                        } => {
                            if !url.is_empty() {
                                let _ = webbrowser::open(&url);
                            }
                            *oauth_add_signal_clone.lock().await =
                                Some(event_loop::OauthAddSignal::Pending {
                                    url: url.clone(),
                                    user_code: user_code.clone(),
                                    message: message.clone(),
                                });
                            // The add-flow surfaces the URL/code in the
                            // OauthPending modal, so suppress the transcript
                            // notice there to avoid duplicating the link. Only
                            // the reconnect flow (no modal) gets the notice.
                            let in_add_flow = awaiting_oauth_add_clone.load(Ordering::SeqCst);
                            if !in_add_flow {
                                let body = if user_code.is_empty() {
                                    format!(
                                        "{message}\n  Open: {url}\n  Waiting for authorization…"
                                    )
                                } else {
                                    format!(
                                        "{message}\n  Open: {url}\n  Code: {user_code}\n  Waiting for authorization…"
                                    )
                                };
                                push_local_notice(&mut msgs, NoticeSeverity::Info, body);
                            }
                        }
                        neenee_core::ConnectStatus::Done { provider } => {
                            *oauth_add_signal_clone.lock().await =
                                Some(event_loop::OauthAddSignal::Done);
                            push_local_notice(
                                &mut msgs,
                                NoticeSeverity::Info,
                                format!("{provider} authorized."),
                            );
                        }
                        neenee_core::ConnectStatus::DiscoveryWarning { provider, message } => {
                            // Login succeeded but live model discovery failed, so
                            // the model list may still be the seed subset. Tell
                            // the user why rather than letting a stale list read
                            // as "the account only has these models".
                            push_local_notice(
                                &mut msgs,
                                NoticeSeverity::Warning,
                                format!(
                                    "{provider}: could not refresh the model list ({message}). Showing the previous list."
                                ),
                            );
                        }
                        neenee_core::ConnectStatus::Failed { provider, message } => {
                            *oauth_add_signal_clone.lock().await =
                                Some(event_loop::OauthAddSignal::Failed {
                                    message: message.clone(),
                                });
                            push_local_notice(
                                &mut msgs,
                                NoticeSeverity::Error,
                                format!("{provider} connect failed: {message}"),
                            );
                        }
                    }
                }
                AgentResponse::Error(msg) => {
                    let mut msgs = messages_clone.write().await;
                    push_local_notice(&mut msgs, NoticeSeverity::Error, msg);
                }
                AgentResponse::TuiLayoutUpdated(_value) => {
                    // Persisted transcript layout confirmed by the harness.
                    // The apply path already set `app.transcript_layout`
                    // optimistically (via `Strategy::from_config`, the same
                    // interpreter the harness-side value round-trips through),
                    // so no re-seed is needed on success. A save failure is
                    // surfaced separately as `AgentResponse::Error`. Kept as
                    // an explicit arm (rather than a `_ =>` catch-all) so a
                    // future normalization step can hook in here.
                }
                AgentResponse::TuiColorSchemeUpdated { .. } => {
                    // Appearance changes are applied optimistically in the TUI
                    // so every frame switches at once. Save failures arrive as
                    // `AgentResponse::Error`; this success response is kept
                    // explicit for protocol exhaustiveness.
                }
            }
        }
    });

    let messages_for_loop = messages.clone();

    let mut app = App {
        input: String::new(),
        messages: Vec::new(),
        messages_version: 0,
        side_messages: Vec::new(),
        side_messages_version: 0,
        layout_height_cache: Default::default(),
        in_side_view: false,
        side_session_id: None,
        parent_status: ParentStatus::Idle,
        scroll: 0,
        follow_bottom: true,
        content_lines: 0,
        view_height: 0,
        max_scroll: 0,
        sticky_step: None,
        sticky_rect: None,
        activity_rect: None,
        hint_context_rect: None,
        token_ledger,
        context_tokens: None,
        round_tps: None,
        token_report_scroll: 0,
        token_report_detail: false,
        todos_rect: None,
        queue_rect: None,
        modal_rect: None,
        sticky_summary_line: None,
        pin_summary_line: None,
        focus_stack: Vec::new(),
        tx,
        should_quit,
        serve_tap: serve_tap_for_app,
        serve_cancel: None,
        suggestion_index: None,
        completion_dismissed: false,
        custom_commands,
        cursor_position: 0,
        input_scroll: 0,
        active_modal: Modal::None,
        modal_index: 0,
        last_input_rect: neenee_tui_engine::Rect::default(),
        cursor_sync_pending: false,
        cursor_visible: true,
        session_scroll: 0,
        session_modal_follow: true,
        permissions_scroll: 0,
        config_scroll: 0,
        skills_expanded: None,
        history_scroll: 0,
        history_modal_follow: true,
        history_preview: false,
        history_search: false,
        current_provider: initial_provider,
        current_model: initial_model,
        cwd: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        path_scan_cache: None,
        session_context: None,
        loop_status: LoopStatus::Idle,
        activity_status: String::new(),
        unattended: false,
        todos: None,
        round_count: 0,
        current_turn: 0,
        review_alert: String::new(),
        round_started_at: None,
        activity_tab: ActivityTab::Activity,
        activity_scroll: 0,
        queue_scroll: 0,
        queue_modal_follow: true,
        help_scroll: 0,
        modal_keymap_open: false,
        pending_permission: None,
        pending_input: None,
        question: None,
        question_scroll: 0,
        question_modal_follow: true,
        sessions_overview: Vec::new(),
        permission_confirm_always: false,
        permission_show_details: false,
        permission_scroll: 0,
        permission_max_scroll: 0,
        input_history,
        history_index: None,
        history_draft: String::new(),
        pending_images: Vec::new(),
        pending_text_pastes: Vec::new(),
        pending_dispatch: std::collections::VecDeque::new(),
        send_target: crate::tui::app::SendTarget::NextRound,
        naturally_completed_sessions: std::collections::HashSet::new(),
        idle_sessions: std::collections::HashSet::new(),
        running_sessions: std::collections::HashSet::new(),
        selection: SelectionState::None,
        drag: SelectionDrag::default(),
        layout_map: LayoutMap::new(),
        modal_hit_map: crate::tui::model::layout::ModalHitMap::new(),
        hovered_step: None,
        transcript_layout: crate::tui::view::layout::Strategy::from_config(
            &tui_config.transcript_layout,
        ),
        color_scheme: Theme::normalize_color_scheme(&tui_config.color_scheme).to_string(),
        custom_color_scheme: tui_config.custom_color_scheme.clone(),
        custom_color_draft: tui_config.custom_color_scheme.clone(),
        focused_target: None,
        copy_toast_until: None,
        copy_toast_message: String::new(),
        copy_toast_failed: false,
        ctrl_c_armed_ticks: 0,
        esc_armed_ticks: 0,
        spinner_epoch: std::time::Instant::now(),
        stashed_input: String::new(),
        editor_target: None,
        editor_field: 0,
        editor_key: String::new(),
        editor_model: String::new(),
        editor_model_settings_only: false,
        editor_target_is_builtin: false,
        editor_effort: "high".to_string(),
        editor_thinking_available: false,
        editor_thinking: true,
        custom_field: 0,
        custom_fields: Vec::new(),
        custom_protocol_wire: String::new(),
        custom_models: Vec::new(),
        custom_url_hint: String::new(),
        custom_user_agent: None,
        custom_auth: neenee_core::ChannelAuth::ApiKey,
        custom_template_id: None,
        awaiting_oauth_add: false,
        oauth_pending_message: String::new(),
        oauth_pending_url: String::new(),
        oauth_pending_user_code: String::new(),
        oauth_pending_error: None,
        oauth_scroll: 0,
        custom_suggest_index: 0,
        custom_scroll: 0,
        custom_edit_id: None,
        custom_name: String::new(),
        custom_base_url: String::new(),
        custom_token: String::new(),
        custom_model: String::new(),
        template_choice: 0,
        template_scroll: 0,
        model_search: false,
        editor_return_to: Modal::None,
        model_scroll: 0,
        model_modal_follow: true,
        pending_provider_delete: None,
        provider_delete_focus: ProviderDeleteChoice::default(),
        provider_delete_rect: None,
        key_status: HashMap::new(),
        provider_picker: ProviderPickerSnapshot::default(),
        theme: Theme::from_color_scheme(&tui_config.color_scheme, &tui_config.custom_color_scheme),
        logo: load_user_logo(),
    };

    // Run app
    let res = event_loop::run_app_loop(
        &mut terminal,
        &mut app,
        event_loop::UiRuntime {
            current_provider,
            current_model,
            context_tokens,
            round_tps,
            harness,
            activity_status,
            pending_permission,
            pending_question,
            pending_input,
            is_responding,
            dirty,
            dirty_notify,
            envoy_permission_parent,
            envoy_question_parent,
            messages: messages_for_loop,
            side_messages,
            parent_status,
            side_view_signal,
            key_status,
            provider_picker,
            sessions_overview,
            open_sessions,
            oauth_add_signal,
            awaiting_oauth_add,
            session_context,
            todos,
            round_count,
            current_turn,
            review_alert,
            round_started_at,
            unsent_input_signal,
            outbox_signals,
        },
        session,
    )
    .await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.writer(),
        PopKeyboardEnhancementFlags,
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        return Err(err.into());
    }

    Ok(app.input_history)
}

#[allow(clippy::too_many_arguments)]
pub async fn start_tui(
    tx: mpsc::UnboundedSender<AgentRequest>,
    rx: mpsc::UnboundedReceiver<AgentResponse>,
    initial_provider: String,
    initial_model: String,
    input_history: Vec<String>,
    initial_messages: Vec<Message>,
    initial_round_count: u64,
    custom_commands: Vec<(String, String)>,
    tui_config: config::TuiConfig,
    session: SessionSource,
    token_ledger: Option<Arc<neenee_core::TokenSourceLedger>>,
) -> Result<Vec<String>, Box<dyn Error>> {
    run_tui(
        tx,
        rx,
        initial_provider,
        initial_model,
        input_history,
        initial_messages,
        initial_round_count,
        custom_commands,
        tui_config,
        session,
        token_ledger,
    )
    .await
}

fn push_core_notice(messages: &mut Vec<TranscriptMessage>, notice: &neenee_core::AgentNotice) {
    let _surface = notice.surface;
    messages.push(TranscriptMessage::notice(
        notice_severity_from_core(notice.severity),
        notice.render_text(),
    ));
}

/// Create the live provider-retry disclosure, or refresh the existing one in
/// place. There is deliberately at most one such message in a transcript.
fn upsert_provider_retry(
    messages: &mut Vec<TranscriptMessage>,
    attempt: usize,
    max_attempts: usize,
    delay_ms: u64,
    failure: String,
) {
    let delay = std::time::Duration::from_millis(delay_ms);
    if let Some(existing) = messages
        .iter_mut()
        .rfind(|message| message.is_provider_retry())
    {
        existing.update_provider_retry(attempt, max_attempts, delay, failure);
        return;
    }
    messages.push(TranscriptMessage::provider_retry(
        attempt,
        max_attempts,
        delay,
        failure,
    ));
}

/// Retry state is a live UI component, not durable conversation history.
fn clear_provider_retry(messages: &mut Vec<TranscriptMessage>) {
    messages.retain(|message| !message.is_provider_retry());
}

/// Apply the visible transcript effect of a stream-start signal. The signal
/// retires transient retry state but deliberately creates no message: transport
/// lifecycle alone must not influence transcript geometry.
fn begin_stream(messages: &mut Vec<TranscriptMessage>) {
    clear_provider_retry(messages);
}

/// Append a streamed assistant-text delta to the current turn, creating the
/// message only when the first visible text arrives. Returning `None` means the
/// caller must perform the structural insertion (and request a full transcript
/// snapshot); returning an id permits the cheap per-message patch path.
fn append_stream_text_delta(
    messages: &mut [TranscriptMessage],
    round: Option<u64>,
    turn: Option<u64>,
    delta: &str,
) -> Option<u64> {
    let message = messages.last_mut().filter(|message| {
        message.role == Role::Assistant
            && matches!(&message.kind, MessageKind::Text)
            && message.round == round
            && message.turn == turn
    })?;
    message.push_stream(delta);
    Some(message.id)
}

fn push_local_notice(
    messages: &mut Vec<TranscriptMessage>,
    severity: NoticeSeverity,
    text: impl Into<String>,
) {
    messages.push(TranscriptMessage::notice(severity, text));
}

/// Format a single inline-transcript notice for a task-list update. Task-list
/// changes are the agent's own bookkeeping — full per-item detail lives in the
/// Activity modal — so the transcript never fans them out into one line per
/// changed step. Instead every update collapses to **at most one** summary line:
/// the running `done/total` tally, optionally annotated with how many items
/// changed status this turn. Returns `None` when nothing changed.
#[cfg(test)]
fn describe_todos_change(prev: Option<&TodoList>, new: Option<&TodoList>) -> Option<String> {
    let new = new.filter(|l| !l.items.is_empty())?;
    let done = new.count(neenee_core::TodoStatus::Completed);
    let total = new.items.len();
    let Some(prev) = prev.filter(|l| !l.items.is_empty()) else {
        return Some(format!("tasks started · {done}/{total}"));
    };
    // Count status transitions across the items present in both snapshots.
    // Newly added items (no positional counterpart) do not read as a status
    // *change* and are absorbed into the tally rather than flagged here.
    let changed = prev
        .items
        .iter()
        .zip(new.items.iter())
        .filter(|(a, b)| a.status != b.status)
        .count();
    if changed == 0 && prev.items.len() == new.items.len() {
        return None;
    }
    // One compact line: progress tally plus — only when something actually
    // moved — how many steps changed this turn.
    if changed > 0 {
        Some(format!("tasks · {done}/{total} · {changed} updated"))
    } else {
        Some(format!("tasks · {done}/{total}"))
    }
}

#[cfg(test)]
mod describe_todos_change_tests {
    //! Behaviour contract for the single-line task-list transcript notice.
    //! The point of condensing is that *every* update — even one that ticks
    //! five steps at once — yields at most one `ℹ` line, not a fan-out.
    use super::*;
    use neenee_core::{TodoId, TodoItem, TodoStatus};

    fn item(id: u64, status: TodoStatus) -> TodoItem {
        TodoItem {
            id: TodoId(id),
            content: format!("step {id}"),
            status,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn list(items: &[TodoItem]) -> TodoList {
        TodoList {
            items: items.to_vec(),
            ..TodoList::default()
        }
    }

    #[test]
    fn first_appearance_announces_started_with_tally() {
        let new = list(&[
            item(1, TodoStatus::InProgress),
            item(2, TodoStatus::Pending),
            item(3, TodoStatus::Pending),
        ]);
        // No previous list → the "started" line, counting completed (0/3).
        assert_eq!(
            describe_todos_change(None, Some(&new)),
            Some("tasks started · 0/3".to_string())
        );
    }

    #[test]
    fn multiple_status_changes_collapse_to_one_line() {
        // The regression this guards: previously each changed step emitted its
        // own `ℹ` line. Now five simultaneous ticks produce exactly one.
        let prev = list(&[
            item(1, TodoStatus::Pending),
            item(2, TodoStatus::Pending),
            item(3, TodoStatus::InProgress),
            item(4, TodoStatus::Pending),
            item(5, TodoStatus::Pending),
        ]);
        let new = list(&[
            item(1, TodoStatus::Completed),
            item(2, TodoStatus::Completed),
            item(3, TodoStatus::Completed),
            item(4, TodoStatus::InProgress),
            item(5, TodoStatus::Cancelled),
        ]);
        assert_eq!(
            describe_todos_change(Some(&prev), Some(&new)),
            Some("tasks · 3/5 · 5 updated".to_string())
        );
    }

    #[test]
    fn single_status_change_counts_one() {
        let prev = list(&[
            item(1, TodoStatus::Pending),
            item(2, TodoStatus::InProgress),
        ]);
        let new = list(&[item(1, TodoStatus::Pending), item(2, TodoStatus::Completed)]);
        assert_eq!(
            describe_todos_change(Some(&prev), Some(&new)),
            Some("tasks · 1/2 · 1 updated".to_string())
        );
    }

    #[test]
    fn no_change_emits_nothing() {
        let same = list(&[
            item(1, TodoStatus::InProgress),
            item(2, TodoStatus::Pending),
        ]);
        assert_eq!(describe_todos_change(Some(&same), Some(&same)), None);
    }

    #[test]
    fn size_only_change_drops_the_updated_suffix() {
        // Items added without any positional status change: still one line,
        // but without the "N updated" suffix since nothing transitioned.
        let prev = list(&[item(1, TodoStatus::Pending)]);
        let new = list(&[
            item(1, TodoStatus::Pending),
            item(2, TodoStatus::Pending),
            item(3, TodoStatus::Pending),
        ]);
        assert_eq!(
            describe_todos_change(Some(&prev), Some(&new)),
            Some("tasks · 0/3".to_string())
        );
    }

    #[test]
    fn empty_new_list_emits_nothing() {
        let prev = list(&[item(1, TodoStatus::Pending)]);
        assert_eq!(
            describe_todos_change(Some(&prev), Some(&TodoList::default())),
            None
        );
        assert_eq!(
            describe_todos_change(None, Some(&TodoList::default())),
            None
        );
    }
}

/// Load the user-supplied ASCII logo from `$XDG_CONFIG_HOME/neenee/logo.txt`,
/// clamped to the empty-state bounding box. Best-effort: a missing or unreadable
/// file returns `None`, leaving the built-in wordmark in place.
fn load_user_logo() -> Option<Vec<String>> {
    let path = neenee_persistence::paths::get().logo_file();
    let raw = std::fs::read_to_string(&path).ok()?;
    // Re-use the renderer's parser so the clamp stays defined in one place.
    // The parser already strips CRLF/trailing blanks and truncates to the box.
    view::parse_logo(&raw)
}

#[cfg(test)]
mod tests;
