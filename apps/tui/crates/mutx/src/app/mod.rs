//! The TUI's central application state ([`App`]) plus the `Modal` kind and
//! the `impl App` blocks that hold pure state-management methods.
//!
//! Input-box completion lives in [`crate::completion`]; the event/render
//! loop and shared runtime live in `crate::event_loop`. Everything
//! else that mutates `App` either lives here (state navigation, focus,
//! sticky/pinned step bookkeeping) or in `completion.rs` (the only other
//! `impl App` block).
//!
//! [`crate::completion`]: crate::completion

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tokio::sync::mpsc;

use muta_contracts::{
    AgentRequest, ChannelAuth, ImagePart, LoopStatus, ParentStatus, PermissionRequest,
    ProviderPickerSnapshot, SessionOverview, TodoList,
};

use crate::completion::CompletionItemKind;
use crate::composer_attachments;
use crate::event_loop::resolve_focused_mut;
use crate::fuzzy;
use crate::model::document::{NoticeSeverity, TranscriptMessage};
use crate::model::layout::{InteractiveTarget, LayoutMap, ModalHitMap};
use crate::model::selection::{SelectionDrag, SelectionState};
use crate::providers::{
    CustomField, ProviderPreset, RankedModel, RankedProvider, edit_fields,
    models_flat_filtered_from, providers_filtered_from,
};
use crate::view::Theme;
use crate::{ActivityTab, Modal};

use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueuedDispatchState {
    /// Staged in the outbox, waiting for its turn (auto-drain or a recall /
    /// delete / reorder from the Queue modal).
    Waiting,
    /// A fresh round is being started for this item (`FollowUp` sent,
    /// `FollowUpStarted` not yet received).
    Dispatching,
}

/// Target queue mode for the live composer while a round is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ComposerSendMode {
    /// Send as steering input at the next safe turn boundary.
    #[default]
    Steer,
    /// Send as follow-up input when the agent finishes active work.
    FollowUp,
}

/// A user message owned by the compact outbox (the **next-round** queue).
///
/// Follow-up content and a busy-Enter steer whose round ended before admission
/// (handed back by `UserInputUnavailable`) wait here to become the **next**
/// round's prompt. The item is intentionally absent from the transcript until
/// the harness dispatches it, so pending state never scrolls away or
/// masquerades as conversation history. (A *live* insert is different: it is
/// a transcript entry from the moment it is sent — see
/// `DeliveryStatus::Queued` — and never passes
/// through the outbox.)
#[derive(Debug, Clone)]
pub struct QueuedDispatch {
    pub id: String,
    pub session_id: String,
    pub state: QueuedDispatchState,
    /// The user's literal prompt text, sent verbatim to the agent on dispatch.
    pub text: String,
    /// When the item was staged (epoch ms). Surfaced by the persistent queue
    /// bar and the Queue modal as the item's send time, distinct from the
    /// `sent_at_ms` stamped on the dispatch request — this is *queued-at*.
    pub queued_at_ms: u64,
    /// Pasted images staged for this message (Ctrl+V). Empty for plain text.
    pub images: Vec<ImagePart>,
    /// Large pasted text blocks staged behind `[Pasted text #N +M lines]`
    /// chips inside `text`. Empty for plain-text drafts. Order matches the
    /// chip numbering, so the Nth chip expands to `pending_text_pastes[N-1]`.
    pub text_pastes: Vec<String>,
}

/// The attachments staged behind a recorded history entry, retained **in
/// memory** so ↑/↓ and Ctrl+R recall can restore a just-sent / interrupted
/// message's images and large pastes. Keyed by the same `(text, session_id)`
/// identity [`muta_contracts::merge_history`] uses, so a recall finds the
/// payloads that shipped with the exact prompt text.
///
/// Deliberately **not** persisted: `history.json` is rebuildable cosmetic
/// telemetry (ADR-0018), and base64 image blobs would balloon the file and
/// duplicate conversation data. The cache lives for the process lifetime
/// (capped, newest-first) and is re-seeded on every send, which is exactly
/// the window the interrupt → ↑/↓ → resend flow needs.
#[derive(Debug, Clone, Default)]
pub struct HistoryAttachments {
    pub images: Vec<ImagePart>,
    pub text_pastes: Vec<String>,
}

/// Outcome of [`App::recall_queued`]. Every queued dispatch is a next-round
/// item, so recall always restores the newest staged message into the
/// composer immediately (no agent roundtrip to cancel).
pub enum RecallQueued {
    Restored(QueuedDispatch),
}

/// Which surface owns the terminal cursor right now — the single source of
/// truth that the event loop's hide/show state machine, the immediate
/// pre-draw cursor re-sync, and the composer's `show_caret` flag all derive
/// from.
///
/// The terminal cursor is what the host terminal's IME anchors its
/// composition window to, so the owner must be exactly the one text-input
/// surface the user is typing into — or [`Self::None`] when no such surface
/// exists (a transcript step has keyboard focus, the view is zoomed into an
/// runner task, or a read-only / decision modal is open). In the `None` case
/// the cursor is hidden so the IME has no stale anchor to bind to, which is
/// the bug that previously let the IME "drift" when a disclosure was
/// clicked mid-composition: the caret left the composer but the cursor
/// stayed visible at its old coordinate.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum CaretOwner {
    /// The live composer (no modal, no runner zoom, no transcript-step focus).
    Composer,
    /// A modal that renders its own caret (`Modal::owns_caret`).
    Modal,
    /// No text-input surface is active — the cursor must be hidden.
    None,
}

/// Which end of an active input selection the caret should adopt when the
/// selection is broken. `Head` is the edge nearest the hidden caret — the
/// point where the mouse button was released for a drag selection — while
/// `Tail` is the opposite end (where the drag began).
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum SelectionEdge {
    Tail,
    Head,
}

/// Capturable snapshot of the main transcript's scroll position, saved when
/// zooming into a nested view and restored on return.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct ScrollSnapshot {
    pub offset: u16,
    pub follow_bottom: bool,
}

/// One frame on the focus stack: the runner task call-id plus the parent
/// view's scroll snapshot, restored verbatim when the frame is popped.
#[derive(Clone, Debug, PartialEq)]
pub struct ZoomFrame {
    pub call_id: String,
    pub saved_scroll: ScrollSnapshot,
}

/// Which button is focused in the provider-delete confirm overlay
/// ([`App::pending_provider_delete`]). `Cancel` is the safe default — Enter
/// dismisses without deleting; the user must move focus to `Delete` to destroy
/// the provider. The derive places `Default` on the first variant (`Cancel`),
/// matching the "safe-default" contract documented above.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProviderDeleteChoice {
    #[default]
    Cancel,
    Delete,
}

/// Transient state representing an ongoing provider retry countdown or execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRetryState {
    pub attempt: usize,
    pub max_attempts: usize,
    pub retry_at: std::time::Instant,
    pub failure: String,
}

impl ProviderRetryState {
    pub fn summary(&self, now: std::time::Instant) -> String {
        let retry = self.attempt.saturating_sub(1);
        let max_retries = self.max_attempts.saturating_sub(1).max(retry);
        let timing = if now < self.retry_at {
            format!(
                "next in {}",
                format_retry_duration(self.retry_at.saturating_duration_since(now))
            )
        } else {
            format!(
                "running for {}",
                format_retry_duration(now.saturating_duration_since(self.retry_at))
            )
        };
        format!("retry {retry}/{max_retries} ({timing})")
    }
}

pub fn format_retry_duration(duration: std::time::Duration) -> String {
    let millis = duration.as_millis() as u64;
    if millis >= 10_000 {
        format!("{}s", millis.div_ceil(1_000))
    } else {
        format!("{:.1}s", millis as f64 / 1_000.0)
    }
}

