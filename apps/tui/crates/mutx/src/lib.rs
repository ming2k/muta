//! Terminal UI frontend, in three layers:
//!
//! - `mutx-engine` — the in-house grid engine (a retained cell grid
//!   with dirty tracking and a back/front diff; ADR-0038) plus the crossterm
//!   backend.
//! - the view modules under this one — the drawing tree + semantic document
//!   model, painting `muta_contracts` domain types into the engine grid:
//!   `model` (document, layout map for hit-testing, selection state),
//!   `view` (the transcript-area renderer the shell drives each frame), the
//!   drawing sub-trees (`components` / `overlays` / `tools` /
//!   `disclosure`), layout strategies (`layout`), and drawing leaves /
//!   shared tokens (`theme`, `design`, `chrome`, `composer`,
//!   `primitives`, …). The view modules are crate-private: the public
//!   surface is the shell entry points, not the widget tree.
//! - the app shell (this module's remaining submodules): application state
//!   ([`app`]), input mapping ([`input`]), and the event/render loop
//!   (`event_loop`). [`start_tui`] is the entry point wired by the
//!   `muta` binary (`mutx`), which stays a thin shell over this
//!   crate. The debug-only [`showcase`] module is a "Storybook" rendering
//!   individual components in isolation (`mutx showcase <component>`).
//!
//! The seam between shell and view is the borrowed `view::TranscriptView`
//! the event loop fills in each frame; the view modules never reach back into
//! the shell.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

#[cfg(debug_assertions)]
pub mod showcase;

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

// ── View layer (merged from the former `mutx-view` crate) ─────────────

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
pub(crate) mod effort_ignition;
pub(crate) mod empty_state;
pub(crate) mod footer_stack;
pub(crate) mod markdown_table;
pub(crate) mod message_body;
pub(crate) mod notice;
pub(crate) mod page_header;
pub(crate) mod primitives;
pub(crate) mod round_interrupt;
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
pub(crate) mod views;

#[cfg(test)]
mod snapshot_tests;

pub(crate) use app::{App, CaretOwner, ProviderDeleteChoice, ProviderRetryState, SelectionEdge};
pub(crate) use completion::CompletionKind;
pub(crate) use modal::{ActivityTab, Modal, Recess};
pub(crate) use providers::{
    CustomField, PROVIDER_TEMPLATES, protocol_model_candidates, provider_template_label_for,
};