/// The view-scoped chrome of one session: the typed activity phase, the
/// responding flag, and the structural round/turn counters. Each session —
/// the primary and every live `/btw` aside — owns an entry in
/// [`App::session_chrome`], and a view renders exclusively from the entry of
/// the session it displays ([`App::viewed_chrome`]). This is what keeps an
/// aside view from inheriting the primary's activity bar (and vice versa):
/// before this type existed these were single global fields, so whichever
/// view was focused showed the *primary's* state no matter which session was
/// actually streaming.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionChrome {
    /// Typed activity phase (None when idle). Per session: a background
    /// aside's live phase lives in its own entry, invisible to the main view.
    pub phase: Option<crate::phase::Phase>,
    /// Whether this session currently has a live round (drives the
    /// breathing/spinner animation and Esc-to-interrupt arming).
    pub responding: bool,
    /// Round counter for this session (Activity modal's `round N`).
    pub round_count: u64,
    /// Current turn within this session's round (1-indexed for display).
    pub current_turn: u64,
    /// When this session's current round started (elapsed-timer segment).
    pub round_started_at: Option<std::time::Instant>,
    /// Whether this session has a stopped round parked for `/retry`
    /// (ADR-0128). Mirrored from the session-scoped harness snapshot — the
    /// authoritative durable resume point, not a transcript scan — so the
    /// hint bar only offers `/retry` for a round that actually stopped
    /// before completing.
    pub can_retry: bool,
    /// Latest completed principal ReAct turn's performance sample. Kept
    /// session-scoped so primary and `/btw` views never borrow each other's
    /// hint-bar measurement.
    pub last_turn_performance: Option<muta_contracts::TurnPerformanceSnapshot>,
}

/// Whether [`App::adopt_as_draft`] may clobber a composer that currently
/// holds unsent, unsaved work. Explicit user gestures (queue recall,
/// Ctrl+R insert) may — the user asked for it. Asynchronous events
/// (a Phase-1 unsend restore) may not — the in-progress draft they would
/// destroy was never sent anywhere and has no other copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftAdoption {
    /// Replace whatever the composer holds. For user-initiated paths where
    /// the current content is either sent or superseded.
    Replace,
    /// Adopt only if the composer is idle (empty text, no staged
    /// attachments). Otherwise leave the in-progress draft untouched. For
    /// the asynchronous Phase-1 unsend restore.
    OnlyIfIdle,
}

pub struct App {
    pub input: String,
    /// Structured transcript messages (semantic document model).
    pub messages: Vec<TranscriptMessage>,
    /// Version of the shared runtime buffer that `messages` was last synced
    /// from. The loop re-clones the buffer only when the runtime version moves
    /// past this, so an unchanged transcript costs no per-frame deep clone.
    /// Starts at 0 (the `Versioned` sentinel) so the first frame always syncs.
    pub messages_version: u64,
    /// Side-conversation transcript (ADR-0017). Populated only while a `/btw`
    /// side session is live; per-turn events tagged with the side `session_id`
    /// route here instead of into `messages`.
    pub side_messages: Vec<TranscriptMessage>,
    /// Companion to `messages_version` for the side buffer.
    pub side_messages_version: u64,
    /// Per-message laid-out height cache (Stage 2). Lets the transcript renderer
    /// skip re-wrapping off-screen messages, making per-frame layout O(visible)
    /// instead of O(transcript). Cleared whenever the transcript changes (a
    /// `messages_version` / `side_messages_version` bump) so a cached height is
    /// only ever read while the message's content is unchanged.
    pub layout_height_cache: crate::view::HeightCache,
    /// True while the user is composing into the `/btw` aside view
    /// (ADR-0017/0103). Drives [`App::focused_messages`] to swap the viewed
    /// transcript to [`App::side_messages`] and reserves the aside header.
    pub in_side_view: bool,
    /// Active side `session_id`, learned from `AgentResponse::SideViewOpened`.
    /// The response listener routes a `Turn { session_id, .. }` event into the
    /// side buffer when this matches, and into the primary buffer otherwise.
    pub side_session_id: Option<String>,
    /// Coarse primary-session status, mirrored from
    /// `AgentResponse::ParentStatus` for the side banner.
    pub parent_status: ParentStatus,
    /// Live `/btw` asides list (ADR-0103), mirrored from
    /// `AgentResponse::BtwList`. Drives the asides modal and the main view's
    /// header aside count. Kept even while inside an aside view so jumping
    /// back never needs a round trip.
    pub btw_list: Vec<muta_contracts::BtwAsideSummary>,
    /// Per-session chrome (activity text, responding flag, round/turn
    /// counters) for every session this client has observed, keyed by
    /// `session_id` — the primary **and** every live aside. A view renders
    /// from its own session's entry (see [`App::viewed_chrome`]), so an
    /// aside view never inherits the primary's activity bar and the primary
    /// never shows an aside's: chrome is view-scoped, not global (the
    /// pre-scoped fields below remain the *primary's* entry and the source
    /// of truth for the main view).
    pub session_chrome: std::collections::HashMap<String, SessionChrome>,
    /// The primary's chrome, saved when entering an aside view and restored
    /// on exit. Entering swaps the *displayed* chrome to the aside's own
    /// [`SessionChrome`] entry; exiting restores the primary's exactly as it
    /// was (a running primary round keeps its activity bar, elapsed timer,
    /// and counters across the aside detour).
    pub saved_primary_chrome: Option<SessionChrome>,
    /// Scroll + follow slots for the asides modal (shared pattern with the
    /// sessions picker: `session_scroll` / `session_modal_follow`).
    pub btw_scroll: usize,
    pub btw_modal_follow: bool,
    pub scroll: u16,
    /// Whether the view follows the newest content (auto-scroll to bottom).
    pub follow_bottom: bool,
    /// Last measured stream height in lines and viewport height, used to pin
    /// the view to the bottom while following.
    pub content_lines: usize,
    pub view_height: u16,
    pub max_scroll: u16,
    /// Expanded step pinned under the HUD bar (its message index + screen rect),
    /// when its body is scrolled into view. Clicks inside the rect collapse it.
    pub sticky_step: Option<usize>,
    pub sticky_rect: Option<mutx_engine::Rect>,
    /// Screen rect of the activity bar for the current frame, so clicks inside
    /// it open the Activity modal. `None` when no activity bar is shown (idle,
    /// streaming, runner view, or chrome hidden).
    pub activity_rect: Option<mutx_engine::Rect>,
    /// Screen rect of the context-meter segment in the hint bar (the
    /// `89.2k (8%)` indicator), so a click on it opens the TokenReport modal.
    /// `None` when the hint bar or context meter is not shown.
    pub hint_context_rect: Option<mutx_engine::Rect>,
    /// Screen rect of the last-turn stream-rate segment in the hint bar.
    /// Clicking it opens the independent PerformanceReport modal.
    pub hint_performance_rect: Option<mutx_engine::Rect>,
    /// Shared token-source ledger (reported vs. estimated token accounting),
    /// read by the TokenReport modal. `Some` in the standalone path (the
    /// in-process harness shares this ledger); `None` in attach mode, where
    /// the accounting lives daemon-side and the modal renders the on-demand
    /// [`Self::token_report`] snapshot instead.
    pub token_ledger: Option<Arc<muta_contracts::TokenSourceLedger>>,
    /// Token-source report fetched on demand from the harness for the viewed
    /// session. Populated by a `QueryTokenUsage` round-trip when the
    /// TokenReport modal opens in attach mode (`token_ledger` is `None`);
    /// `None` while the round-trip is in flight (the modal renders a loading
    /// placeholder). Cleared when the viewed session switches.
    pub token_report: Option<muta_contracts::TokenSourceReport>,
    /// Latest session-scoped AI context snapshot from the harness. This is a
    /// provider usage/projection value, never a persisted transcript estimate.
    pub context_tokens: Option<muta_contracts::ContextTokenSnapshot>,
    /// Scroll offset of the TokenReport modal body.
    pub token_report_scroll: usize,
    /// `true` when the TokenReport modal is drilled into one round's ReAct-turn
    /// usage; `false` when it shows the session's round list.
    pub token_report_detail: bool,
    /// Attempt-row cursor inside the drilled round's turn table (display
    /// order, newest first). Drives row highlighting and the Enter target
    /// for the third drill level — mirrors `performance_report_turn_cursor`.
    pub token_report_turn_cursor: usize,
    /// The open per-attempt usage page (`Context Usage › x round › x turn`),
    /// keyed by the attempt's `(turn, attempt)` so it survives ledger
    /// snapshots that grow between frames. `None` while the round detail or
    /// the round list is shown.
    pub token_report_turn: Option<(u32, u32)>,
    /// Scroll offset and hierarchy state for the independent performance
    /// report. It shares the request-ledger snapshot as a data source but no
    /// navigation or rendering state with Context Usage.
    pub performance_report_scroll: usize,
    pub performance_report_detail: bool,
    /// Attempt-row cursor inside the drilled round's "Turns / attempts"
    /// table (display order, newest first). Drives row highlighting and
    /// the Enter target for the third drill level.
    pub performance_report_turn_cursor: usize,
    /// The open attempt stage page (`Performance › x round › x turn`),
    /// keyed by the attempt's `(turn, attempt)` so it survives ledger
    /// snapshots that grow between frames. `None` while the round detail
    /// or the round list is shown.
    pub performance_report_turn: Option<(u32, u32)>,
    /// Cross-session usage-statistics report fetched on demand from the
    /// harness (`QueryUsageStats`, ADR-0122). Session-independent: it
    /// aggregates the durable store under `data/usage/`, which survives
    /// session cleanup. `None` while the round-trip is in flight (the
    /// overlay renders a loading placeholder).
    pub usage_stats: Option<muta_contracts::usage_stats::UsageStatsReport>,
    /// Scroll offset of the usage-statistics overlay body.
    pub usage_stats_scroll: usize,
    /// Screen rect of the todo bar (the one-row task-list summary), so a click
    /// on it opens the Activity modal directly on the Todos section. `None`
    /// when no todos are shown (empty task list or bar hidden).
    pub todos_rect: Option<mutx_engine::Rect>,
    /// Screen rect of the persistent queue bar (the one-row outbox summary),
    /// so a click anywhere on it expands the full Queue modal. `None` when the
    /// bar is hidden (chrome hidden or runner zoom).
    pub queue_rect: Option<mutx_engine::Rect>,
    /// Screen rect of the currently-open dismissable overlay modal (the
    /// centered panel, not the full-screen backdrop), so a click that lands
    /// outside it closes the modal — mirroring Esc. Written each render from
    /// the rect returned by the modal renderer. `None` when no modal is open,
    /// when the modal paints no full backdrop (Permission), or when it borrows
    /// the composer input and therefore must close through its own restore
    /// path (Provider / ModelEditor).
    pub modal_rect: Option<mutx_engine::Rect>,
    /// The body (scrollable content) height of the currently-open overlay
    /// modal, captured each render from the rect the modal renderer paints
    /// its body into. This is the per-modal equivalent of `view_height` (which
    /// measures the transcript viewport) and is what `ScrollPageUp` /
    /// `ScrollPageDown` use as the page step so a page advance always matches
    /// the actual modal body rather than the transcript behind it. `0` when
    /// no modal is open (or before the first render after one opens), in
    /// which case page handlers fall back to `view_height`.
    pub modal_body_height: u16,
    /// Screen rect of the provider-delete confirm overlay panel
    /// ([`App::pending_provider_delete`]), recorded each render so the
    /// mouse branch can detect outside-click dismissal (a press outside the
    /// panel cancels the staged deletion but leaves the provider picker open).
    pub provider_delete_rect: Option<mutx_engine::Rect>,
    /// Content-line index of the sticky step's real summary. Used to re-anchor
    /// the scroll offset when the user collapses the pinned step so the summary
    /// lands at the top of the viewport instead of jumping to unrelated content.
    pub sticky_summary_line: Option<usize>,
    /// Content-line the user asked to keep pinned at the top of the viewport by
    /// collapsing a sticky summary. While set, the per-frame scroll clamp is
    /// allowed to scroll past the natural `max_scroll` so a short tail of
    /// content below the collapsed step does not yank the header back down.
    /// Cleared on any manual scroll, view reset, or when auto-follow resumes.
    pub pin_summary_line: Option<usize>,
    /// Latched when a disclosure toggle (expand/collapse of a tool step,
    /// command result, thinking, provider-retry, or notice card) changed the
    /// transcript's height, so the event loop's next frame must be *staged*
    /// — laid out to measure the new `content_lines` — before the toggle's
    /// target scroll offset is applied. The staged pass emits no terminal
    /// bytes, so the terminal only ever sees the final viewport, never an
    /// intermediate one that gets re-clamped a frame later (the source of the
    /// expand/collapse flicker).
    ///
    /// Cleared by the loop once the settled offset has been painted, by any
    /// manual scroll, and by view resets — the same lifecycle as
    /// [`Self::pin_summary_line`].
    pub scroll_settle_pending: bool,
    /// Stack of nested zoom frames (runner tasks). Empty means the root
    /// conversation is shown; the top frame is the currently focused view.
    /// Each frame carries the parent's scroll snapshot, restored on exit.
    pub focus_stack: Vec<ZoomFrame>,
    pub tx: mpsc::UnboundedSender<AgentRequest>,
    pub should_quit: Arc<AtomicBool>,
    pub suggestion_index: Option<usize>,
    /// Latched whenever the user finishes a completion: an `Enter` commit (any
    /// kind), an `Esc` dismiss, **or a slash-command accept via Tab/Enter**
    /// (a terminal accept — see [`Self::accept_completion`]). While `true`,
    /// the completion popup is suppressed even if `completion_kind()` would
    /// otherwise show one — so accepting a command does not immediately flash
    /// a subcommand menu or a collapsed single-exact-match list. Cleared by
    /// the next `InsertChar` / `Backspace` (the user is editing again, so
    /// live completions are once again useful). `@path` accepts via Tab do
    /// **not** latch — Tab is meant to keep cycling path candidates.
    pub completion_dismissed: bool,
    /// Backend-owned slash-command vocabulary published by the daemon.
    pub command_catalog: muta_contracts::CommandCatalog,
    /// Latest race-checked completion rows returned by the daemon.
    pub backend_completions: Vec<muta_contracts::InputCompletion>,
    pub completion_response_input: Option<String>,
    pub completion_response_cursor: usize,
    pub completion_requested: Option<(String, usize)>,
    pub completion_request_id: u64,
    pub cursor_position: usize,
    pub input_scroll: usize,
    /// Screen rect of the composer panel in the last drawn frame (the whole
    /// tinted box, chrome rows included), or `None` while no composer is
    /// shown (overlay modal open, runner view). The spatial mouse router
    /// uses it to route wheel ticks and selection edge-autoscroll to the
    /// input's own viewport instead of the transcript. Zero-height rows
    /// (a collapsed composer) never contain a pointer cell.
    pub input_rect: Option<mutx_engine::Rect>,
    /// Edge-autoscroll direction armed while a mouse selection drag that
    /// started inside the composer leaves the input's text rows: `Some(true)`
    /// scrolls up (pointer above), `Some(false)` down. Stepped by the event
    /// loop's heartbeat tick so holding the pointer still at the edge keeps
    /// scrolling, and cleared when the pointer re-enters or the drag ends.
    pub input_drag_scroll: Option<bool>,
    /// Authoritative foreground surface: the full-screen view plus whatever
    /// panel/transient floats over it (ADR-0141). Callers consume
    /// [`Self::active_modal`] as the rendering projection; panel identity is
    /// always read from [`Self::active_panel`] and view identity from
    /// [`Self::current_view`].
    pub(crate) surfaces: crate::surfaces::SurfaceRouter,
    pub modal_index: usize,
    /// Retained panel states + the MRU order that backs the Ctrl+L quick
    /// switcher (ADR-0139/0141). Browse panels open through
    /// [`Self::open_panel`], which initialises state exactly once per panel
    /// and restores it on every later open — hide/close/switch instead of
    /// the old reset-on-every-open ritual. Full-screen views are not
    /// registered here: their state already persists on `App`.
    pub(crate) panels: crate::surfaces::PanelRegistry,
    /// The quick switcher's live fuzzy query (ADR-0139). The
    /// switcher does not borrow the composer (it must work over surfaces
    /// that have their own input semantics); printable keys append here,
    /// Backspace drops one, and the row set is `switcher_rows` filtered by
    /// `fuzzy_match` against each view's label + hint.
    pub(crate) view_switcher_query: String,
    /// The session whose outbox the Queue view auto-blocked on entry
    /// (ADR-0139). `hide_active_panel` is an `&mut App` method that
    /// cannot see the loop's `viewed_session_id`, so the block site records
    /// the target here and the exit hook consumes it.
    pub(crate) queue_exit_session: Option<String>,
    /// Wall-clock instant of the last user key press or input edit. Used by
    /// the event loop to quiesce background animation redraws while active
    /// composition / typing is in progress.
    pub last_key_press: std::time::Instant,
    /// Body scroll offset shared by the Tools / Mcp / Skills managers
    /// (`Modal::Tools` / `Modal::Mcp` / `Modal::Skills`). Reset to 0 on
    /// open. Clamped (and, when `session_modal_follow` is set, auto-followed to
    /// the selection cursor) by the renderer each frame.
    pub session_scroll: usize,
    /// When true, the Tools/Mcp/Skills body scroll follows the ↑/↓ selection
    /// cursor (the default after open / navigation). Cleared the moment the
    /// user scrolls manually (wheel / page keys) so they can browse freely, and
    /// re-set the moment they navigate again.
    pub session_modal_follow: bool,
    /// Session DAG tree representation for `/tree` visualization.
    pub session_tree: muta_contracts::SessionTree,
    /// Scroll offset for the `/tree` modal body.
    pub tree_scroll: usize,
    /// Auto-follow selection in `/tree` modal.
    pub tree_modal_follow: bool,
    /// `true` while the sessions picker is drilled into the session-info
    /// sub-view (`i`). The detail body renders from [`Self::session_detail`];
    /// Esc backs out to the list (mirrors the TokenReport drill-in).
    pub session_info_detail: bool,
    /// Full detail for the session under the info sub-view cursor. Populated by
    /// an on-demand `QuerySessionDetail` round-trip when the sub-view opens
    /// (`i`) and refreshed whenever the selection moves while in the sub-view.
    /// `None` while the round-trip is in flight.
    pub session_detail: Option<muta_contracts::SessionDetail>,
    /// Body scroll offset of the session-info sub-view. Reset to 0 on open and
    /// when the detail changes; reused (not the list's `session_scroll`).
    pub session_info_scroll: usize,
    /// Body scroll offset of the permissions manager modal. Reset to 0 each
    /// time the modal opens; clamped and auto-followed to the selection by the
    /// renderer each frame.
    pub permissions_scroll: usize,
    /// Body scroll offset of the config category list.
    pub config_scroll: usize,
    /// Which pane of the `/config` Settings View currently owns the keyboard.
    pub config_focus: crate::overlays::ConfigFocus,
    /// Selected category in the `/config` Settings View (0..4).
    pub config_category: usize,
    /// Selected item/field index in the active category's detail pane.
    pub config_detail_index: usize,
    /// Scroll offset for the `/config` detail pane body.
    pub config_detail_scroll: usize,
    /// Whether the custom theme hex editor is actively focused for text entry.
    pub config_custom_editing: bool,
    /// Latest `[websearch]` snapshot (presence-only view) from the harness.
    /// Refreshed when the Settings view opens (`QueryWebSearchConfig`) and
    /// on every `WebSearchConfigUpdated` ack.
    pub websearch_config: Option<muta_contracts::WebSearchConfigView>,
    /// Web-search settings pane: which field index borrows the composer
    /// input row for text entry (SearXNG URL / API keys). `None` = browse.
    pub websearch_editing: Option<usize>,
    /// Index of the skills-modal row whose detail block is expanded
    /// (`Modal::Skills`), or `None` when every row is collapsed. `Enter`
    /// toggles the selected row; reset to `None` each time the modal opens.
    /// The skills modal reuses [`Self::modal_index`] for its selection cursor
    /// and [`Self::session_scroll`] for its body scroll.
    pub skills_expanded: Option<usize>,
    /// Body scroll offset of the history modal (Ctrl+R). Reset to 0 each time
    /// the modal opens (and when toggling browse/search/preview); clamped and
    /// auto-followed to the selection by the renderer each frame.
    pub history_scroll: usize,
    /// When true, the history modal's body scroll follows the ↑/↓ selection
    /// cursor. Cleared on manual scroll (free browse), re-set on navigation.
    pub history_modal_follow: bool,
    /// When true, the history modal shows the full (multi-line) text of the
    /// selected entry instead of the one-line-per-row fuzzy list. Toggled by
    /// Tab; ↑/↓ re-shows the focused entry's complete prompt. `history_scroll`
    /// is reused as the per-entry scroll inside preview mode.
    pub history_preview: bool,
    /// Whether the history modal's **search sub-layer** is active. The modal
    /// opens in browse mode (`false`): a plain reverse-chronological list with
    /// no query field. Pressing `/` enters search (`true`), which borrows the
    /// composer line as a live fuzzy query; the first Esc returns to browse and
    /// the second closes the modal. See [`App::history_rows`].
    pub history_search: bool,
    pub current_provider: String,
    pub current_model: String,
    /// Raw current working directory captured at startup. Used to resolve
    /// `@path` mention completions against the real filesystem.
    pub cwd: std::path::PathBuf,
    /// The id of the session the TUI is currently viewing (`primary_session_id`
    /// outside a side view, the side id inside one). Learned each frame from
    /// the session source and stamped onto every recorded input-history entry
    /// so the inline ↑/↓ recall can walk this session's prompts only, while
    /// Ctrl+R searches the whole cross-session history.
    pub current_session_id: String,
    /// The workspace label for the current session — the project root's
    /// display path (already tilde-shortened). Stamped onto recorded entries
    /// and surfaced by the history panel's selected-row origin line.
    pub current_workspace: String,
    /// Latest session-context snapshot for the Tools / Mcp / Skills /
    /// Permissions managers, or `None` before the first `QuerySessionContext`
    /// round-trip completes. Refreshed each frame from the response listener.
    pub session_context: Option<muta_contracts::SessionContextSnapshot>,
    pub loop_status: LoopStatus,
    /// Whether the primary session has a stopped round parked for `/retry`
    /// (ADR-0128). Mirrored from the session-scoped harness snapshot — the
    /// durable resume point — and consumed by [`Self::viewed_chrome`] to
    /// build the primary's retry affordance. Asides read their own
    /// `SessionChrome::can_retry`.
    pub harness_retry_pending: bool,
    /// Typed activity-bar phase for the primary session (`None` = idle /
    /// bar hidden). Never holds transport setbacks — see `crate::phase`.
    pub phase: Option<crate::phase::Phase>,
    /// Token-stall watch mirrored from the runtime; drives the silent
    /// clause.
    pub pulse: crate::pulse::TokenWatch,
    pub provider_retry: Option<ProviderRetryState>,
    /// Whether all tool permissions are auto-approved this session
    /// (`--delegate` / `/delegate on`). Mirrored from the harness snapshot.
    pub delegated: bool,
    /// Unified task list, mirrored from `AgentResponse::TodosUpdated`. Shown
    /// inside the Activity modal (and no longer pinned above the input box) so
    /// the footer reclaims the vertical space. `None` (or an empty list)
    /// hides it. A plan approved via `plan_exit` seeds this list from its
    /// `##` headings.
    pub todos: Option<TodoList>,
    /// Harness round counter, mirrored each frame. Surfaced inside the
    /// Activity modal as `round N` (the activity bar itself no longer shows
    /// the structural counters — it surfaces status/plan/elapsed and is the
    /// click target that opens the modal).
    pub round_count: u64,
    /// Current turn within the active round (1-indexed for display:
    /// `0` means the round has started but no model request has fired yet —
    /// e.g. the "queued" / "preparing context" phase). Mirrored each frame
    /// from the response listener; shown in the Activity modal as
    /// `round N · turn M · <status>`.
    pub current_turn: u64,
    /// Wall-clock instant the current round started, or `None` between rounds.
    /// Drives the muted `<elapsed>` segment in the activity bar.
    pub round_started_at: Option<std::time::Instant>,
    /// Active tab inside the Activity modal (`Modal::Activity`).
    /// Ignored while any other modal is open.
    pub activity_tab: ActivityTab,
    /// Scroll offset inside `Modal::Activity`. Reset to 0 each time the modal
    /// opens; clamped each frame by the modal's body renderer.
    pub activity_scroll: usize,
    /// Scroll offset inside `Modal::Queue`. Reset to 0 each time the modal
    /// opens; clamped each frame by the modal's body renderer. When
    /// `queue_modal_follow` is set, it is nudged so the ↑/↓ selection stays
    /// on screen.
    pub queue_scroll: usize,
    /// When true, the queue modal's body scroll follows the ↑/↓ selection
    /// cursor (the default after open / navigation). Cleared the moment the
    /// user scrolls manually (wheel / page keys) so they can browse a long
    /// queue freely, and re-set the moment they navigate again. Mirrors
    /// `session_modal_follow` / `question_modal_follow`.
    pub queue_modal_follow: bool,
    /// Scroll offset inside `Modal::Help`. Reset to 0 each time the modal opens;
    /// clamped each frame by the modal's body renderer. The keybinding list
    /// overflows a typical terminal, so this is what keeps the lower sections
    /// reachable — the renderer used to take a throwaway `&mut 0`, leaving the
    /// modal unscrollable.
    pub help_scroll: usize,
    /// Whether the active modal is showing its in-modal keybindings page
    /// (toggled by `?` when the footer has collapsed). Not a nested modal —
    /// the same `active_modal` stays open and the body is swapped for the
    /// full keymap. Cleared on modal close / stage change / Esc.
    pub modal_keymap_open: bool,
    pub pending_permission: Option<PermissionRequest>,
    /// The pending interactive-input request (L3.5 β) from an interactive
    /// `bash` command, or `None`. Set when a `RoundEvent::InputRequest` arrives;
    /// the input-injection modal reads it for its prompt/command/secret.
    pub pending_input: Option<muta_contracts::InputRequest>,
    /// The open question (ask_user) modal's self-contained MVU state, or
    /// `None` when no question modal is open. Replaces the four separate
    /// `question_*` fields that previously scattered the modal's state across
    /// `App`; all interaction now flows through `QuestionModel::update`.
    pub question: Option<crate::question_model::QuestionModel>,
    /// Scroll offset inside `Modal::Question`. Reset to 0 each time a question
    /// modal opens; clamped each frame by the modal's body renderer and, when
    /// `question_modal_follow` is set, nudged so the highlighted option stays on
    /// screen.
    pub question_scroll: usize,
    /// When true, the question modal's body scroll follows the ↑/↓ option
    /// highlight (the default after open / navigation). Cleared the moment the
    /// user scrolls manually (wheel / page keys) so they can browse a long
    /// option list freely, and re-set the moment they navigate again. Mirrors
    /// `session_modal_follow` / `history_modal_follow`.
    pub question_modal_follow: bool,
    /// Rows shown in the sessions picker (`/sessions` or `mutx attach`).
    pub sessions_overview: Vec<SessionOverview>,
    /// Live monitor snapshot for the `/host` daemon control panel
    /// (ADR-0096), mirrored from `UiRuntime::host_sessions` each frame.
    pub host_sessions: Vec<muta_contracts::MonitoredSession>,
    /// Scroll slot + selection-follow for the `/host` panel body.
    pub host_scroll: usize,
    pub host_modal_follow: bool,
    /// Which pane of the `/host` session dashboard owns the keyboard: the
    /// console/input region (default) or the sessions dock (`Tab` toggles).
    pub host_focus: crate::overlays::DashboardFocus,
    /// Scroll offset for the dashboard's console pane.
    pub host_detail_scroll: usize,
    /// The dashboard's session preview modal (ADR-0097 §3): the session id
    /// opened by Enter on a dock selection. Selection alone never opens it;
    /// Esc closes. Read-only.
    pub host_preview: Option<String>,
    /// Scroll offset for the preview modal body.
    pub host_preview_scroll: usize,
    /// Whether the dashboard's inline new-session prompt is open. While true,
    /// the composer input buffer is the task description and Enter creates a
    /// session instead of attaching.
    pub host_prompting: bool,
    /// What the open dashboard prompt does on submit: `true` = create a new
    /// session (from `n`), `false` = prompt the selected session (from `p`).
    pub host_prompt_new: bool,
    /// The dashboard console's receipt transcript (ADR-0097 §3): one entry
    /// per dispatched directive plus the daemon's answer. Lives for the
    /// dashboard's open lifetime (cleared on open) — it is a cockpit log,
    /// not history.
    pub host_console_log: Vec<crate::overlays::ConsoleLine>,
    /// Whether the dashboard's kill confirmation is armed: `k` on a dock
    /// selection asks first (`k` again confirms within the window, anything
    /// else cancels). Killing is irreversible, so it stays a two-surface
    /// gesture like the queue's `Shift+D`.
    pub host_kill_confirm: Option<String>,
    /// Id the armed kill confirmation refers to (kept separately so a dock
    /// selection move between presses can be compared against it).
    pub host_kill_confirm_id: Option<String>,
    /// `/host` Enter on a hosted session: the id to switch to, read by the
    /// caller after the TUI exits to re-attach (ADR-0096).
    pub switch_to_target: Option<String>,
    /// Which full-screen overlay (if any) the TUI opened straight into at
    /// startup instead of a conversation view. In that mode the overlay is not
    /// a transient modal — there is no conversation the user asked for behind
    /// it — so closing it must quit the program rather than drop into an empty
    /// chat. Cleared (set to [`crate::StartupOverlay::None`]) once a
    /// session is opened from the picker. Always `None` for the in-session
    /// `/sessions` modal, which just dismisses on Esc/click-out.
    pub startup_overlay: crate::StartupOverlay,
    pub permission_confirm_always: bool,
    /// Whether the inline permission sheet is expanded to show the full
    /// description + arguments. Collapsed by default so the prompt stays
    /// brief; "Details" toggles this.
    pub permission_show_details: bool,
    pub permission_scroll: usize,
    pub permission_max_scroll: usize,
    pub input_history: Vec<muta_contracts::HistoryEntry>,
    /// **Derived** prompt rows for the viewed session, reconstructed from the
    /// transcript (see [`Self::backfill_session_history`]). Never persisted:
    /// the session file is the durable source of truth for conversation
    /// content (ADR-0018), so these rows exist only so the inline ↑/↓ recall
    /// can walk a resumed conversation's prompts without this client having
    /// recorded them. Indexed by `input_history.len() + i` in
    /// [`Self::current_session_history`] — see [`Self::history_entry`].
    /// Ordered oldest-first (transcript append order) so growth never shifts
    /// existing indices.
    pub session_history_backfill: Vec<muta_contracts::HistoryEntry>,
    /// How many transcript messages [`Self::backfill_session_history`] has
    /// already consumed for the current session, so a long streaming session
    /// rescans only its tail. Reset to `0` on every viewed-session change.
    pub session_history_backfill_cursor: usize,
    /// Whether identical prompt text collapses to one history entry across
    /// sessions (`[input_history] dedup`, default `true`). Read by
    /// [`Self::record_input_history`] and threaded into the persisted merge.
    pub input_history_dedup: bool,
    /// Whether `/slash` command invocations are recorded into the input
    /// history (`[input_history] record_commands`, default `false`).
    pub input_history_record_commands: bool,
    /// Whether `record_input_history` / `clear_input_history` actually touch
    /// the on-disk `history.json`. Production keeps this `true` (set from
    /// `main`'s TUI entry point); tests construct `App` directly and default
    /// it to `false`, so a unit test can never write (or truncate!) the
    /// user's real `$XDG_STATE_HOME/muta/history.json` — a bug that once
    /// polluted it with synthetic `prompt N` rows stamped `session-a`.
    /// In-memory history still behaves identically; only the disk write is
    /// suppressed.
    pub input_history_persist: bool,
    /// When true, the Ctrl+R history modal is awaiting an explicit clear
    /// confirmation (`y` confirms, any other key / Esc cancels). Armed by the
    /// `Ctrl+X` clear shortcut so a stray keystroke can never wipe history.
    pub history_clear_confirm: bool,
    /// The inline ↑/↓ history **pointer**. Together with [`Self::history_draft`]
    /// this forms the input-history pointer model:
    ///
    /// - `None` — the composer shows the **draft** (the live, editable, remembered
    ///   input slot). This is the "newest" position: the input that has **not
    ///   been successfully sent** (still being composed, restored by a Phase-1
    ///   unsend, inserted from Ctrl+R, or recalled from the queue).
    /// - `Some(p)` — the composer shows history row `p` of the current session's
    ///   newest-first slice ([`Self::current_session_history`]), as a **read-only
    ///   snapshot**: edits made on a history row are temporary and are discarded
    ///   when the pointer moves away — coming back to the row reloads the
    ///   original text.
    ///
    /// ↑ moves the pointer toward older rows (and stashes the draft into
    /// [`Self::history_draft`] on the first press); ↓ moves it back toward the
    /// newest row and, past it, back to `None` (restoring the draft). A
    /// successful send clears the draft, because the input has been historicised
    /// and is no longer "unsent".
    pub history_index: Option<usize>,
    /// The live, editable, remembered input slot — the content of the **draft**
    /// mode (when [`Self::history_index`] is `None`). It is stashed here when ↑
    /// leaves the draft for a history row, and restored when ↓ walks back past
    /// the newest row, so an accidental ↑/↓ never loses what the user was
    /// composing. It is **cleared on send** (the input has been historicised)
    /// and replaced whenever a new input is adopted as the draft (Phase-1
    /// unsend restore, Ctrl+R insert, queue recall). Distinct from
    /// `stashed_input`, which is borrowed by modal flows.
    pub history_draft: String,
    /// Attachments staged behind [`Self::history_draft`] (the images and
    /// large pastes that were in the composer when the first ↑ stashed it),
    /// so ↓ past the newest entry restores them together with the text.
    pub history_draft_images: Vec<ImagePart>,
    pub history_draft_text_pastes: Vec<String>,
    /// In-memory attachment cache for recorded history entries, keyed by
    /// `(text, session_id)` — see [`HistoryAttachments`]. Seeded by
    /// [`Self::record_input_history`], consumed by the ↑/↓ and Ctrl+R recall
    /// paths so re-sending an interrupted or completed message restores its
    /// images and large pastes instead of shipping a bare chip label.
    pub history_attachments: HashMap<(String, Option<String>), HistoryAttachments>,
    /// FIFO insertion order of [`Self::history_attachments`] keys, so the
    /// cache can be pruned oldest-first when it outgrows its cap.
    pub history_attachments_order: VecDeque<(String, Option<String>)>,
    /// The inline ↑/↓ **queue pointer** — the id of the outbox item
    /// ([`Self::pending_dispatch`]) the composer is currently editing. Forms
    /// a pointer model over the queue that mirrors the history pointer:
    ///
    /// - `None` — the composer is the **draft** (or a history row): queue
    ///   navigation is not active.
    /// - `Some(id)` — the composer shows the queue item's content as an
    ///   **editable projection**: ↑/↓ move the pointer across the queue
    ///   (newest → oldest and back) without removing anything, and Enter
    ///   writes the edited content back **into that item in place** (the
    ///   queue's length and order are untouched).
    ///
    /// Held as an id (not an index) so dispatch/reorder/delete cannot
    /// invalidate it silently; a vanished target makes Enter fall back to an
    /// ordinary send (see [`Self::queue_pointer_target`]).
    ///
    /// The pointer walks the queue *before* history: ↑ from the draft enters
    /// the queue first (the outbox is the newer, more urgent surface), and
    /// only an exhausted queue hands ↑ on to input history.
    pub queue_pointer: Option<String>,
    /// The draft stashed aside when the queue pointer is armed — the exact
    /// counterpart of [`Self::history_draft`] for queue navigation, restored
    /// when ↓ walks back past the newest queue item.
    pub queue_pointer_draft: String,
    pub queue_pointer_draft_images: Vec<ImagePart>,
    pub queue_pointer_draft_text_pastes: Vec<String>,
    /// Images pasted (Ctrl+V) and waiting to be sent with the next message.
    /// Each entry is paired 1-to-1 with an `[Image #N]` chip inside
    /// [`App::input`]; the chip's `#N` is `index + 1` after
    /// [`App::reconcile_attachments`] has run.
    pub pending_images: Vec<ImagePart>,
    /// Large pasted text blocks staged behind `[Pasted text #N +M lines]`
    /// chips inside [`App::input`]. Each entry is the full original paste;
    /// the matching chip in the input is just a short label so the input
    /// box stays compact. Order matches the chip numbering.
    pub pending_text_pastes: Vec<String>,
    /// Session-affine compact outbox — the **next-round queue**. Pending items
    /// are never appended to the transcript; the queue bar shows counts and
    /// ↑/↓ walk a non-destructive pointer over the items (see
    /// [`Self::queue_pointer`]). Every staged message waits for the running
    /// round to finish naturally before starting a new one (next-round only).
    pub pending_dispatch: VecDeque<QueuedDispatch>,
    /// Target queue mode for the live composer while a round is running.
    pub composer_send_mode: ComposerSendMode,
    /// Sessions whose outbox is hard-blocked by the user. While a session is
    /// blocked, no queued message auto-drains — not even after its round
    /// reaches natural completion and the harness goes idle. The queue modal
    /// blocks a session on open (so items can be managed safely) and resumes
    /// on close; `Ctrl+P` toggles the block from the bar without opening
    /// the modal.
    /// Independent of the transient "paused" coloring: a session can be idle
    /// (visibly paused) without being blocked, and vice versa.
    pub queue_blocked_sessions: std::collections::HashSet<String>,
    /// Sessions whose last interactive round reached its natural completion
    /// event and whose harness has subsequently reported idle. Both facts are
    /// tracked separately so errors/interrupts never auto-run follow-ups.
    pub naturally_completed_sessions: std::collections::HashSet<String>,
    pub idle_sessions: std::collections::HashSet<String>,
    pub running_sessions: std::collections::HashSet<String>,
    /// Semantic selection state.
    pub selection: SelectionState,
    /// Drag gesture state.
    pub drag: SelectionDrag,
    /// Layout map for the current frame (updated each draw).
    pub layout_map: LayoutMap,
    /// Modal-local click targets for the current frame.
    pub modal_hit_map: ModalHitMap,
    /// Message index of the step (tool step or reasoning trace) whose header
    /// currently rests under the mouse pointer (inline or sticky pinned), so
    /// the next draw lights it up to the intermediate hover tone as a click
    /// affordance. `None` whenever the pointer is elsewhere or an overlay
    /// modal is open.
    pub hovered_step: Option<usize>,
    /// Which layout strategy arranges the transcript message stream. Selected
    /// via `[tui] transcript_layout`; defaults to the turn-banded layout (each
    /// tool-bearing ReAct turn grouped under a labelled header). See
    /// `crate::view::layout::Strategy`.
    pub transcript_layout: crate::view::layout::Strategy,
    /// Canonical active color-scheme id (`zen`, a built-in preset, or
    /// `custom`). The renderer theme is rebuilt from this value immediately
    /// when the Appearance page applies a choice.
    pub color_scheme: String,
    /// Last persisted custom semantic palette. Retained while a preset is
    /// active so switching schemes never discards the user's colors.
    pub custom_color_scheme: muta_contracts::ColorSchemeConfig,
    /// Transactional working copy used by the custom palette editor. Esc
    /// discards it; Enter promotes it to `custom_color_scheme` and persists it.
    pub custom_color_draft: muta_contracts::ColorSchemeConfig,
    /// Whether clicking outside a dismissable modal closes it (mirroring Esc).
    /// From `[tui] click_outside_dismiss` (default `true`): when true, an
    /// outside click dismisses a dismissable modal like Esc (the draft is
    /// parked, so nothing is lost). Modals holding precious in-progress input
    /// are never click-dismissable regardless of this flag, and the `muta
    /// resume` startup picker's click-outside still quits. Esc / Ctrl+C always
    /// close/quit regardless of this flag.
    pub click_outside_dismiss: bool,
    /// Whether a disclosure toggle (expand/collapse) auto-scrolls to keep the
    /// toggled card well-placed. From `[tui] expand_auto_scroll` (default
    /// `false`): when false, the toggle changes only the card's height and the
    /// scroll offset is left exactly where the user put it; when true, the
    /// expand path shifts the summary toward the viewport top and the collapse
    /// path keeps a scrolled-past summary visible. Enabled toggles settle
    /// through the staged measure-then-paint path (`scroll_settle_pending`),
    /// so the auto-scroll itself never flickers.
    pub expand_auto_scroll: bool,
    /// Keyboard-focused activatable target in the current frame, and the TUI's
    /// only navigation state — there is no separate "browse mode". `None` means
    /// every key has its ordinary input-box meaning (typing flows into the
    /// prompt). `Some` means a transcript step is highlighted: `Ctrl+↑`/`Ctrl+↓`
    /// (or bare `↑`/`↓`) cycle it, `Enter` activates it, and `Esc` clears it.
    /// Mouse hover/click is an acceleration path onto the same state.
    pub focused_target: Option<InteractiveTarget>,
    /// Show a brief "copied" toast. Held until this deadline elapses so the
    /// duration is wall-clock consistent regardless of the event-loop cadence.
    pub copy_toast_until: Option<std::time::Instant>,
    pub copy_toast_message: String,
    pub copy_toast_failed: bool,
    /// A transient notice toast (command acknowledgments such as
    /// `/delegate on`, surfaced via `NoticeSurface::Toast`). Unlike the
    /// inline `MessageKind::Notice`, this never enters the transcript: it
    /// renders as a top-right bubble that fades on its own, mirroring the copy
    /// toast. Severity drives the bubble's accent color. Held until
    /// `notice_toast_until` elapses so the duration is wall-clock consistent
    /// regardless of the loop cadence. A newer toast replaces an in-flight one.
    pub notice_toast_until: Option<std::time::Instant>,
    pub notice_toast_message: String,
    pub notice_toast_severity: NoticeSeverity,
    /// Deadline until which a second Ctrl+C quits. Wall-clock based (like
    /// the copy/notice toasts) so the quit window is a real duration —
    /// previously this was a per-tick counter, which stretched the intended
    /// ~2s window to ~20s whenever the loop idled at its 1s heartbeat.
    pub ctrl_c_armed_until: Option<std::time::Instant>,
    /// Deadline until which a second Esc interrupts the running task.
    /// Wall-clock based for the same reason as `ctrl_c_armed_until`: the
    /// loop wakes far more often than its 100ms animation heartbeat (every
    /// keystroke, mouse move, stream delta, and dirty-notify), so the old
    /// 20-tick counter burned the intended ~2s window in a few hundred
    /// milliseconds — the "Esc again interrupts" toast flashed and vanished
    /// before a second press could land.
    pub esc_armed_until: Option<std::time::Instant>,
    /// Epoch the breathing indicator is timed against. The spinner phase is
    /// derived from wall-clock elapsed time since this instant rather than a
    /// per-frame counter, so the breathing cadence stays constant regardless of
    /// how often the loop redraws (mouse movement, streaming, paste, etc. all
    /// wake the loop at irregular intervals and would otherwise jitter it).
    pub spinner_epoch: std::time::Instant,
    /// Epoch the empty-state help carousel is timed against (ADR-0104). The
    /// slide index is derived from wall-clock elapsed time since this instant
    /// (same pattern as [`Self::spinner_epoch`]) so the rotation cadence stays
    /// constant regardless of draw frequency.
    pub carousel_epoch: std::time::Instant,
    /// Epoch the effort-ignition celebration is timed against. Set when the
    /// model's top reasoning tier (`max`) is selected; `None` once the
    /// animation completes. Wall-clock based (like `spinner_epoch`) so the
    /// wave cadence survives the loop's irregular wakeups. Drives the
    /// composer's background wave tint, the hint bar's `M A X` label
    /// takeover, and the prompt's charge — see `crate::effort_ignition`.
    pub effort_ignition_epoch: Option<std::time::Instant>,
    /// The composer draft parked while the input-injection sheet
    /// (L3.5 β) borrows the input line. Under ADR-0139 the
    /// picker flows (Models / Connections / History) park their drafts in
    /// per-view slots on the `PanelRegistry`; this remaining global slot
    /// serves the one request-driven borrowed-line surface, whose
    /// lifecycle (queue-front arrival → reply) never coexists with a
    /// picker's.
    pub injection_stashed_input: String,
    /// Provider id targeted by the unified key editor (`Modal::ModelEditor`).
    pub editor_target: Option<String>,
    /// Which editor field is focused. `0` = API key (text entry); `1` = effort
    /// (←/→ cycling); `2` = thinking (Space toggle, when available). The
    /// effort/thinking rows are only shown for models that expose those controls,
    /// so `editor_field` is clamped to `0` otherwise.
    pub editor_field: u8,
    /// API-key buffer for the editor (the input line is borrowed for the
    /// focused field).
    pub editor_key: String,
    /// Wire model id the key editor will activate once a key is entered (carried
    /// from the Models-picker selection or the provider's current model; not
    /// user-editable).
    pub editor_model: String,
    /// When true, `Modal::ModelEditor` edits the selected provider model's
    /// channel settings only (for example OpenAI effort or Anthropic
    /// effort/thinking), not the provider API key or active provider.
    pub editor_model_settings_only: bool,
    /// When `editor_model_settings_only` is true, whether the edited model is
    /// **built-in** (served by a built-in provider like `anthropic`). A built-in
    /// model's per-model reasoning knobs persist to the `[model_reasoning]`
    /// table via `EditModelReasoning`; a user-defined model's knobs persist to
    /// its channel via `EditProviderModel` (ADR-0045).
    pub editor_target_is_builtin: bool,
    /// Current reasoning-effort selection in the key editor, as a lowercase wire
    /// string. Defaults to `"high"`; cycled with ←/→ over the selected model's
    /// supported levels.
    pub editor_effort: String,
    /// Whether the selected model exposes a separate thinking on/off switch.
    /// OpenAI GPT effort has no separate thinking field, so this is false
    /// there; Anthropic adaptive channels set it true.
    pub editor_thinking_available: bool,
    /// Current extended-thinking on/off selection in the key editor. Defaults
    /// to `true` (adaptive thinking on — the recommended mode for Claude).
    /// Toggled with Space when [`Self::editor_thinking_available`] is true;
    /// orthogonal to effort.
    pub editor_thinking: bool,
    /// Capability-override tri-state for **vision** (ADR-0149 layer 1), shown
    /// in the per-model settings editor. Cycled with Space: `None` = inherit
    /// (no override) → `Some(true)` force on → `Some(false)` force off.
    pub editor_vision_override: Option<bool>,
    /// Capability-override tri-state for **tool calling**, same cycling and
    /// semantics as [`Self::editor_vision_override`].
    pub editor_tool_override: Option<bool>,
    /// Focused field of the provider editor (`Modal::CustomProvider`) as an
    /// index into [`Self::custom_fields`] — the per-preset visible field set
    /// (Name / Base URL / Token / Model). The focused field always borrows the
    /// composer line; the Model field borrows it as a live filter query.
    pub custom_field: u8,
    /// The ordered visible fields of the provider editor, chosen by the active
    /// preset (create) or the edited connection's protocol (edit). Empty when no
    /// editor is open.
    pub custom_fields: Vec<CustomField>,
    /// Wire protocol of the provider being created/edited (`"openai"` |
    /// `"anthropic"` | `"google"`), carried from the preset or the edited
    /// provider rather than chosen with a protocol picker.
    pub custom_protocol_wire: String,
    /// Models seeded by the active preset (create mode). Submitted as the
    /// provider's model list unless the editor exposes a free-text Model field
    /// (then the single typed model is submitted instead). Empty in edit mode.
    pub custom_models: Vec<String>,
    /// Base URL placeholder for the active preset (the expected endpoint shape).
    pub custom_url_hint: String,
    /// Template-specific user agent carried into newly-created channels.
    pub custom_user_agent: Option<String>,
    /// How newly-created connections authenticate (from the selected preset).
    pub custom_auth: muta_contracts::ChannelAuth,
    /// Stable preset id the active create flow was seeded from, or `None` in
    /// edit mode / when no preset is active. Sent as `AddProvider::preset_id`
    /// (the wire field) so the catalog can re-seed the connection from the
    /// preset's current
    /// models on later startups. `None` yields a pure-custom instance that is
    /// never re-seeded.
    pub custom_preset_id: Option<String>,
    /// True while an "Add preset connection → OAuth" flow is in flight.
    pub awaiting_oauth_add: bool,
    pub oauth_pending_message: String,
    pub oauth_pending_url: String,
    pub oauth_pending_user_code: String,
    pub oauth_pending_error: Option<String>,
    /// Selected copyable card in OAuth Pending modal (0 = URL, 1 = Code).
    pub oauth_selected_item: usize,
    /// Scroll offset for the OAuth pending modal body. Reset when the modal
    /// opens or its content changes.
    pub oauth_scroll: usize,
    /// Highlight index into the live suggestion list for the provider editor's
    /// Model **filter** field (type to filter, `↑/↓` to move, committed live).
    pub custom_suggest_index: usize,
    /// Scroll offset for the custom-provider editor body. Rendered body sets
    /// the upper bound automatically.
    pub custom_scroll: usize,
    /// When `Some(id)`, the provider editor is **editing** the existing user
    /// provider `id` (meta only: Name/Base URL/Token; models stay managed in the
    /// Models picker). `None` is create mode.
    pub custom_edit_id: Option<String>,
    /// Provider-editor buffers holding the unfocused text fields (the focused one
    /// lives in the borrowed composer line). Name / Base URL / Token / Model /
    /// Effort.
    pub custom_name: String,
    pub custom_base_url: String,
    pub custom_token: String,
    pub custom_model: String,
    /// Selected row of the provider-template chooser (`Modal::ProviderPreset`),
    /// indexing `crate::PROVIDER_PRESETS`. Cycled with `↑/↓`.
    pub preset_choice: usize,
    /// Scroll offset for the preset-chooser body. The rendered body
    /// sets the upper bound automatically (via `render_body`), and `↑/↓` move
    /// the selection so the chosen preset stays on-screen.
    pub preset_scroll: usize,
    /// Whether the model picker's **search sub-layer** is active. Both pickers
    /// (`Modal::Models` and `Modal::Connections`) open in browse mode
    /// (`false`): a plain ranked list with no query field. Pressing `/` enters
    /// search (`true`), which borrows the composer line as a live fuzzy query;
    /// the first Esc returns to browse and the second closes the modal. Mirrors
    /// [`Self::history_search`]. See [`Self::models_flat_filtered`].
    pub model_search: bool,
    /// Body scroll offset of the model picker. Reset to 0 each time the modal
    /// opens (and when toggling browse/search); clamped and auto-followed to the
    /// selection by the renderer each frame. Mirrors [`Self::history_scroll`].
    pub model_scroll: usize,
    /// When true, the model picker's body scroll follows the ↑/↓ selection
    /// cursor. Cleared on manual scroll (free browse), re-set on navigation.
    pub model_modal_follow: bool,
    /// Pending provider-delete confirmation overlay. `Some(id)` means the
    /// confirm dialog is open over the Connections list: the provider
    /// `id` is staged for deletion and waits on the user's choice. Set when
    /// `Shift+D` lands on a deletable custom provider; cleared on Cancel, Esc,
    /// outside-click, and after a confirmed Delete dispatches the request.
    pub pending_provider_delete: Option<String>,
    /// Focused button in the provider-delete confirm overlay. Defaults to
    /// [`ProviderDeleteChoice::Cancel`] (the safe choice) each time the overlay
    /// opens; ←/→/Tab move between the two buttons.
    pub provider_delete_focus: ProviderDeleteChoice,
    /// Lowercase provider name → whether a usable API key is configured.
    pub key_status: HashMap<String, bool>,
    /// Live model-picker snapshot (default id + per-model favorite / key-ready
    /// / last-used). Drives the `/models` and `/connections` pickers' rendering
    /// and sort order. Refreshed from the response listener each frame.
    pub provider_picker: ProviderPickerSnapshot,
    /// Theme.
    pub theme: Theme,
    /// User-supplied ASCII logo lines loaded at startup from
    /// `$XDG_CONFIG_HOME/muta/logo.txt` (clamped to the empty-state bounding
    /// box). `None` when no user logo is present → built-in wordmark is used.
    /// Passed into the empty-state hero via `TranscriptView::logo`.
    pub logo: Option<Vec<String>>,
}

mod composer;
mod history;
mod providers;
mod queue;
mod runners;
mod surfaces;