use crossterm::{
    event::{
        DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use muta_contracts::{
    AgentRequest, AgentResponse, HarnessSnapshot, LoopStatus, Message, ParentStatus,
    PermissionRequest, ProviderPickerSnapshot, Role, RoundEvent, SessionContextSnapshot,
    SessionOverview, TodoList, UserQuestionRequest,
};
use mutx_engine::{Backend, Terminal};
use std::{
    collections::{HashMap, VecDeque},
    error::Error,
    io,
    sync::Arc,
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::sync::{Mutex, mpsc};

use crate::model::document::{
    CommandPhase, MessageKind, NoticeSeverity, TranscriptMessage, UserMessageOrigin,
    notice_severity_from_core,
};
use crate::model::layout::LayoutMap;
use crate::model::selection::{SelectionDrag, SelectionState};
use crate::transcript::{
    finalize_streaming_reasoning, merge_command_rows, merge_round_interrupt_rows,
    rebase_transcript_rounds, transcript_commands_from_ledger, transcript_interrupts_from_records,
    transcript_messages_from_core,
};
use crate::view::Theme;

/// Where the session this TUI drives lives. All sessions in the unified
/// daemon model are remote (daemon-hosted).
#[derive(Debug, Clone)]
pub enum SessionSource {
    Remote {
        /// The hosted session's id, learned from the WS handshake.
        session_id: String,
    },
}

impl SessionSource {
    /// The primary session id.
    pub(crate) async fn session_id(&self) -> String {
        match self {
            SessionSource::Remote { session_id } => session_id.clone(),
        }
    }
}

/// Which full-screen overlay (if any) the TUI opens straight into at startup
/// instead of a conversation view. In that mode the overlay is not a transient
/// modal — there is no conversation the user asked for behind it — so closing
/// it quits the program rather than dropping into an empty chat (mirrors how
/// `mutx attach`'s picker behaves). Distinct from `None`, where the TUI
/// opens directly onto a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupOverlay {
    /// Ordinary startup: land on the conversation.
    None,
    /// `mutx attach` (no id): open the sessions picker to choose a session.
    SessionsPicker,
    /// `mutx dashboard`: open the session dashboard over the carrier session.
    Dashboard,
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
                    event: muta_contracts::EnvoyEvent::StreamDelta(_)
                        | muta_contracts::EnvoyEvent::StreamReasoningDelta(_),
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
    input_history: Vec<muta_contracts::HistoryEntry>,
    initial_messages: Vec<Message>,
    initial_commands: Vec<muta_contracts::CommandRecord>,
    initial_round_count: u64,
    command_catalog: muta_contracts::CommandCatalog,
    initial_round_interrupts: Vec<muta_contracts::RoundInterrupt>,
    tui_config: config::TuiConfig,
    input_history_config: config::InputHistoryConfig,
    session: SessionSource,
    token_ledger: Option<Arc<muta_contracts::TokenSourceLedger>>,
    startup_overlay: StartupOverlay,
) -> Result<TuiOutcome, Box<dyn Error>> {
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
    // The mutx-engine engine owns its grid + diff + crossterm I/O directly. No
    // No ratatui, no WideHealBackend wrapper — the engine's retained grid writes
    // wide-glyph trailing cells with the glyph's own background at write time,
    // so ghost cells cannot occur regardless of terminal or multiplexer
    // (ADR-0038).
    let backend = Backend::new(stdout);
    let mut terminal = Terminal::new(backend);
    // Install the signal guard after the terminal enters raw mode + alt screen
    // so any later SIGTERM/SIGINT/SIGHUP restores it instead of stranding it.
    terminal::spawn_signal_guard();
    // Panic hook: a panic anywhere on the main thread unwinds the process
    // without running run_tui's cleanup (raw mode + alt screen + mouse
    // capture stay enabled, leaving the host terminal scrambled). The signal
    // guard covers SIGINT/SIGTERM/SIGHUP/SIGQUIT but not panics; this closes
    // that gap. Installed once per process — the /host re-attach loop calls
    // run_tui repeatedly and must not chain hooks. Background tasks (the
    // response listener, ws pumps) panic without unwinding the terminal:
    // only the thread that owns the terminal restores it.
    static PANIC_HOOK: std::sync::Once = std::sync::Once::new();
    PANIC_HOOK.call_once(|| {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let main_thread = std::thread::current()
                .name()
                .is_some_and(|name| name == "main");
            if main_thread {
                terminal::restore_terminal();
            }
            default_hook(info);
        }));
    });
    let tui_config = Arc::new(tui_config);
    let mut restored = transcript_messages_from_core(initial_messages, &tui_config);
    restored = merge_command_rows(restored, transcript_commands_from_ledger(initial_commands));
    restored = merge_round_interrupt_rows(
        restored,
        transcript_interrupts_from_records(initial_round_interrupts),
    );
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
        muta_contracts::ContextTokenSnapshot,
    >::new()));
    let context_tokens_clone = context_tokens.clone();

    let is_responding = Arc::new(AtomicBool::new(false));
    let ir_clone = is_responding.clone();
    let harness = Arc::new(Mutex::new(HarnessSnapshot {
        loop_status: LoopStatus::Idle,
        round_counter: initial_round_count,
        autopilot: false,
        workspace_security: muta_contracts::WorkspaceSecuritySnapshot::default(),
        retry_pending: false,
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
    // Wall-clock instant the current round started. Stamped on a "running"
    // HarnessState so the activity bar can render a live `<elapsed>` segment.
    let round_started_at: Arc<Mutex<Option<std::time::Instant>>> = Arc::new(Mutex::new(None));
    let round_started_at_clone = round_started_at.clone();
    let activity_status = Arc::new(Mutex::new(String::new()));
    let activity_clone = activity_status.clone();
    let provider_retry: Arc<Mutex<Option<ProviderRetryState>>> = Arc::new(Mutex::new(None));
    let provider_retry_clone = provider_retry.clone();
    let pending_permission = Arc::new(Mutex::new(VecDeque::<PermissionRequest>::new()));
    let pending_permission_clone = pending_permission.clone();
    let pending_question = Arc::new(Mutex::new(VecDeque::<UserQuestionRequest>::new()));
    let pending_question_clone = pending_question.clone();
    let pending_input = Arc::new(Mutex::new(VecDeque::<muta_contracts::InputRequest>::new()));
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
    // Effective `[websearch]` config (presence-only view), fetched when the
    // Settings view opens and refreshed on every update ack. The event loop
    // mirrors it into `App::websearch_config` each frame.
    let websearch_config = Arc::new(Mutex::new(
        Option::<muta_contracts::WebSearchConfigView>::None,
    ));
    let websearch_config_clone = websearch_config.clone();
    let provider_picker = Arc::new(Mutex::new(ProviderPickerSnapshot::default()));
    let provider_picker_clone = provider_picker.clone();
    let sessions_overview = Arc::new(Mutex::new(Vec::<SessionOverview>::new()));
    let sessions_overview_clone = sessions_overview.clone();
    let sessions_overview_rev = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let sessions_overview_rev_clone = sessions_overview_rev.clone();
    let session_detail = Arc::new(tokio::sync::Mutex::new(
        None::<muta_contracts::SessionDetail>,
    ));
    let session_detail_clone = session_detail.clone();
    let session_tree = Arc::new(tokio::sync::Mutex::new(None::<muta_contracts::SessionTree>));
    let session_tree_clone = session_tree.clone();
    // Token-source report fetched on demand from the harness when the
    // context-usage modal opens in attach mode (the ledger is daemon-side
    // there). Mirrors the `session_detail` on-demand pattern.
    let token_report = Arc::new(tokio::sync::Mutex::new(
        None::<muta_contracts::TokenSourceReport>,
    ));
    let token_report_clone = token_report.clone();
    // Cross-session usage statistics (ADR-0122), fetched on demand when the
    // `/usage` overlay opens. Same on-demand pattern.
    let usage_stats = Arc::new(tokio::sync::Mutex::new(
        None::<muta_contracts::usage_stats::UsageStatsReport>,
    ));
    let usage_stats_clone = usage_stats.clone();
    // The **live primary session id**. The handshake-time `SessionSource` is
    // frozen for the process lifetime, but the harness repoints its shared
    // store on `/new`, `/session open`, `/resume`, and `/fork` — so anything
    // session-scoped that must follow a mid-run switch (the ↑/↓ prompt
    // history's origin tag above all) reads this cell instead. The listener
    // updates it from `ConversationCleared` / `ConversationReplaced` and the
    // event loop mirrors it into `App::current_session_id` each frame.
    let live_session_id = Arc::new(Mutex::new(session.session_id().await));
    let live_session_id_clone = live_session_id.clone();
    let open_sessions = Arc::new(AtomicBool::new(false));
    let open_sessions_clone = open_sessions.clone();
    let open_tree = Arc::new(AtomicBool::new(false));
    let open_tree_clone = open_tree.clone();
    // `/host` daemon control panel (ADR-0096): a live monitor snapshot the TUI
    // maintains client-side (separate from the session attach stream).
    let host_sessions = Arc::new(Mutex::new(Vec::<muta_contracts::MonitoredSession>::new()));
    let host_sessions_rev = Arc::new(std::sync::atomic::AtomicU64::new(0));
    // `mutx dashboard` seeds the open flag so the event loop's very first
    // frame raises the dashboard over the carrier session (the same
    // one-shot signal `AgentResponse::OpenHostPanel` sets for `/dashboard`).
    let open_host = Arc::new(AtomicBool::new(
        startup_overlay == StartupOverlay::Dashboard,
    ));
    let open_host_clone = open_host.clone();
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
    // `/btw` aside shared state (ADR-0017, ADR-0103). The aside transcript
    // buffer, the parent-status mirror, the asides list, and the one-shot
    // view-transition signal all cross the listener → loop boundary here.
    let side_messages = Arc::new(versioned::Versioned::new(Vec::<TranscriptMessage>::new()));
    let side_messages_clone = side_messages.clone();
    let parent_status = Arc::new(Mutex::new(ParentStatus::Idle));
    let parent_status_clone = parent_status.clone();
    let side_view_signal = Arc::new(Mutex::new(None::<event_loop::SideViewSignal>));
    let side_view_signal_clone = side_view_signal.clone();
    // The asides list (ADR-0103 §5). Navigation is local; a reply only
    // replaces rows and never re-opens a hidden view.
    let btw_list = Arc::new(Mutex::new(Vec::<muta_contracts::BtwAsideSummary>::new()));
    let btw_list_clone = btw_list.clone();
    // View-scoped chrome (ADR-0103 fix): per-session activity / responding /
    // round / turn, maintained by the listener for the primary *and* every
    // live aside. The loop mirrors this into `App::session_chrome` each
    // frame; a view renders only its own session's entry, so an aside view
    // shows its own activity bar instead of inheriting the primary's.
    let session_chrome = Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
        String,
        crate::app::SessionChrome,
    >::new()));
    let session_chrome_clone = session_chrome.clone();
    /// Per-event chrome bookkeeping for one session's stream. Writes the
    /// session's own `SessionChrome` entry (bookkeeping for every session),
    /// and additionally updates the *displayed* legacy fields only when the
    /// event belongs to the primary (`!routes_to_side`) — preserving the
    /// existing isolation of the main view from aside rounds while giving
    /// the aside view its own state to render once focused.
    struct ChromeUpdate {
        session_id: String,
        map: Arc<std::sync::Mutex<std::collections::HashMap<String, crate::app::SessionChrome>>>,
    }
    impl ChromeUpdate {
        fn edit(&mut self, f: impl FnOnce(&mut crate::app::SessionChrome)) {
            if let Ok(mut map) = self.map.lock() {
                f(map.entry(self.session_id.clone()).or_default());
            }
        }
    }
    // Which session the frontend is currently viewing (primary id, or the
    // focused aside's id), written by the event loop each frame and read by
    // the listener to scope on-demand queries (e.g. `TokenUsageReport`).
    let viewed_session_id = Arc::new(Mutex::new(None::<String>));
    let viewed_session_id_clone = viewed_session_id.clone();
    // Phase-1 unsend signal: set by the listener when the harness reports an
    // `UnsentInput`, drained by the event loop to restore the composer.
    let unsent_input_signal = Arc::new(Mutex::new(None::<event_loop::UnsentInput>));
    let unsent_input_signal_clone = unsent_input_signal.clone();
    // Toast-surfaced notices (command acknowledgments such as `/autopilot on`)
    // are forwarded by the listener and drained by the loop into a transient
    // bubble, never entering the transcript.
    let notice_toast_signal = Arc::new(Mutex::new(None::<event_loop::NoticeToastSignal>));
    let notice_toast_signal_clone = notice_toast_signal.clone();
    let outbox_signals = Arc::new(Mutex::new(VecDeque::<event_loop::OutboxSignal>::new()));
    let outbox_signals_clone = outbox_signals.clone();
    let completion_signal = Arc::new(Mutex::new(None::<event_loop::CompletionSignal>));
    let completion_signal_clone = completion_signal.clone();
    // Dashboard console receipts (ADR-0097 §3): one-shot control tasks push
    // here, the loop drains into `App::host_console_log`.
    let host_console_signal = Arc::new(Mutex::new(VecDeque::<crate::overlays::ConsoleLine>::new()));

    // Spawn the daemon monitor client (ADR-0096): maintains the live session
    // snapshot the `/host` control panel renders. Best-effort — no daemon is
    // a normal state and the panel simply shows an empty list.
    {
        let host_sessions = host_sessions.clone();
        let host_sessions_rev = host_sessions_rev.clone();
        let dirty = dirty_clone.clone();
        let dirty_notify = dirty_notify_clone.clone();
        tokio::spawn(async move {
            let project_root =
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let Some(info) = muta_runtime::client::discover(&project_root) else {
                return;
            };
            let action = muta_contracts::MonitorAction {
                watch: true,
                include_idle: true,
            };
            let Ok(mut rx) = muta_runtime::client::monitor_stream(&info, action).await else {
                return;
            };
            while let Some(event) = rx.recv().await {
                {
                    let mut rows = host_sessions.lock().await;
                    match event {
                        muta_contracts::MonitorEvent::Snapshot(snap) => {
                            *rows = snap.sessions;
                        }
                        muta_contracts::MonitorEvent::SessionAdded(row)
                        | muta_contracts::MonitorEvent::SessionUpdated(row) => {
                            muta_runtime::client::upsert_session_row(&mut rows, row);
                        }
                        muta_contracts::MonitorEvent::SessionRemoved { session_id } => {
                            rows.retain(|r| r.id != session_id);
                        }
                        // The daemon began its graceful shutdown (ADR-0101):
                        // no row change; the stream closes right after. The
                        // next daemon interaction re-discovers or re-spawns.
                        muta_contracts::MonitorEvent::DaemonDraining => {}
                    }
                }
                host_sessions_rev.fetch_add(1, std::sync::atomic::Ordering::Release);
                dirty.store(true, Ordering::SeqCst);
                dirty_notify.notify_one();
            }
        });
    }

    // Spawn response listener
    tokio::spawn(async move {
        let mut reasoning_start: Option<std::time::Instant> = None;
        // Listener-local side routing keys (ADR-0017, widened by ADR-0103):
        // every live aside's `session_id`, learned from `SideViewOpened` and
        // `BtwList`. Kept here (not in `UiRuntime`) because only the listener
        // routes per-turn events; the loop reads the already-routed
        // `side_messages` buffer. A *set* (not the single old id) so a
        // background aside keeps streaming into the side buffer after the
        // user detaches from its view.
        let mut listener_side_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();
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
            match resp {
                // ADR-0017 + ADR-0103: per-turn events arrive tagged with the
                // session they belong to. The listener routes each event to
                // the side buffer when its `session_id` belongs to a live
                // aside — *whether or not that aside is the focused view*
                // (background asides keep streaming into their buffer), and
                // to the primary transcript otherwise. Permission and
                // user-question requests stay global so their modals surface
                // regardless of which view is focused.
                AgentResponse::Round { session_id, event } => {
                    let routes_to_side = listener_side_ids.contains(session_id.as_str());
                    // Select the transcript buffer for this event (ADR-0017):
                    // the side buffer when the event's `session_id` belongs to
                    // a live aside, the primary buffer otherwise. Global
                    // responding/activity/harness state below is gated on
                    // `!routes_to_side` so a concurrent aside round never
                    // clobbers the primary view's chrome; the aside view reads
                    // its own buffer + the parent-status banner instead.
                    // Permission and user-question requests stay global
                    // regardless of origin so their modals always surface.
                    //
                    // Chrome bookkeeping (view-scoped state): every session —
                    // primary *and* asides — mirrors its own activity /
                    // responding / round / turn into `App::session_chrome`
                    // via `chrome_updater`. The primary's entry also feeds the
                    // legacy display fields (gated exactly like today), while
                    // an aside's entry is pure bookkeeping until its view is
                    // focused — at which point `enter_side_view` swaps it in.
                    let chrome_session_id = session_id.clone();
                    let mut chrome_updater = ChromeUpdate {
                        session_id: chrome_session_id,
                        map: session_chrome_clone.clone(),
                    };
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
                        RoundEvent::UserInputUnavailable { input_id } => {
                            // The round closed before an insert (`Ctrl+O`)
                            // could be admitted. Two owners exist for the
                            // content: the transcript entry staged at insert
                            // time (keyed by `insert_id`) and — only for the
                            // legacy queue-owned path — the outbox item. The
                            // entry flips to `⏸ Held`: the turn ended
                            // (naturally or interrupted), so this message now
                            // waits to ship as the *next* round's prompt. The
                            // event loop re-queues it into the outbox under
                            // the same id, which both drains the held entry
                            // when its round starts and re-enables
                            // pointer-based recall/edit.
                            {
                                let mut msgs = buf.write().await;
                                if let Some(entry) = msgs
                                    .iter_mut()
                                    .rev()
                                    .find(|m| m.insert_id.as_deref() == Some(input_id.as_str()))
                                {
                                    entry.hold_pending_round();
                                }
                            }
                            outbox_signals_clone.lock().await.push_back(
                                event_loop::OutboxSignal::Unavailable {
                                    session_id,
                                    input_id,
                                },
                            );
                        }
                        // The mid-round insert path is live via `Ctrl+O`
                        // (InsertIntoRound): the steer is admitted at a safe
                        // turn boundary, so this event settles the transcript
                        // entry the loop already staged as `⏸ Queued` (found
                        // by correlation id) instead of pushing a duplicate —
                        // one entry per insert, from staging to delivery. The
                        // entry keeps its `Insert` origin so it renders the
                        // `↳ insert` provenance. The cancellation variants
                        // stay unused by this frontend (nothing cancels a
                        // pending insert today) and remain deliberate no-ops
                        // rather than being masked by a catch-all.
                        RoundEvent::UserInputInserted(input) => {
                            let input_id = input.id.clone();
                            let visible = input
                                .display_text
                                .clone()
                                .unwrap_or_else(|| input.text.clone());
                            {
                                let mut msgs = buf.write().await;
                                // Find the newest staged entry with this
                                // correlation id and settle it in place.
                                // Fallback: if the correlating entry is gone
                                // (rebuilt transcript, resumed session), push
                                // a fresh one so the admitted steer is still
                                // visible.
                                let settled = msgs
                                    .iter_mut()
                                    .rev()
                                    .find(|m| m.insert_id.as_deref() == Some(input_id.as_str()))
                                    .map(|m| {
                                        m.delivery =
                                            crate::model::document::DeliveryStatus::Delivered;
                                        m.origin = UserMessageOrigin::Insert;
                                        if m.sent_at_ms.is_none() {
                                            m.sent_at_ms = input.sent_at_ms;
                                        }
                                        true
                                    })
                                    .unwrap_or(false);
                                if !settled {
                                    let mut message = TranscriptMessage::new(Role::User, visible);
                                    message.sent_at_ms = input.sent_at_ms;
                                    message.origin = UserMessageOrigin::Insert;
                                    msgs.push(message);
                                }
                            }
                            outbox_signals_clone.lock().await.push_back(
                                event_loop::OutboxSignal::Inserted {
                                    session_id,
                                    input_id,
                                },
                            );
                        }
                        RoundEvent::UserInputCancelled { .. } => {}
                        RoundEvent::UserInputCancelFailed { .. } => {}
                        RoundEvent::NextRoundStarted(input) => {
                            let input_id = input.id.clone();
                            let visible = input
                                .display_text
                                .clone()
                                .unwrap_or_else(|| input.text.clone());
                            {
                                let mut msgs = buf.write().await;
                                // A handed-back insert (`HeldNextRound`) now
                                // ships as this round's prompt: settle its
                                // held entry in place rather than pushing a
                                // second copy. The entry keeps its `Insert`
                                // origin so the `↳ insert` provenance stays
                                // truthful about how the prompt arrived.
                                let settled = msgs
                                    .iter_mut()
                                    .rev()
                                    .find(|m| m.insert_id.as_deref() == Some(input_id.as_str()))
                                    .map(|m| {
                                        m.delivery =
                                            crate::model::document::DeliveryStatus::Delivered;
                                        m.origin = UserMessageOrigin::Insert;
                                        true
                                    })
                                    .unwrap_or(false);
                                if !settled {
                                    let mut message = TranscriptMessage::new(Role::User, visible);
                                    message.sent_at_ms = input.sent_at_ms;
                                    msgs.push(message);
                                }
                            }
                            outbox_signals_clone.lock().await.push_back(
                                event_loop::OutboxSignal::NextRoundStarted {
                                    session_id,
                                    input_id,
                                },
                            );
                        }
                        RoundEvent::RoundCompleted(summary) => {
                            // The web header chip still consumes this summary;
                            // the TUI's Context Usage modal now derives its
                            // rates from the token ledger instead.
                            let _ = summary;
                            outbox_signals_clone
                                .lock()
                                .await
                                .push_back(event_loop::OutboxSignal::RoundCompleted { session_id });
                        }
                        RoundEvent::RoundInterrupted(record) => {
                            // C11: the durable twin of the live stop. Append
                            // the projection row (a warning notice) with the
                            // record's own timestamp so the trailing ` · HH:MM`
                            // shows when the stop happened. The reason label
                            // rides in the notice body; the transcript merge
                            // on resume renders the same row at its seam.
                            let at_ms = record.at_ms;
                            let mut msgs = buf.write().await;
                            msgs.push(
                                TranscriptMessage::round_interrupted(record).with_sent_at_ms(at_ms),
                            );
                        }
                        RoundEvent::Notice(notice) => {
                            // Provider retry has a dedicated, self-refreshing
                            // transcript disclosure driven by RetryScheduled.
                            // Do not also degrade its toast into an appended
                            // inline notice on every failed attempt.
                            if notice.kind == muta_contracts::NoticeKind::ProviderRetry {
                                // Skip the inline append; RetryScheduled owns
                                // the retry disclosure.
                            } else if notice.surface == muta_contracts::NoticeSurface::Toast {
                                // Toast-surfaced notices (command
                                // acknowledgments such as `/autopilot on`) are
                                // forwarded as a transient bubble instead of
                                // being appended to the transcript. They carry
                                // no conversational content, so polluting the
                                // scrollback with them would only muddy the
                                // model's output. The loop drains this signal
                                // and shows a top-right toast.
                                *notice_toast_signal_clone.lock().await =
                                    Some(event_loop::NoticeToastSignal {
                                        severity: notice_severity_from_core(notice.severity),
                                        text: notice.render_text(),
                                    });
                            } else {
                                let mut msgs = buf.write().await;
                                push_core_notice(&mut msgs, &notice);
                            }
                        }
                        RoundEvent::Text(t) => {
                            let (provider, model) =
                                event_loop::attribution(&cp_clone, &cm_clone).await;
                            let effort = event_loop::picker_effort(
                                &provider_picker_clone,
                                &cp_clone,
                                &cm_clone,
                            )
                            .await;
                            *provider_retry_clone.lock().await = None;
                            let mut msgs = buf.write().await;
                            let mut message = TranscriptMessage::new(Role::Assistant, t)
                                .with_attribution(provider, model)
                                .with_effort(effort)
                                .with_sent_at_ms(crate::event_loop::now_epoch_ms());
                            if let Some((round, turn)) =
                                positions_by_session.get(&session_id).copied()
                            {
                                message.round = Some(round);
                                message.turn = Some(turn);
                            }
                            msgs.push(message);
                            if !routes_to_side {
                                // `Text` is *content*, not a lifecycle signal
                                // (ADR-0091). Clearing the optimistic activity
                                // surface here was what let a toast-only slash
                                // reply leave the bar stuck on "queued" after
                                // ADR-0088 migrated `/autopilot` from `Text` to
                                // `Notice`. Only collapse the surface when the
                                // harness is actually idle: a slash reply
                                // delivered mid-round must not tear down the
                                // running round's bar, and the round's terminal
                                // `HarnessState(Idle)` (or the driver's
                                // post-dispatch reconcile) is what retires the
                                // surface when the harness truly goes idle.
                                if harness_clone.lock().await.loop_status.is_idle() {
                                    ir_clone.store(false, Ordering::SeqCst);
                                    activity_clone.lock().await.clear();
                                }
                            }
                        }
                        RoundEvent::CommandResult { name, args, result } => {
                            // A typed slash-command result (ADR-0091): settle
                            // the pending command component in place — one
                            // row owns both the input and the output
                            // (ADR-0108). Content-bearing like `Text` — same
                            // idle-only activity-surface handling.
                            *provider_retry_clone.lock().await = None;
                            let invocation = if name == "shell" {
                                args.clone()
                            } else {
                                format!("/{} {}", name, args).trim_end().to_string()
                            };
                            {
                                let mut msgs = buf.write().await;
                                // Prefer the newest *pending* row with this
                                // invocation: two identical commands run in
                                // quick succession each settle their own row
                                // (FIFO), instead of the second reply
                                // bouncing off the first's completed row.
                                let settled = msgs
                                    .iter_mut()
                                    .rev()
                                    .find(|message| {
                                        message.is_command_result()
                                            && message.raw == invocation.trim()
                                            && message.command_result_phase()
                                                == Some(CommandPhase::Pending)
                                    })
                                    .map(|message| message.settle_command_result(result.clone()))
                                    .unwrap_or(false);
                                if !settled {
                                    // No pending row matched (the transcript
                                    // was rebuilt, or the command predates
                                    // this view): the reply still renders as
                                    // a complete command component.
                                    let mut message = TranscriptMessage::command_result(
                                        name.clone(),
                                        args.clone(),
                                        Some(result.clone()),
                                    )
                                    .with_sent_at_ms(crate::event_loop::now_epoch_ms());
                                    if let Some((round, turn)) =
                                        positions_by_session.get(&session_id).copied()
                                    {
                                        message.round = Some(round);
                                        message.turn = Some(turn);
                                    }
                                    msgs.push(message);
                                }
                            }
                            if !routes_to_side && harness_clone.lock().await.loop_status.is_idle() {
                                ir_clone.store(false, Ordering::SeqCst);
                                activity_clone.lock().await.clear();
                            }
                        }
                        RoundEvent::Activity(status) => {
                            // View-scoped chrome: record this session's own
                            // activity text regardless of which view is
                            // focused; only the primary also drives the
                            // displayed global activity state.
                            chrome_updater.edit(|c| {
                                c.activity = status.clone();
                                c.responding = true;
                            });
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
                            // View-scoped chrome: per-session structural
                            // counters (Activity modal's `round N · turn M`).
                            chrome_updater.edit(|c| {
                                c.round_count = round;
                                c.current_turn = turn;
                            });
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
                            // View-scoped chrome: a stream means this session
                            // is mid-round (elapsed timer origin).
                            chrome_updater.edit(|c| {
                                c.responding = true;
                                if c.round_started_at.is_none() {
                                    c.round_started_at = Some(std::time::Instant::now());
                                }
                                if c.activity.is_empty() {
                                    c.activity = "responding".to_string();
                                }
                            });
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
                                // Create the transcript item from real content,
                                // never from a transport-level start signal — but
                                // keep it on the streaming patch path (a targeted
                                // append): the frozen history's heights are
                                // untouched by a tail append.
                                let (provider, model) =
                                    event_loop::attribution(&cp_clone, &cm_clone).await;
                                let effort = event_loop::picker_effort(
                                    &provider_picker_clone,
                                    &cp_clone,
                                    &cm_clone,
                                )
                                .await;
                                *provider_retry_clone.lock().await = None;
                                let pre_append_tail = msgs.last().map(|tail| tail.id);
                                let mut message = TranscriptMessage::new(Role::Assistant, delta)
                                    .with_attribution(provider, model)
                                    .with_effort(effort);
                                if let Some((round, turn)) = position {
                                    message.round = Some(round);
                                    message.turn = Some(turn);
                                }
                                msgs.invalidate_message_height(message.id);
                                msgs.record_append_message(pre_append_tail, message.clone());
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
                            *provider_retry_clone.lock().await = None;
                            // Targeted finalize (no full snapshot): the final
                            // text replaces the streaming entry in place and
                            // evicts only that entry's cached height.
                            let mut msgs = buf.write_streaming().await;
                            // Identity-addressed (ADR-0114): a command entry
                            // dispatched during the stream can sit between the
                            // assistant-text entry and the transcript tail;
                            // resolve by position, not by "is last".
                            if let Some(message) = msgs.iter_mut().rfind(|message| {
                                message.role == Role::Assistant
                                    && matches!(&message.kind, MessageKind::Text)
                                    && message.round == round
                                    && message.turn == turn
                            }) {
                                message.raw = final_content;
                                message.reparse();
                                let finalized = message.clone();
                                let id = message.id;
                                msgs.invalidate_message_height(id);
                                msgs.record_replace_message(id, finalized);
                            } else if !final_content.is_empty() {
                                // Defensive fallback for providers that deliver only a final
                                // payload without any preceding text delta.
                                let (provider, model) =
                                    event_loop::attribution(&cp_clone, &cm_clone).await;
                                let effort = event_loop::picker_effort(
                                    &provider_picker_clone,
                                    &cp_clone,
                                    &cm_clone,
                                )
                                .await;
                                let mut message =
                                    TranscriptMessage::new(Role::Assistant, final_content)
                                        .with_attribution(provider, model)
                                        .with_effort(effort);
                                if let Some((round, turn)) = position {
                                    message.round = Some(round);
                                    message.turn = Some(turn);
                                }
                                let pre_append_tail = msgs.last().map(|tail| tail.id);
                                msgs.record_append_message(pre_append_tail, message.clone());
                                msgs.push(message);
                            }
                        }
                        RoundEvent::StreamDiscard => {
                            let position = positions_by_session.get(&session_id).copied();
                            let round = position.map(|(round, _)| round);
                            let turn = position.map(|(_, turn)| turn);
                            *provider_retry_clone.lock().await = None;
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
                                *provider_retry_clone.lock().await = None;
                                let mut msgs = buf.write().await;
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
                                !muta_contracts::model_by_id(&model_id)
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
                            // Identity-addressed append (ADR-0114): resolve the
                            // streaming Thinking entry for *this* (round, turn)
                            // by scanning backwards, not by "is last". A command
                            // entry (`/autopilot`, shell passthrough) or a local
                            // notice can be appended between reasoning deltas —
                            // under `last_mut()` addressing the next delta would
                            // fork the trace into a second Thinking entry. The
                            // scan stops at the last Thinking message of this
                            // position; older positions cannot match.
                            let changed = append_reasoning_delta(&mut msgs, round, turn, &delta);
                            if changed.is_none() {
                                // The first disclosed reasoning delta creates the visible
                                // reasoning component directly. `StreamStart` intentionally
                                // creates no transcript placeholder, so hidden-chain models
                                // cannot leave phantom spacing behind. Targeted append —
                                // the settled history's cached heights survive.
                                let (provider, model) =
                                    event_loop::attribution(&cp_clone, &cm_clone).await;
                                let effort = event_loop::picker_effort(
                                    &provider_picker_clone,
                                    &cp_clone,
                                    &cm_clone,
                                )
                                .await;
                                let mut thinking = TranscriptMessage::thinking(delta.clone())
                                    .with_attribution(provider, model)
                                    .with_effort(effort);
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
                                let pre_append_tail = msgs.last().map(|tail| tail.id);
                                msgs.record_append_message(pre_append_tail, thinking.clone());
                                msgs.push(thinking);
                                reasoning_start = Some(std::time::Instant::now());
                            }
                            if let Some(id) = changed {
                                msgs.record_reasoning_delta(id, delta);
                            }
                        }
                        RoundEvent::StreamReasoningEnd(content) => {
                            let duration_ms = reasoning_start
                                .take()
                                .map(|started| started.elapsed().as_millis() as u64);
                            let position = positions_by_session.get(&session_id).copied();
                            let round = position.map(|(round, _)| round);
                            let turn = position.map(|(_, turn)| turn);
                            // Targeted finalize (no full snapshot): a
                            // streaming write resolves the trace by position
                            // (ADR-0114), swaps in the finalized clone, and
                            // drops only that message's height entry. A
                            // finished trace gains a cached height (its
                            // summary stops moving), which is exactly why
                            // the entry must be evicted once.
                            let mut msgs = buf.write_streaming().await;
                            // The round closes with `AssistantEnd` *before* `ReasoningEnd`
                            // (see golden_reasoning_precedes_text_in_the_same_turn), so by
                            // the time this arrives the assistant's text message is usually
                            // the literal last message. Resolve by position (ADR-0114) —
                            // scanning backward for the most recent *streaming* Thinking
                            // entry of this (round, turn) — so an entry appended after the
                            // trace (command row, notice) cannot steal or orphan the
                            // finalize, and the spinner never runs forever.
                            let target = msgs.iter_mut().rfind(|message| {
                                message.is_thinking_streaming()
                                    && message.round == round
                                    && message.turn == turn
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
                                let finalized = last.clone();
                                let id = last.id;
                                msgs.invalidate_message_height(id);
                                msgs.record_replace_message(id, finalized);
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
                            let effort = event_loop::picker_effort(
                                &provider_picker_clone,
                                &cp_clone,
                                &cm_clone,
                            )
                            .await;
                            // Stamp the current ReAct turn so this step
                            // joins its compact sibling tool batch;
                            // `TurnStarted` has already populated the session
                            // position map.
                            let position = positions_by_session.get(&session_id).copied();
                            let sent_at_ms = event_loop::now_epoch_ms();
                            *provider_retry_clone.lock().await = None;
                            // Targeted append (no full snapshot): a new
                            // running tool step has no height-cache entry and
                            // cannot disturb any settled message's height,
                            // so the streaming patch path carries it and the
                            // frozen history keeps its cached heights.
                            let mut msgs = buf.write_streaming().await;
                            let pre_append_tail = msgs.last().map(|tail| tail.id);
                            // A tool step starts collapsed: there's no result to show
                            // yet. The lifecycle-aware default (see `step_interaction`)
                            // expands it on completion — Ok follows per-tool density,
                            // Failed/Denied force-expand to surface the error.
                            let mut message = TranscriptMessage::tool_step(id, name, arguments)
                                .with_attribution(provider, model)
                                .with_effort(effort)
                                .with_sent_at_ms(sent_at_ms);
                            if let Some((round, turn)) = position {
                                message = message.with_round(round).with_turn(turn);
                            }
                            msgs.record_append_message(pre_append_tail, message.clone());
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
                            // Targeted finalize (no full snapshot): finishing
                            // a running step swaps in the finished clone and
                            // evicts exactly that message's height entry (a
                            // finished step gains a cached height, and the
                            // lifecycle default may expand it — both reasons
                            // the old entry, if any, is stale).
                            let mut msgs = buf.write_streaming().await;
                            let mut finished = false;
                            let mut finalized_step: Option<(u64, TranscriptMessage)> = None;
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
                                    finalized_step = Some((existing.id, existing.clone()));
                                    break;
                                }
                            }
                            if !finished {
                                // No matching in-flight call (e.g. turn restored from
                                // history): synthesize a finished step with its default
                                // disclosure applied directly.
                                let effort = event_loop::picker_effort(
                                    &provider_picker_clone,
                                    &cp_clone,
                                    &cm_clone,
                                )
                                .await;
                                let mut message =
                                    TranscriptMessage::tool_step(id.clone(), name.clone(), "{}")
                                        .with_attribution(provider, model)
                                        .with_effort(effort);
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
                                let pre_append_tail = msgs.last().map(|tail| tail.id);
                                msgs.record_append_message(pre_append_tail, message.clone());
                                msgs.push(message);
                            }
                            if let Some((id, message)) = finalized_step {
                                msgs.invalidate_message_height(id);
                                msgs.record_replace_message(id, message);
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
                                muta_contracts::EnvoyEvent::PermissionRequest(req) => {
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
                                muta_contracts::EnvoyEvent::UserQuestionRequest(req) => {
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
                            // Nested assistant and reasoning deltas mutate a
                            // child of the enclosing tool step. Like top-level
                            // tool streams, they have no standalone
                            // height-cache entry.
                            let mut msgs = if matches!(
                                &event,
                                muta_contracts::EnvoyEvent::StreamDelta(_)
                                    | muta_contracts::EnvoyEvent::StreamReasoningDelta(_)
                            ) {
                                buf.write_streaming().await
                            } else {
                                buf.write().await
                            };
                            let applied = msgs
                                .iter_mut()
                                .find(|m| m.is_tool_step() && matches!(&m.kind, crate::model::document::MessageKind::ToolStep { id, .. } if id == &parent_call_id))
                                .is_some_and(|message| message.push_envoy_event(&event));
                            if applied
                                && matches!(
                                    &event,
                                    muta_contracts::EnvoyEvent::StreamDelta(_)
                                        | muta_contracts::EnvoyEvent::StreamReasoningDelta(_)
                                )
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
                            // surfaces (ADR-0017: the side runs on autopilot, so in
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
                            window_tokens_before,
                            window_tokens_after,
                        } => {
                            let mut msgs = buf.write().await;
                            push_local_notice(
                                &mut msgs,
                                NoticeSeverity::Info,
                                format!(
                                    "Compacted {} messages: {} -> {} tokens.",
                                    archived_messages, window_tokens_before, window_tokens_after
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
                            // View-scoped chrome: the authoritative
                            // running/idle transition for this session —
                            // start the timer on running, retire the
                            // activity surface on idle. Recorded for every
                            // session (asides included) so a background
                            // aside's finish is visible the moment its view
                            // is (re)focused. `can_retry` mirrors the
                            // session's durable `/retry` resume point
                            // (ADR-0128): offered exactly while a stopped
                            // round is parked, never for one that completed.
                            {
                                let round_counter = snapshot.round_counter;
                                let retry_pending = snapshot.retry_pending;
                                chrome_updater.edit(|c| {
                                    c.round_count = round_counter;
                                    c.responding = running;
                                    c.can_retry = retry_pending && !running;
                                    if running {
                                        c.current_turn = 0;
                                        if c.round_started_at.is_none() {
                                            c.round_started_at = Some(std::time::Instant::now());
                                        }
                                        if c.activity.is_empty() {
                                            c.activity = "running".to_string();
                                        }
                                    } else {
                                        c.activity.clear();
                                        c.current_turn = 0;
                                        c.round_started_at = None;
                                    }
                                });
                            }
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
                                    // Stamp the round timer so the activity bar can render a
                                    // live `<elapsed>` segment.
                                    *round_started_at_clone.lock().await =
                                        Some(std::time::Instant::now());
                                }
                                ir_clone.store(running, Ordering::SeqCst);
                                if !running {
                                    // The dispatch cycle is complete: any
                                    // command component still Pending will
                                    // never receive its reply on this pass
                                    // (modal/picker/side-view commands emit
                                    // no `RoundEvent::CommandResult`). Mark
                                    // it Cancelled so the row stops promising
                                    // an output (ADR-0108) — the invocation
                                    // stays readable, and the ledger still
                                    // holds the authoritative record.
                                    for message in messages_clone.write().await.iter_mut() {
                                        message.cancel_pending_command();
                                    }
                                    activity_clone.lock().await.clear();
                                    *current_turn_clone.lock().await = 0;
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
                            if !running {
                                *provider_retry_clone.lock().await = None;
                            }
                            let mut msgs = buf.write().await;
                            finalize_streaming_reasoning(&mut msgs, duration_ms);
                        }
                        RoundEvent::TodosUpdated(list) => {
                            if !routes_to_side {
                                *todos_clone.lock().await = Some(list);
                            }
                        }
                        RoundEvent::AutopilotChanged(enabled) => {
                            if !routes_to_side {
                                harness_clone.lock().await.autopilot = enabled;
                            }
                        }
                        RoundEvent::RetryScheduled {
                            attempt,
                            max_attempts,
                            delay_ms,
                            message,
                        } => {
                            let delay = std::time::Duration::from_millis(delay_ms);
                            let retry_at = std::time::Instant::now() + delay;
                            *provider_retry_clone.lock().await = Some(ProviderRetryState {
                                attempt,
                                max_attempts,
                                retry_at,
                                failure: message,
                            });
                            if !routes_to_side {
                                *activity_clone.lock().await = "waiting to retry".to_string();
                                ir_clone.store(true, Ordering::SeqCst);
                            }
                        }
                        RoundEvent::Error(e) => {
                            let last_retry = provider_retry_clone.lock().await.take();
                            let mut msgs = buf.write().await;
                            // A terminal round error may still carry the raw
                            // retryable-envelope encoding (e.g. a 429 that
                            // exhausted its retry budget): strip it so the
                            // user sees the message, never the wire framing.
                            let raw_msg = muta_contracts::public_error_message(&e);
                            let message = if let Some(retry) = last_retry
                                && retry.attempt > 1
                            {
                                if raw_msg.starts_with("Failed after")
                                    || raw_msg.starts_with("Exhausted")
                                {
                                    raw_msg
                                } else {
                                    format!(
                                        "Exhausted {} retry attempts · {}",
                                        retry.attempt, raw_msg
                                    )
                                }
                            } else {
                                raw_msg
                            };
                            push_local_notice(&mut msgs, NoticeSeverity::Error, message);
                            if !routes_to_side {
                                ir_clone.store(false, Ordering::SeqCst);
                                activity_clone.lock().await.clear();
                            }
                        }
                    } // end inner `match event`
                }
                AgentResponse::ParentStatus(status) => {
                    // ADR-0017: primary-session status for the `/btw` side
                    // banner. Mirrored into `App::parent_status` each frame.
                    *parent_status_clone.lock().await = status;
                }
                AgentResponse::SideViewOpened {
                    side_id,
                    messages,
                    commands,
                    round_interrupts,
                    ..
                } => {
                    // ADR-0017 + ADR-0103 §6: enter the aside view. Record the
                    // routing key so subsequent per-turn events stream into the
                    // side buffer, then back-fill that buffer from the event's
                    // transcript payload (the aside's full persisted history,
                    // inherited parent context included) so the viewed pixels
                    // match the model's actual context window — instead of the
                    // old behaviour of seeding an empty buffer.
                    listener_side_ids.insert(side_id.clone());
                    let mut rebuilt = transcript_messages_from_core(messages, &tui_config_clone);
                    rebuilt =
                        merge_command_rows(rebuilt, transcript_commands_from_ledger(commands));
                    rebuilt = merge_round_interrupt_rows(
                        rebuilt,
                        transcript_interrupts_from_records(round_interrupts),
                    );
                    *side_messages_clone.write().await = rebuilt;
                    *side_view_signal_clone.lock().await =
                        Some(event_loop::SideViewSignal::Opened { side_id });
                }
                AgentResponse::SideViewClosed => {
                    // ADR-0103: leave the aside view. The routing keys are
                    // NOT dropped — the asides keep running in the background
                    // and their events must keep streaming into the side
                    // buffer — only the view flips. Which ids stay is
                    // governed by `BtwList`: the next list refresh prunes ids
                    // whose asides closed (pristine discard / explicit close).
                    *side_view_signal_clone.lock().await = Some(event_loop::SideViewSignal::Closed);
                }
                AgentResponse::BtwList(rows) => {
                    // ADR-0103 §5: the asides list. Mirrored into the loop's
                    // `App::btw_list` each frame. The list is also the
                    // routing-truth source: ids absent from it no longer have
                    // a live aside, so their events stop routing to the side
                    // buffer.
                    listener_side_ids.retain(|id| rows.iter().any(|row| &row.id == id));
                    for row in rows.iter() {
                        listener_side_ids.insert(row.id.clone());
                    }
                    *btw_list_clone.lock().await = rows;
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
                AgentResponse::ConversationCleared { session_id } => {
                    messages_clone.write().await.clear();
                    *round_count_clone.lock().await = 0;
                    needs_round_rebase = false;
                    context_tokens_clone.lock().await.clear();
                    // `/new` minted a fresh session and switched to it. The
                    // same contract as `ConversationReplaced` below: track the
                    // post-switch id so session-scoped client state follows.
                    *live_session_id_clone.lock().await = session_id.clone();
                    *token_report_clone.lock().await = None;
                    *session_tree_clone.lock().await = None;
                }
                AgentResponse::ConversationReplaced {
                    session_id,
                    messages,
                    commands,
                    round_interrupts,
                } => {
                    let mut rebuilt = transcript_messages_from_core(messages, &tui_config_clone);
                    rebuilt =
                        merge_command_rows(rebuilt, transcript_commands_from_ledger(commands));
                    rebuilt = merge_round_interrupt_rows(
                        rebuilt,
                        transcript_interrupts_from_records(round_interrupts),
                    );
                    *messages_clone.write().await = rebuilt;
                    needs_round_rebase = true;
                    // The model-window revision changed; do not reuse an API
                    // anchor from the previous session/projection.
                    context_tokens_clone.lock().await.clear();
                    // Attached mode: the viewed primary session just switched
                    // (`/session open|new|fork`). Track the new id so
                    // session-scoped client state (the ↑/↓ prompt history
                    // origin, `TokenUsageReport` routing) follows, and drop
                    // the previous session's cached report.
                    *live_session_id_clone.lock().await = session_id.clone();
                    *token_report_clone.lock().await = None;
                    *session_tree_clone.lock().await = None;
                }
                AgentResponse::SessionsOverview(sessions) => {
                    *sessions_overview_clone.lock().await = sessions;
                    // Bump the revision so the loop's per-iteration mirror can
                    // skip the deep clone when the overview is unchanged.
                    sessions_overview_rev_clone.fetch_add(1, std::sync::atomic::Ordering::Release);
                }
                AgentResponse::OpenSessionsPanel => {
                    open_sessions_clone.store(true, Ordering::SeqCst);
                }
                AgentResponse::SessionTreeSnapshot { session_id, tree } => {
                    // A tree query is session-scoped. Reject a response that
                    // raced a primary-session switch; the switch arms its own
                    // refresh when the Tree view is next shown.
                    let live = live_session_id_clone.lock().await.clone();
                    if live == session_id {
                        *session_tree_clone.lock().await = Some(tree);
                    }
                }
                AgentResponse::OpenTreePanel => {
                    open_tree_clone.store(true, Ordering::SeqCst);
                }
                AgentResponse::OpenHostPanel => {
                    open_host_clone.store(true, Ordering::SeqCst);
                }
                AgentResponse::SessionDetail(detail) => {
                    *session_detail_clone.lock().await = Some(detail);
                }
                AgentResponse::TokenUsageReport { session_id, report } => {
                    // Install the daemon-side report only when it still
                    // belongs to the session the frontend is viewing — a
                    // reply that raced a session switch would otherwise
                    // populate the modal with the previous session's rows.
                    let viewed = viewed_session_id_clone.lock().await.clone();
                    if viewed.as_deref() == Some(session_id.as_str()) {
                        *token_report_clone.lock().await = Some(report);
                    }
                }
                AgentResponse::UsageStatsReport { report } => {
                    // Session-independent by design (ADR-0122): no
                    // viewed-session guard, the durable store aggregates
                    // across every session.
                    *usage_stats_clone.lock().await = Some(report);
                }
                AgentResponse::InputCompletions {
                    request_id,
                    input,
                    cursor,
                    items,
                } => {
                    *completion_signal_clone.lock().await = Some(event_loop::CompletionSignal {
                        request_id,
                        input,
                        cursor,
                        items,
                    });
                }
                AgentResponse::SessionContext(snapshot) => {
                    *session_context_clone.lock().await = Some(snapshot);
                }
                AgentResponse::Exit => {
                    should_quit_clone.store(true, Ordering::SeqCst);
                }
                AgentResponse::ProviderSwitched { provider, model } => {
                    // A provider/model switch refreshes the hint bar (the
                    // long-lived "still in effect" indicator) but is NOT
                    // appended to the transcript as an inline notice: the
                    // acknowledgment is a command ack (ADR-0088), surfaced as a
                    // transient toast the harness emits alongside this event
                    // for genuine user-initiated switches. Attach/startup
                    // synthetic replays of this event only re-hydrate the hint
                    // bar — no toast, no transcript row.
                    *cp_clone.lock().await = provider;
                    *cm_clone.lock().await = model;
                }
                AgentResponse::ConnectStatus(status) => {
                    let mut msgs = messages_clone.write().await;
                    match status {
                        muta_contracts::ConnectStatus::Pending {
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
                        muta_contracts::ConnectStatus::Done { provider } => {
                            *oauth_add_signal_clone.lock().await =
                                Some(event_loop::OauthAddSignal::Done);
                            push_local_notice(
                                &mut msgs,
                                NoticeSeverity::Info,
                                format!("{provider} authorized."),
                            );
                        }
                        muta_contracts::ConnectStatus::DiscoveryWarning { provider, message } => {
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
                        muta_contracts::ConnectStatus::Failed { provider, message } => {
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
                AgentResponse::WebSearchConfigSnapshot(snapshot) => {
                    *websearch_config_clone.lock().await = Some(snapshot);
                }
                AgentResponse::WebSearchConfigUpdated(snapshot) => {
                    // Authoritative post-update ack: the pane re-renders from
                    // persisted state, discarding any optimistic local edit.
                    *websearch_config_clone.lock().await = Some(snapshot);
                }
            }
        }
    });

    let messages_for_loop = messages.clone();

    let mut app = App {
        views: crate::views::ViewRegistry::new(),
        surfaces: if startup_overlay == StartupOverlay::SessionsPicker {
            crate::views::SurfaceRouter::with_view(crate::views::ViewId::Sessions)
        } else {
            crate::views::SurfaceRouter::new()
        },
        queue_exit_session: None,
        view_switcher_query: String::new(),
        input: String::new(),
        messages: Vec::new(),
        messages_version: 0,
        side_messages: Vec::new(),
        side_messages_version: 0,
        layout_height_cache: Default::default(),
        in_side_view: false,
        side_session_id: None,
        parent_status: ParentStatus::Idle,
        btw_list: Vec::new(),
        session_chrome: std::collections::HashMap::new(),
        saved_primary_chrome: None,
        btw_scroll: 0,
        btw_modal_follow: true,
        session_tree: muta_contracts::SessionTree::default(),
        tree_scroll: 0,
        tree_modal_follow: true,
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
        token_report: None,
        context_tokens: None,
        token_report_scroll: 0,
        token_report_detail: false,
        usage_stats: None,
        usage_stats_scroll: 0,
        todos_rect: None,
        queue_rect: None,
        modal_rect: None,
        modal_body_height: 0,
        sticky_summary_line: None,
        pin_summary_line: None,
        scroll_settle_pending: false,
        focus_stack: Vec::new(),
        tx,
        should_quit,
        suggestion_index: None,
        completion_dismissed: false,
        command_catalog,
        backend_completions: Vec::new(),
        completion_response_input: None,
        completion_response_cursor: 0,
        completion_requested: None,
        completion_request_id: 0,
        cursor_position: 0,
        input_scroll: 0,
        modal_index: 0,
        last_key_press: std::time::Instant::now(),
        session_scroll: 0,
        session_modal_follow: true,
        session_info_detail: false,
        session_detail: None,
        session_info_scroll: 0,
        permissions_scroll: 0,
        config_scroll: 0,
        config_focus: crate::overlays::ConfigFocus::Categories,
        config_category: 0,
        config_detail_index: 0,
        config_detail_scroll: 0,
        config_custom_editing: false,
        websearch_config: None,
        websearch_editing: None,
        skills_expanded: None,
        history_scroll: 0,
        history_modal_follow: true,
        history_preview: false,
        history_search: false,
        current_provider: initial_provider,
        current_model: initial_model,
        cwd: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        current_session_id: String::new(),
        current_workspace: String::new(),
        session_context: None,
        loop_status: LoopStatus::Idle,
        harness_retry_pending: false,
        activity_status: String::new(),
        provider_retry: None,
        autopilot: false,
        todos: None,
        round_count: 0,
        current_turn: 0,
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
        host_sessions: Vec::new(),
        host_scroll: 0,
        host_modal_follow: true,
        host_focus: crate::overlays::DashboardFocus::Detail,
        host_detail_scroll: 0,
        host_preview: None,
        host_preview_scroll: 0,
        host_prompting: false,
        host_prompt_new: false,
        host_console_log: Vec::new(),
        host_kill_confirm: None,
        host_kill_confirm_id: None,
        switch_to_target: None,
        startup_overlay,
        permission_confirm_always: false,
        permission_show_details: false,
        permission_scroll: 0,
        permission_max_scroll: 0,
        input_history: if input_history_config.record_commands {
            input_history
        } else {
            // `[input_history] record_commands = false` (default): scrub any
            // legacy `/slash` invocations from the loaded history so they stop
            // showing in the picker, and — since this list is what gets
            // `save_history`d on exit — the on-disk file heals itself too.
            input_history
                .into_iter()
                .filter(|e| !e.text.starts_with('/'))
                .collect()
        },
        history_index: None,
        history_draft: String::new(),
        history_draft_images: Vec::new(),
        history_draft_text_pastes: Vec::new(),
        queue_pointer: None,
        queue_pointer_draft: String::new(),
        queue_pointer_draft_images: Vec::new(),
        queue_pointer_draft_text_pastes: Vec::new(),
        history_attachments: std::collections::HashMap::new(),
        history_attachments_order: std::collections::VecDeque::new(),
        session_history_backfill: Vec::new(),
        session_history_backfill_cursor: 0,
        history_clear_confirm: false,
        input_history_dedup: input_history_config.dedup,
        input_history_record_commands: input_history_config.record_commands,
        // This is the production TUI path (see `main` → `run_tui`): the
        // process owns the user's real `history.json`, so disk persistence
        // is enabled. Tests build `App` directly and keep this `false` so
        // they never write to (or truncate) the user's state directory.
        input_history_persist: true,
        pending_images: Vec::new(),
        pending_text_pastes: Vec::new(),
        pending_dispatch: std::collections::VecDeque::new(),
        queue_blocked_sessions: std::collections::HashSet::new(),
        naturally_completed_sessions: std::collections::HashSet::new(),
        idle_sessions: std::collections::HashSet::new(),
        running_sessions: std::collections::HashSet::new(),
        selection: SelectionState::None,
        drag: SelectionDrag::default(),
        layout_map: LayoutMap::new(),
        modal_hit_map: crate::model::layout::ModalHitMap::new(),
        hovered_step: None,
        transcript_layout: crate::view::layout::Strategy::from_config(
            &tui_config.transcript_layout,
        ),
        color_scheme: Theme::normalize_color_scheme(&tui_config.color_scheme).to_string(),
        custom_color_scheme: tui_config.custom_color_scheme.clone(),
        custom_color_draft: tui_config.custom_color_scheme.clone(),
        click_outside_dismiss: tui_config.click_outside_dismiss,
        expand_auto_scroll: tui_config.expand_auto_scroll,
        focused_target: None,
        copy_toast_until: None,
        copy_toast_message: String::new(),
        copy_toast_failed: false,
        notice_toast_until: None,
        notice_toast_message: String::new(),
        notice_toast_severity: NoticeSeverity::Info,
        ctrl_c_armed_until: None,
        esc_armed_until: None,
        spinner_epoch: std::time::Instant::now(),
        carousel_epoch: std::time::Instant::now(),
        effort_ignition_epoch: None,
        injection_stashed_input: String::new(),
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
        custom_auth: muta_contracts::ChannelAuth::ApiKey,
        custom_template_id: None,
        awaiting_oauth_add: false,
        oauth_pending_message: String::new(),
        oauth_pending_url: String::new(),
        oauth_pending_user_code: String::new(),
        oauth_pending_error: None,
        oauth_selected_item: 0,
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

    if startup_overlay == StartupOverlay::SessionsPicker {
        app.views.open(crate::views::ViewId::Sessions);
    }

    // Run app
    let res = event_loop::run_app_loop(
        &mut terminal,
        &mut app,
        event_loop::UiRuntime {
            current_provider,
            current_model,
            context_tokens,
            harness,
            activity_status,
            provider_retry,
            pending_permission,
            pending_question,
            pending_input,
            is_responding,
            dirty,
            dirty_notify,
            completion_signal,
            envoy_permission_parent,
            envoy_question_parent,
            messages: messages_for_loop,
            side_messages,
            parent_status,
            side_view_signal,
            btw_list,
            session_chrome,
            host_console_signal,
            viewed_session_id,
            live_session_id,
            key_status,
            provider_picker,
            sessions_overview,
            sessions_overview_rev,
            session_detail,
            session_tree,
            token_report,
            usage_stats,
            websearch_config,
            open_sessions,
            open_tree,
            host_sessions,
            host_sessions_rev,
            open_host,
            oauth_add_signal,
            awaiting_oauth_add,
            session_context,
            todos,
            round_count,
            current_turn,
            round_started_at,
            unsent_input_signal,
            notice_toast_signal,
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

    let switch = app.switch_to_target.take();
    Ok(TuiOutcome {
        history: app.input_history,
        switch_to: switch,
    })
}

/// What a TUI run produced (ADR-0096): the input history to persist, and —
/// when the user picked a session in the `/host` panel — the daemon session
/// to switch to (the caller re-attaches).
pub struct TuiOutcome {
    pub history: Vec<muta_contracts::HistoryEntry>,
    pub switch_to: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub async fn start_tui(
    tx: mpsc::UnboundedSender<AgentRequest>,
    rx: mpsc::UnboundedReceiver<AgentResponse>,
    initial_provider: String,
    initial_model: String,
    input_history: Vec<muta_contracts::HistoryEntry>,
    initial_messages: Vec<Message>,
    initial_commands: Vec<muta_contracts::CommandRecord>,
    initial_round_count: u64,
    command_catalog: muta_contracts::CommandCatalog,
    initial_round_interrupts: Vec<muta_contracts::RoundInterrupt>,
    tui_config: config::TuiConfig,
    input_history_config: config::InputHistoryConfig,
    session: SessionSource,
    token_ledger: Option<Arc<muta_contracts::TokenSourceLedger>>,
    startup_overlay: StartupOverlay,
) -> Result<TuiOutcome, Box<dyn Error>> {
    run_tui(
        tx,
        rx,
        initial_provider,
        initial_model,
        input_history,
        initial_messages,
        initial_commands,
        initial_round_count,
        command_catalog,
        initial_round_interrupts,
        tui_config,
        input_history_config,
        session,
        token_ledger,
        startup_overlay,
    )
    .await
}

fn push_core_notice(messages: &mut Vec<TranscriptMessage>, notice: &muta_contracts::AgentNotice) {
    let _surface = notice.surface;
    messages.push(
        TranscriptMessage::notice(
            notice_severity_from_core(notice.severity),
            notice.render_text(),
        )
        .with_sent_at_ms(event_loop::now_epoch_ms()),
    );
}

/// Apply the visible transcript effect of a stream-start signal. The signal
/// deliberately creates no message: transport lifecycle alone must not influence
/// transcript geometry.
fn begin_stream(_messages: &mut Vec<TranscriptMessage>) {}

/// Append a disclosed reasoning delta to the current turn's Thinking entry,
/// creating the entry only when the first disclosed delta arrives (that
/// structural path lives at the call site). Returning `Some(id)` permits the
/// cheap per-message patch path; `None` means the caller must create the
/// entry.
///
/// Identity-addressed (ADR-0114): resolves the target by scanning backwards
/// for the Thinking entry matching `(round, turn)`, **not** by "is the last
/// message". Command entries (`/autopilot`, shell passthrough) and local
/// notices can be appended between reasoning deltas — under `last_mut()`
/// addressing the next delta would fork the trace into a second Thinking
/// entry (the "two Thinking blocks" bug).
fn append_reasoning_delta(
    messages: &mut [TranscriptMessage],
    round: Option<u64>,
    turn: Option<u64>,
    delta: &str,
) -> Option<u64> {
    let target = messages
        .iter_mut()
        .rfind(|message| message.is_thinking() && message.round == round && message.turn == turn)?;
    target.push_stream(delta);
    if let MessageKind::Thinking { content, .. } = &mut target.kind {
        content.push_str(delta);
    }
    Some(target.id)
}

/// Append a streamed assistant-text delta to the current turn, creating the
/// message only when the first visible text arrives. Returning `None` means the
/// caller must perform the structural insertion (and request a full transcript
/// snapshot); returning an id permits the cheap per-message patch path.
///
/// Identity-addressed (ADR-0114): the target assistant-text entry is resolved
/// by scanning backwards for the entry matching `(round, turn)`, **not** by
/// "is the last message". Command entries and local notices appended between
/// text deltas must not fork the stream into a second entry.
fn append_stream_text_delta(
    messages: &mut [TranscriptMessage],
    round: Option<u64>,
    turn: Option<u64>,
    delta: &str,
) -> Option<u64> {
    let message = messages.iter_mut().rfind(|message| {
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
    // Timestamped like the command rows (`sent_at_ms` → trailing ` · HH:MM`)
    // so a locally synthesized notice reads as the same kind of transcript
    // entry, with an anchor for "when did this happen" after further output
    // has scrolled it up.
    messages.push(
        TranscriptMessage::notice(severity, text).with_sent_at_ms(event_loop::now_epoch_ms()),
    );
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
    let done = new.count(muta_contracts::TodoStatus::Completed);
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
    use muta_contracts::{TodoId, TodoItem, TodoStatus};

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

    // ── Identity-addressed streaming appends (ADR-0114) ──────────────────

    fn thinking_entry(round: u64, turn: u64, content: &str) -> TranscriptMessage {
        let mut m = TranscriptMessage::thinking(content);
        m.round = Some(round);
        m.turn = Some(turn);
        m
    }

    #[test]
    fn reasoning_delta_appends_across_an_intervening_command_entry() {
        // Regression (ADR-0114): dispatching `/autopilot` mid-stream pushes a
        // CommandResult entry after the still-streaming Thinking entry. The
        // next reasoning delta must extend the *original* entry, not fork a
        // second Thinking block.
        let mut messages = vec![thinking_entry(8, 1, "the error chain is")];
        messages.push(TranscriptMessage::pending_command("autopilot", "on").with_sent_at_ms(1_000));

        let id = append_reasoning_delta(&mut messages, Some(8), Some(1), " now clear")
            .expect("must resolve the original thinking entry");
        assert_eq!(id, messages[0].id);
        // Still exactly one Thinking entry…
        assert_eq!(
            messages.iter().filter(|m| m.is_thinking()).count(),
            1,
            "the delta must not fork a second Thinking entry"
        );
        // …and the delta landed inside it, in order.
        let MessageKind::Thinking { content, .. } = &messages[0].kind else {
            panic!("entry 0 must remain a Thinking entry");
        };
        assert_eq!(content, "the error chain is now clear");
        // The command entry stays between the original position and the end,
        // untouched.
        assert!(messages[1].is_command_result());
    }

    #[test]
    fn reasoning_delta_finds_latest_entry_of_same_turn() {
        // Multiple thinking entries can share a position across retries; the
        // backward scan must hit the newest one.
        let mut messages = vec![
            thinking_entry(2, 1, "first attempt"),
            thinking_entry(2, 1, "second attempt"),
        ];
        let id = append_reasoning_delta(&mut messages, Some(2), Some(1), "…").unwrap();
        assert_eq!(id, messages[1].id);
        let MessageKind::Thinking { content, .. } = &messages[1].kind else {
            panic!()
        };
        assert_eq!(content, "second attempt…");
        let MessageKind::Thinking { content, .. } = &messages[0].kind else {
            panic!()
        };
        assert_eq!(content, "first attempt");
    }

    #[test]
    fn reasoning_delta_rejects_foreign_positions() {
        // A delta for another turn must not graft onto an older turn's entry.
        let mut messages = vec![thinking_entry(8, 1, "old")];
        assert_eq!(
            append_reasoning_delta(&mut messages, Some(8), Some(2), "new"),
            None
        );
        assert_eq!(
            append_reasoning_delta(&mut messages, Some(9), Some(1), "new"),
            None
        );
    }

    #[test]
    fn text_delta_appends_across_an_intervening_command_entry() {
        use muta_contracts::Role;
        let mut text = TranscriptMessage::new(Role::Assistant, "hello ");
        text.round = Some(3);
        text.turn = Some(1);
        let mut messages = vec![text];
        messages.push(TranscriptMessage::pending_command("autopilot", "on").with_sent_at_ms(1_000));

        let id = append_stream_text_delta(&mut messages, Some(3), Some(1), "world")
            .expect("must resolve the original text entry");
        assert_eq!(id, messages[0].id);
        assert!(messages[0].raw.contains("world"));
        assert_eq!(
            messages.iter().filter(|m| m.raw.contains("world")).count(),
            1,
            "the delta must not fork a second text entry"
        );
    }
}

/// Load the user-supplied ASCII logo from `$XDG_CONFIG_HOME/muta/logo.txt`,
/// clamped to the empty-state bounding box. Best-effort: a missing or unreadable
/// file returns `None`, leaving the built-in wordmark in place.
fn load_user_logo() -> Option<Vec<String>> {
    let path = muta_persistence::paths::get().logo_file();
    let raw = std::fs::read_to_string(&path).ok()?;
    // Re-use the renderer's parser so the clamp stays defined in one place.
    // The parser already strips CRLF/trailing blanks and truncates to the box.
    view::parse_logo(&raw)
}

#[cfg(test)]
pub(crate) mod tests;
