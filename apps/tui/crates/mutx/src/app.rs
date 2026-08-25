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
    CustomField, ProviderTemplate, RankedModel, RankedProvider, edit_fields,
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
    /// A fresh round is being started for this item (`ChatToSession` sent,
    /// `NextRoundStarted` not yet received).
    Dispatching,
}

/// A user message owned by the compact outbox (the **next-round** queue).
///
/// Two kinds of content live here: a busy Enter (staged while a round runs)
/// and a mid-round insert (`Ctrl+O`) whose round ended before admission
/// (handed back by `UserInputUnavailable`) — both wait to become the **next**
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
/// envoy task, or a read-only / decision modal is open). In the `None` case
/// the cursor is hidden so the IME has no stale anchor to bind to, which is
/// the bug that previously let the IME "drift" when a disclosure was
/// clicked mid-composition: the caret left the composer but the cursor
/// stayed visible at its old coordinate.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum CaretOwner {
    /// The live composer (no modal, no envoy zoom, no transcript-step focus).
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

/// One frame on the focus stack: the envoy task call-id plus the parent
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
                "running · {}",
                format_retry_duration(now.saturating_duration_since(self.retry_at))
            )
        };
        format!("retry {retry}/{max_retries} · {timing}")
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

/// The view-scoped chrome of one session: the activity-bar text, the
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
    /// Activity-bar text ("" when idle). Per session: a background aside's
    /// "running" status lives in its own entry, invisible to the main view.
    pub activity: String,
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
    /// streaming, envoy view, or chrome hidden).
    pub activity_rect: Option<mutx_engine::Rect>,
    /// Screen rect of the context-meter segment in the hint bar (the
    /// `89.2k (8%)` indicator), so a click on it opens the TokenReport modal.
    /// `None` when the hint bar or context meter is not shown.
    pub hint_context_rect: Option<mutx_engine::Rect>,
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
    /// bar is hidden (chrome hidden or envoy zoom).
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
    /// Stack of nested zoom frames (envoy tasks). Empty means the root
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
    /// Authoritative foreground surface and transient return stack. Callers
    /// consume [`Self::active_modal`] as the rendering projection; view
    /// identity is always read from [`Self::active_view`].
    pub(crate) surfaces: crate::views::SurfaceRouter,
    pub modal_index: usize,
    /// Retained view states + the MRU order that backs the Ctrl+L quick
    /// switcher (ADR-0139). Browse surfaces open through
    /// [`Self::open_view`], which initialises state exactly once per view
    /// and restores it on every later open — hide/close/switch instead of
    /// the old reset-on-every-open ritual.
    pub(crate) views: crate::views::ViewRegistry,
    /// The quick switcher's live fuzzy query (ADR-0139). The
    /// switcher does not borrow the composer (it must work over surfaces
    /// that have their own input semantics); printable keys append here,
    /// Backspace drops one, and the row set is `switcher_rows` filtered by
    /// `fuzzy_match` against each view's label + hint.
    pub(crate) view_switcher_query: String,
    /// The session whose outbox the Queue view auto-blocked on entry
    /// (ADR-0139). `hide_active_view` is an `&mut App` method that
    /// cannot see the loop's `viewed_session_id`, so the block site records
    /// the target here and the exit hook consumes it.
    pub(crate) queue_exit_session: Option<String>,
    /// Last-known screen rect of the composer. Refreshed every draw and reused
    /// between frames by the input-driven immediate cursor flush so the IME
    /// composition window is re-anchored in the *same* iteration a keystroke is
    /// handled — before the next frame is even rendered (the fix for the
    /// one-frame cursor lag that mis-anchored IME). It is only an approximation
    /// of the rect the *next* frame will compute (the footer height can change
    /// when wrapping shifts), but a follow-up full draw always lands when that
    /// happens, so the approximation is correct exactly when it matters (the
    /// non-wrap-moving keystrokes that dominate real typing).
    pub last_input_rect: mutx_engine::Rect,
    /// Full-frame area the last render pass measured the composer against.
    /// Lets [`Self::input_geometry_is_clean`] detect a resize between the
    /// observed rect and the current frame (a reflow makes every cached rect
    /// stale by definition). Recorded in the same draw as `last_input_rect`.
    pub(crate) last_frame_area: mutx_engine::Rect,
    /// Wrapped text-row count the last render reserved for the composer at
    /// `last_input_rect.width`. The immediate cursor flush re-derives the
    /// count and skips itself when it differs — a wrap boundary crossing
    /// moves the box (and the caret with it), so the flush must not write a
    /// coordinate the next frame corrects.
    pub(crate) last_input_rows: usize,
    /// Wall-clock instant of the last user key press or input edit. Used by
    /// the event loop to quiesce background animation redraws while active
    /// composition / typing is in progress.
    pub last_key_press: std::time::Instant,
    /// Whether the terminal cursor should be moved to match `cursor_position`
    /// before the next frame, eliminating the one-frame IME lag. Set by
    /// [`App::set_cursor`] (the single write site for `cursor_position`) and
    /// cleared by the event loop's immediate-flush after it syncs the backend.
    pub cursor_sync_pending: bool,
    /// The cursor visibility we last told the terminal. The event loop's
    /// hide/show state machine consults this so show/hide is a state
    /// transition (escape codes emitted only on an edge) driven by
    /// [`App::caret_visible`], not a per-frame guess.
    pub cursor_visible: bool,
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
    pub activity_status: String,
    pub provider_retry: Option<ProviderRetryState>,
    /// Whether write-tool permission prompts are bypassed this session
    /// (`--autopilot` / `/autopilot on`). Mirrored from the harness
    /// snapshot; surfaced by the state bar's flat `autopilot` label (warning
    /// tone + bold) directly below the input so the elevated state is
    /// unmissable.
    pub autopilot: bool,
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
    /// `/autopilot on`, surfaced via `NoticeSurface::Toast`). Unlike the
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
    /// per-view slots on the `ViewRegistry`; this remaining global slot
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
    /// Focused field of the provider editor (`Modal::CustomProvider`) as an
    /// index into [`Self::custom_fields`] — the per-template visible field set
    /// (Name / Base URL / Token / Model). The focused field always borrows the
    /// composer line; the Model field borrows it as a live filter query.
    pub custom_field: u8,
    /// The ordered visible fields of the provider editor, chosen by the active
    /// template (create) or the edited provider's protocol (edit). Empty when no
    /// editor is open.
    pub custom_fields: Vec<CustomField>,
    /// Wire protocol of the provider being created/edited (`"openai"` |
    /// `"anthropic"` | `"google"`), carried from the template or the edited
    /// provider rather than chosen with a protocol picker.
    pub custom_protocol_wire: String,
    /// Models seeded by the active template (create mode). Submitted as the
    /// provider's model list unless the editor exposes a free-text Model field
    /// (then the single typed model is submitted instead). Empty in edit mode.
    pub custom_models: Vec<String>,
    /// Base URL placeholder for the active template (the expected endpoint shape).
    pub custom_url_hint: String,
    /// Template-specific user agent carried into newly-created channels.
    pub custom_user_agent: Option<String>,
    /// How newly-created channels authenticate (from the selected template).
    pub custom_auth: muta_contracts::ChannelAuth,
    /// Stable template id the active create flow was seeded from, or `None` in
    /// edit mode / when no template is active. Sent as `AddProvider::template_id`
    /// so the catalog can re-seed the instance from the template's current
    /// models on later startups. `None` yields a pure-custom instance that is
    /// never re-seeded.
    pub custom_template_id: Option<String>,
    /// True while "+ Add provider → xAI OAuth" browser flow is in flight.
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
    /// Selected row of the provider-template chooser (`Modal::ProviderTemplate`),
    /// indexing `crate::PROVIDER_TEMPLATES`. Cycled with `↑/↓`.
    pub template_choice: usize,
    /// Scroll offset for the provider-template chooser body. The rendered body
    /// sets the upper bound automatically (via `render_body`), and `↑/↓` move
    /// the selection so the chosen template stays on-screen.
    pub template_scroll: usize,
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

impl App {
    /// Rendering/input projection of the authoritative foreground surface.
    pub(crate) fn active_modal(&self) -> Modal {
        self.surfaces.modal()
    }

    /// Exact identity of the focused retained view. This deliberately cannot
    /// be reconstructed from [`Self::active_modal`] because Activity and
    /// Todos share the same modal presentation.
    pub(crate) fn active_view(&self) -> Option<crate::views::ViewId> {
        self.surfaces.active_view()
    }

    /// Replace the foreground with chat and discard unreachable return
    /// frames. View exits should normally use [`Self::hide_active_view`] so
    /// their lifecycle hook runs first.
    pub(crate) fn show_chat_surface(&mut self) {
        self.surfaces.show_chat();
    }

    pub(crate) fn replace_transient_surface(&mut self, modal: Modal) {
        if modal == Modal::None {
            self.show_chat_surface();
        } else {
            self.surfaces.replace_transient(modal);
        }
    }

    /// Push a transient over the current surface, preserving the exact
    /// parent identity and its retained cursor/scroll before the child
    /// borrows shared presentation fields.
    pub(crate) fn push_transient_surface(&mut self, modal: Modal) {
        if let Some(id) = self.active_view() {
            self.save_view_state(id);
        }
        self.surfaces.push_transient(modal);
    }

    /// Pop one transient and restore the parent view's live projection.
    pub(crate) fn pop_transient_surface(&mut self) -> Modal {
        let restored = self.surfaces.pop_transient();
        if let Some(id) = restored.view() {
            self.restore_view_state(id);
        }
        restored.modal()
    }

    pub(crate) fn transient_return_modal(&self) -> Modal {
        self.surfaces
            .return_surface()
            .map_or(Modal::None, crate::views::Surface::modal)
    }

    pub(crate) fn transient_return_view(&self) -> Option<crate::views::ViewId> {
        self.surfaces
            .return_surface()
            .and_then(crate::views::Surface::view)
    }

    pub(crate) fn can_open_view_switcher(&self) -> bool {
        self.can_accept_navigation_signal() && !self.modal_keymap_open
    }

    /// Whether asynchronous presentation intent may replace the foreground.
    /// Data snapshots are always safe to apply, but navigation waits while a
    /// transient transaction or a parent-owned drill-in has control.
    pub(crate) fn can_accept_navigation_signal(&self) -> bool {
        use crate::views::Surface;
        let root_surface = matches!(self.surfaces.active(), Surface::Chat | Surface::View(_));
        let active_view = self.active_view();
        root_surface
            && !(active_view == Some(crate::views::ViewId::Host)
                && (self.host_prompting || self.host_preview.is_some()))
            && !(active_view == Some(crate::views::ViewId::Sessions)
                && self.session_info_detail)
            && !(active_view == Some(crate::views::ViewId::TokenReport)
                && self.token_report_detail)
            && !(active_view == Some(crate::views::ViewId::Config)
                && (self.config_custom_editing
                    || self.websearch_editing.is_some()
                    || self.config_focus == crate::overlays::ConfigFocus::Detail))
    }

    #[cfg(test)]
    pub(crate) fn set_active_modal_for_test(&mut self, modal: Modal) {
        use crate::views::ViewId;
        let view = match modal {
            Modal::Help => Some(ViewId::Help),
            Modal::Activity => Some(if self.activity_tab == ActivityTab::Todos {
                ViewId::Todos
            } else {
                ViewId::Activity
            }),
            Modal::Tools => Some(ViewId::Tools),
            Modal::Mcp => Some(ViewId::Mcp),
            Modal::Skills => Some(ViewId::Skills),
            Modal::Permissions => Some(ViewId::Permissions),
            Modal::UsageStats => Some(ViewId::UsageStats),
            Modal::TokenReport => Some(ViewId::TokenReport),
            Modal::Btw => Some(ViewId::Btw),
            Modal::Config => Some(ViewId::Config),
            Modal::Models => Some(ViewId::Models),
            Modal::Connections => Some(ViewId::Connections),
            Modal::HistorySearch => Some(ViewId::HistorySearch),
            Modal::Queue => Some(ViewId::Queue),
            Modal::Host => Some(ViewId::Host),
            Modal::Sessions => Some(ViewId::Sessions),
            Modal::Tree => Some(ViewId::Tree),
            _ => None,
        };
        if let Some(id) = view {
            self.views.open(id);
            self.surfaces.show_view(id);
        } else if modal == Modal::None {
            self.surfaces.show_chat();
        } else {
            self.surfaces.show_transient(modal);
        }
    }

    /// Record an input-history entry with the on-disk cap mirrored in memory:
    /// `HISTORY_CAP` bounds the persisted union, so an unbounded in-memory
    /// `Vec` would grow past it over a long-lived TUI (each entry is small,
    /// but a multi-day session with heavy prompt reuse is unbounded anyway).
    /// Evicts from the oldest end.
    fn push_history(&mut self, entry: muta_contracts::HistoryEntry) {
        self.input_history.push(entry);
        if self.input_history.len() > muta_contracts::history::HISTORY_CAP {
            let overflow = self.input_history.len() - muta_contracts::history::HISTORY_CAP;
            self.input_history.drain(..overflow);
        }
    }

    /// The token-source report for one session, from whichever source this
    /// frontend has: the shared in-process ledger (standalone path) or the
    /// on-demand harness snapshot (attach path). `None` in attach mode while
    /// the `QueryTokenUsage` round-trip is still in flight.
    pub fn token_source_report(
        &self,
        session_id: &str,
    ) -> Option<muta_contracts::TokenSourceReport> {
        if let Some(ledger) = &self.token_ledger {
            Some(ledger.snapshot_for_session(session_id))
        } else {
            self.token_report.clone()
        }
    }

    /// Whether the Ctrl+C quit window is currently armed (a second Ctrl+C
    /// before the deadline quits). Wall-clock based; an elapsed deadline
    /// reads as disarmed.
    pub fn ctrl_c_armed(&self) -> bool {
        self.ctrl_c_armed_until
            .is_some_and(|until| std::time::Instant::now() < until)
    }

    /// Arm the Ctrl+C quit window until the given deadline, or disarm it
    /// entirely when called with `None`.
    pub fn arm_ctrl_c(&mut self, until: Option<std::time::Instant>) {
        self.ctrl_c_armed_until = until;
    }

    /// How long the first Esc's confirmation window stays open. Matches the
    /// Ctrl+C quit window so both double-press confirmations feel the same.
    pub const ESC_ARM_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

    /// Whether the Esc interrupt window is currently armed (a second Esc
    /// before the deadline interrupts the viewed session's running round).
    /// Wall-clock based; an elapsed deadline reads as disarmed.
    pub fn esc_armed(&self) -> bool {
        self.esc_armed_until
            .is_some_and(|until| std::time::Instant::now() < until)
    }

    /// Arm the Esc interrupt window until the given deadline, or disarm it
    /// entirely when called with `None`.
    pub fn arm_esc(&mut self, until: Option<std::time::Instant>) {
        self.esc_armed_until = until;
    }

    /// Register one Esc press in the interrupt-confirmation flow: the first
    /// press arms the window (returns `false`), a second press inside it
    /// fires (returns `true` and disarms), and a press after the window has
    /// lapsed starts a fresh window instead of firing a stale confirmation.
    pub fn esc_press(&mut self) -> bool {
        if self.esc_armed() {
            self.esc_armed_until = None;
            true
        } else {
            self.esc_armed_until = Some(std::time::Instant::now() + Self::ESC_ARM_WINDOW);
            false
        }
    }

    /// Per-frame bookkeeping for the Esc interrupt window: lapse it once
    /// the wall-clock deadline passes, or immediately when the *viewed*
    /// session no longer has a running round — there is nothing left to
    /// interrupt, so keeping the toast up would mislead. Scoped to the
    /// viewed session (the same `running_sessions` predicate the keymap
    /// uses to map Esc to an interrupt), never the runtime's global
    /// primary-only `is_responding` flag: an aside view armed from its own
    /// running round must survive the primary being idle.
    pub fn tick_esc_arm(&mut self) {
        if let Some(until) = self.esc_armed_until
            && std::time::Instant::now() >= until
        {
            self.esc_armed_until = None;
        }
        if self.esc_armed()
            && !self
                .running_sessions
                .contains(self.current_session_id.as_str())
        {
            self.esc_armed_until = None;
        }
    }

    pub fn byte_cursor(&self) -> usize {
        self.input
            .char_indices()
            .map(|(i, _)| i)
            .nth(self.cursor_position)
            .unwrap_or(self.input.len())
    }

    /// Set the input caret position and mark the terminal cursor as needing an
    /// immediate re-sync before the next frame.
    ///
    /// This is the **single sanctioned write site** for `cursor_position`.
    /// Routing every caret move through it guarantees the event loop's
    /// immediate-flush (which re-anchors the IME composition window in the same
    /// iteration as the keystroke) always fires — a raw `app.cursor_position =
    /// …` would silently skip the flush and re-introduce the one-frame lag.
    pub fn set_cursor(&mut self, pos: usize) {
        self.cursor_position = pos;
        self.cursor_sync_pending = true;
    }

    /// Set the input caret to the end of `self.input` (common case after a
    /// programmatic input replacement: history navigation, modal restore,
    /// paste). Equivalent to `set_cursor(self.input.chars().count())` but
    /// reads as intent at the call site.
    pub fn set_cursor_end(&mut self) {
        let end = self.input.chars().count();
        self.set_cursor(end);
    }

    /// Whether the active selection covers a piece of the composer's text —
    /// the precondition for the caret-relay and delete-selection behaviours.
    /// Supports whole-input selections (`Block` on `INPUT_MSG_IDX`) and
    /// drag-selected ranges (`Range` on `INPUT_MSG_IDX`).
    pub fn has_input_selection(&self) -> bool {
        if !self.selection.is_active() {
            return false;
        }
        match &self.selection {
            SelectionState::Block { message_idx, .. } => *message_idx == crate::view::INPUT_MSG_IDX,
            SelectionState::TableCell { message_idx, .. } => {
                *message_idx == crate::view::INPUT_MSG_IDX
            }
            SelectionState::Range { anchor, head } => {
                anchor.message_idx == crate::view::INPUT_MSG_IDX
                    && head.message_idx == crate::view::INPUT_MSG_IDX
            }
            SelectionState::None => false,
        }
    }

    /// Adopt the caret to the given edge of the input selection and drop
    /// the selection, restoring the (previously hidden) caret at that edge.
    /// `Head` is the release point where the mouse drag finished, while `Tail`
    /// is the anchor point where the drag began.
    ///
    /// No-op (returns `false`) unless [`Self::has_input_selection`].
    pub fn adopt_caret_from_input_selection(&mut self, edge: SelectionEdge) -> bool {
        if !self.has_input_selection() {
            return false;
        }
        let pos = match &self.selection {
            SelectionState::Block { .. } => match edge {
                SelectionEdge::Tail => 0,
                SelectionEdge::Head => self.input.chars().count(),
            },
            SelectionState::Range { anchor, head } => {
                let cursor = match edge {
                    SelectionEdge::Tail => *anchor,
                    SelectionEdge::Head => *head,
                };
                let byte = crate::model::selection::floor_grapheme_boundary(
                    &self.input,
                    cursor.byte_offset,
                )
                .min(self.input.len());
                self.input[..byte].chars().count()
            }
            _ => match edge {
                SelectionEdge::Tail => 0,
                SelectionEdge::Head => self.cursor_position,
            },
        };
        self.selection = SelectionState::None;
        self.drag.cancel();
        self.set_cursor(pos.min(self.input.chars().count()));
        true
    }

    /// Whether the next direction-key press should relay from the hidden
    /// caret position instead of acting on the *visible* (stale) caret:
    /// `true` while a whole-input selection is active on the composer and
    /// the composer owns the caret. Callers run this check *after* the
    /// direction key has been mapped through `process_event` but before its
    /// cursor mutation takes effect for the user — see the event loop's key
    /// relay for the exact sequencing.
    pub fn input_selection_relays_arrows(&self) -> bool {
        self.has_input_selection() && self.caret_owner() == CaretOwner::Composer
    }

    /// Delete the composer text the active input selection covers (the standard
    /// editor behaviour: Backspace/Del over a selection replaces it).
    /// No-op (returns `false`) unless [`Self::has_input_selection`].
    pub fn delete_input_selection(&mut self) -> bool {
        if !self.has_input_selection() {
            return false;
        }
        match &self.selection {
            SelectionState::Block { message_idx, .. }
                if *message_idx == crate::view::INPUT_MSG_IDX =>
            {
                self.input.clear();
                self.selection = SelectionState::None;
                self.drag.cancel();
                self.set_cursor(0);
                true
            }
            SelectionState::Range { .. } => {
                if let Some((start, end)) = self.selection.active_normalized_range() {
                    let start_byte = crate::model::selection::floor_grapheme_boundary(
                        &self.input,
                        start.byte_offset,
                    )
                    .min(self.input.len());
                    let end_byte = crate::model::selection::inclusive_grapheme_end(
                        &self.input,
                        end.byte_offset,
                    )
                    .min(self.input.len());
                    if start_byte < end_byte {
                        self.input.replace_range(start_byte..end_byte, "");
                    }
                    let new_cursor = self.input[..start_byte].chars().count();
                    self.selection = SelectionState::None;
                    self.drag.cancel();
                    self.set_cursor(new_cursor);
                    true
                } else {
                    self.selection = SelectionState::None;
                    self.drag.cancel();
                    false
                }
            }
            _ => {
                self.selection = SelectionState::None;
                self.drag.cancel();
                false
            }
        }
    }

    /// Record the composer's screen rect as observed during the latest draw, so
    /// the input-driven immediate cursor flush can place the caret without
    /// waiting for the next frame.
    pub fn observe_input_rect(
        &mut self,
        rect: mutx_engine::Rect,
        frame_area: mutx_engine::Rect,
        input_rows: usize,
    ) {
        self.last_input_rect = rect;
        self.last_frame_area = frame_area;
        // The renderer-measured row count — the same value that sized
        // `rect.height` — not a re-derivation. Re-deriving here would create
        // a second source of truth that can drift from the masking rules
        // (the ModelEditor key field renders `•`s while `self.input` holds
        // the raw key) and from any future change to the wrap width formula.
        self.last_input_rows = input_rows;
    }

    /// Record that the caret moved without going through [`App::set_cursor`]
    /// (the only legitimate caller is the input handler, which mutates
    /// `cursor_position` in place for performance and then reports the new
    /// value). Marks the immediate flush pending.
    pub fn note_cursor_moved(&mut self) {
        self.cursor_sync_pending = true;
    }

    /// Whether the composer's on-screen geometry can still be trusted to
    /// match [`Self::last_input_rect`] — i.e. whether the input-driven
    /// immediate cursor flush may place the caret against it *without* the
    /// very next `commit_frame` re-measuring a different rect and moving the
    /// caret a second time. That flush→draw two-step is what users perceived
    /// as the caret "drifting"/bouncing while typing (most visibly during a
    /// streaming round, where the footer geometry moves underneath the
    /// composer).
    ///
    /// Rather than duplicating every input of the footer-stack layout, the
    /// divergence is detected empirically and conservatively:
    ///
    /// * the terminal size the last frame rendered at must still be current —
    ///   a resize reflows every wrap, so the observed rect is stale by
    ///   definition until the next committed frame;
    /// * re-measuring the composer's wrapped-row count at the same width the
    ///   last frame used must yield the same count — a wrap boundary crossed,
    ///   a newline added or removed, a paste, or a history recall all change
    ///   the box height, and with it every row above it.
    ///
    /// The remaining geometry movers (activity/todo/queue bar toggles,
    /// page-hints row, recess, envoy zoom) are always driven by a listener
    /// update or a caret-ownership change: the loop suppresses the flush for
    /// the former (see `sync_caret_and_cursor`) and `caret_owner`/visibility
    /// already gate the latter, so no layout knowledge is duplicated here.
    #[cfg(test)]
    pub(crate) fn input_geometry_is_clean(&self, terminal_size: (u16, u16)) -> bool {
        if self.last_input_rect.width == 0 || self.last_input_rows == 0 {
            return false;
        }
        if (self.last_frame_area.width, self.last_frame_area.height) != terminal_size {
            return false;
        }
        // Measure the *displayed* text — the same string the renderer laid
        // out to produce `last_input_rows` — never the raw buffer. For the
        // ModelEditor's masked key field the displayed text is the `•` mask,
        // and only the masked pair (text + caret byte offset) reproduces the
        // renderer's measurement.
        let text_width =
            crate::view::composer_layout_text_width(self.last_frame_area.width as usize);
        self.displayed_input_with_cursor()
            .map(|(text, byte_cursor)| {
                crate::composer::input_row_count(&text, text_width, byte_cursor)
                    == self.last_input_rows
            })
            .unwrap_or(false)
    }

    /// The text the composer will *display* this state, paired with the
    /// caret's byte offset into that displayed text. This is the exact pair
    /// [`crate::event_loop::render`] hands the transcript layout, extracted
    /// so the geometry probe measures the same string the renderer measured
    /// (masking included) instead of re-deriving from the raw buffer.
    #[cfg(test)]
    pub(crate) fn displayed_input_with_cursor(&self) -> Option<(String, usize)> {
        if self.active_modal() == Modal::ModelEditor && self.editor_field == 0 {
            const MASK_CHAR: &str = "•";
            let mask = MASK_CHAR.repeat(self.input.chars().count());
            let caret_byte = MASK_CHAR.len() * self.cursor_position.min(mask.chars().count());
            Some((mask, caret_byte))
        } else {
            Some((self.input.clone(), self.byte_cursor()))
        }
    }

    /// The single source of truth for which surface owns the terminal cursor
    /// this frame. See [`CaretOwner`].
    ///
    /// This is a pure function of (`active_modal`, `focused_target`,
    /// `focus_stack`) — never of the selection, which is folded in separately
    /// by [`Self::caret_visible`] because a selection hides the cursor
    /// regardless of who owns it. Keeping ownership and selection-appearance
    /// decoupled is what lets the event loop distinguish "reposition the
    /// composer's caret" (owner = `Composer`, no selection) from "hide it"
    /// (owner = `Composer` but a selection is active) without re-deriving
    /// either from raw fields.
    pub fn caret_owner(&self) -> CaretOwner {
        if self.active_modal() != Modal::None {
            // The provider-delete confirm overlay is a keyboard-only sub-layer
            // (no text input): suppress the caret while it is open so the host
            // IME does not anchor to the provider-search input behind the
            // panel. Re-arms naturally when the overlay closes and ownership
            // returns to the picker.
            if self.pending_provider_delete.is_some() {
                return CaretOwner::None;
            }
            // The history panel floats above a fully-live composer: the
            // composer IS its filter input, so the composer (not a modal
            // field) owns the caret while this surface is open. This is why
            // `HistorySearch` is deliberately absent from `Modal::owns_caret`.
            if self.active_modal() == Modal::HistorySearch {
                return if self.in_envoy_view() {
                    CaretOwner::None
                } else {
                    CaretOwner::Composer
                };
            }
            return if self.active_modal().owns_caret() {
                CaretOwner::Modal
            } else if self.active_modal() == Modal::Question
                && self
                    .question
                    .as_ref()
                    .is_some_and(|q| q.is_other_highlighted())
            {
                // The Question modal is normally a decision sheet (no caret).
                // But when the synthetic "Other" free-text row is highlighted
                // it becomes a real text-input surface, so it must own the
                // terminal cursor for that one state — otherwise the host IME
                // has no coordinate to anchor its composition window to. This
                // is the only state-dependent ownership; every other modal's
                // ownership is static via `Modal::owns_caret`.
                CaretOwner::Modal
            } else {
                CaretOwner::None
            };
        }
        // No modal: the composer owns the caret unless a transcript step has
        // keyboard focus or we are zoomed into an envoy task (which has no
        // input line at all — its footer collapses to zero height).
        if self.focused_target.is_some() || self.in_envoy_view() {
            CaretOwner::None
        } else {
            CaretOwner::Composer
        }
    }

    /// Whether the terminal cursor should be visible right now —
    /// [`Self::caret_owner`] plus the one extra rule that an active text
    /// selection hides the cursor (a block cursor would clash with the
    /// selection background). This is what every cursor site consults; no
    /// call site should re-derive visibility from raw fields.
    pub fn caret_visible(&self) -> bool {
        !self.selection.is_active() && self.caret_owner() != CaretOwner::None
    }

    /// The modal body's scroll offset and (optional) follow-flag that a
    /// `Scroll*` action should mutate, keyed off [`App::active_modal`].
    ///
    /// This is the single source of truth that the `ScrollUp` / `ScrollDown` /
    /// `ScrollPageUp` / `ScrollPageDown` / `ScrollTop` / `ScrollBottom` actions
    /// consult: every scrollable modal resolves to `Some((&mut scroll,
    /// follow_flag))`, so a key press advances the right field without a
    /// per-modal `if/else` chain duplicated across six action arms.
    ///
    /// The follow flag (`Some` only for list-style modals that auto-follow the
    /// ↑/↓ selection) is cleared on any manual scroll so the user can browse a
    /// long list freely until they navigate again — mirroring the established
    /// per-modal behaviour. Returns `None` for modals that don't scroll their
    /// own body (the inline permission sheet drives `permission_scroll` via a
    /// separate action, and the caret-owning text editors have no body scroll).
    pub(crate) fn modal_scroll_field(&mut self) -> Option<(&mut usize, Option<&mut bool>)> {
        let modal = self.active_modal();
        match modal {
            Modal::Help => Some((&mut self.help_scroll, None)),
            Modal::Activity => Some((&mut self.activity_scroll, None)),
            Modal::Permissions => Some((&mut self.permissions_scroll, None)),
            Modal::Config => match self.config_focus {
                crate::overlays::ConfigFocus::Categories => Some((&mut self.config_scroll, None)),
                crate::overlays::ConfigFocus::Detail => {
                    Some((&mut self.config_detail_scroll, None))
                }
            },
            Modal::TokenReport => Some((&mut self.token_report_scroll, None)),
            Modal::UsageStats => Some((&mut self.usage_stats_scroll, None)),
            Modal::OauthPending => Some((&mut self.oauth_scroll, None)),
            Modal::ProviderTemplate => Some((&mut self.template_scroll, None)),
            Modal::CustomProvider => Some((&mut self.custom_scroll, None)),
            // List-style modals: clear the follow flag so manual scroll wins.
            Modal::Tools | Modal::Mcp | Modal::Skills | Modal::Sessions => Some((
                &mut self.session_scroll,
                Some(&mut self.session_modal_follow),
            )),
            // The dashboard routes body-scroll to the deepest open layer:
            // the session preview when present, else the focused pane (dock
            // selection-scroll or the console read-out scroll).
            Modal::Host => {
                if self.host_preview.is_some() {
                    Some((&mut self.host_preview_scroll, None))
                } else {
                    match self.host_focus {
                        crate::overlays::DashboardFocus::List => {
                            Some((&mut self.host_scroll, Some(&mut self.host_modal_follow)))
                        }
                        crate::overlays::DashboardFocus::Detail => {
                            Some((&mut self.host_detail_scroll, None))
                        }
                    }
                }
            }
            Modal::Queue => Some((&mut self.queue_scroll, Some(&mut self.queue_modal_follow))),
            Modal::Btw => Some((&mut self.btw_scroll, Some(&mut self.btw_modal_follow))),
            Modal::HistorySearch => Some((
                &mut self.history_scroll,
                Some(&mut self.history_modal_follow),
            )),
            Modal::Connections | Modal::Models => {
                Some((&mut self.model_scroll, Some(&mut self.model_modal_follow)))
            }
            Modal::Question => Some((
                &mut self.question_scroll,
                Some(&mut self.question_modal_follow),
            )),
            Modal::Tree => Some((&mut self.tree_scroll, Some(&mut self.tree_modal_follow))),
            // Permission drives its own body via PermissionDetailsUp/Down (and
            // the transcript behind it scrolls when no step is focused); the
            // caret-owning text editors have no body scroll. None => the
            // Scroll* action falls through to the transcript fallback.
            Modal::None | Modal::Permission | Modal::ModelEditor | Modal::InputInjection => None,
            // The quick switcher scrolls its own list through the shared
            // session slot, like the other compact list modals.
            Modal::ViewSwitcher => Some((
                &mut self.session_scroll,
                Some(&mut self.session_modal_follow),
            )),
        }
    }

    /// Reconcile [`App::pending_images`] / [`App::pending_text_pastes`]
    /// against the chips that currently survive in [`App::input`], and
    /// relabel the surviving chips so their `#N` matches their new 1-based
    /// position in the truncated vectors. Cheap to run on every input
    /// mutation: it is a single linear scan over the input string.
    ///
    /// This is the prune + relabel pass that drops orphaned staged entries
    /// whenever the user deletes or edits a chip — by backspace, selection
    /// delete, or hand-typing over the chip text. Mirrors codex's
    /// `reconcile_deleted_elements` and claude-code's `parseReferences`
    /// effect, adapted to muta's "chip text lives in the input" model.
    pub fn reconcile_attachments(&mut self) {
        let new_input = composer_attachments::reconcile(
            &self.input,
            &mut self.pending_images,
            &mut self.pending_text_pastes,
        );
        self.input = new_input;
    }

    /// How many staged messages are waiting in this session's outbox (front
    /// pops first). All entries are next-round items; a busy Enter always
    /// queues rather than injecting mid-round.
    pub fn pending_count(&self, session_id: &str) -> usize {
        self.pending_dispatch
            .iter()
            .filter(|item| item.session_id == session_id)
            .count()
    }

    pub fn remove_dispatch(&mut self, session_id: &str, input_id: &str) -> Option<QueuedDispatch> {
        let position = self
            .pending_dispatch
            .iter()
            .position(|item| item.session_id == session_id && item.id == input_id)?;
        self.pending_dispatch.remove(position)
    }

    /// Is this session's outbox hard-blocked by the user? While blocked, no
    /// queued message auto-drains — not even after natural completion + idle.
    /// The queue modal blocks on open and resumes on close; `Ctrl+P` toggles
    /// from
    /// the bar. A no-op (and leaves the block off) for a session with no
    /// staged items.
    pub fn is_queue_blocked(&self, session_id: &str) -> bool {
        self.queue_blocked_sessions.contains(session_id)
    }

    /// Toggle the user block on the viewed session's outbox. Mirrors `Ctrl+P` /
    /// the queue modal's block control. Returns the new state so the caller
    /// can reflect it in the render snapshot.
    pub fn toggle_queue_block(&mut self, session_id: &str) -> bool {
        if !self.queue_blocked_sessions.insert(session_id.to_string()) {
            // Already present → remove it (toggle off).
            self.queue_blocked_sessions.remove(session_id);
            false
        } else {
            true
        }
    }

    /// Force the block on, regardless of its current state. Used when the
    /// queue modal opens so items can be managed safely (delete / reorder /
    /// re-edit) without one auto-draining mid-edit.
    pub fn block_queue(&mut self, session_id: &str) {
        self.queue_blocked_sessions.insert(session_id.to_string());
    }

    /// Force the block off. Used when the queue modal closes (auto-resume), so
    /// the outbox returns to its normal auto-drain behavior the moment the
    /// user stops managing it — unless they explicitly blocked it with
    /// `Ctrl+P` outside the modal (that toggle is honored because the modal
    /// close path only resumes what its own open path blocked).
    pub fn resume_queue(&mut self, session_id: &str) {
        self.queue_blocked_sessions.remove(session_id);
    }

    /// Remove the viewed session's outbox item at display index `idx`. Used by
    /// the queue modal's `D` delete. Returns the removed dispatch (mostly for
    /// tests).
    pub fn remove_queued_at(&mut self, session_id: &str, idx: usize) -> Option<QueuedDispatch> {
        let position = self
            .pending_dispatch
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                item.session_id == session_id && item.state == QueuedDispatchState::Waiting
            })
            .nth(idx)
            .map(|(pos, _)| pos)?;
        self.pending_dispatch.remove(position)
    }

    /// Move the viewed session's outbox item at display index `idx` by `delta`
    /// slots within the session's Waiting slice (`delta < 0` toward the front
    /// / next to pop, `delta > 0` toward the tail). Other items in the slice
    /// shift to make room (a true reorder, not a swap). Clamped at the slice
    /// boundaries so an item can never escape into another session's region
    /// of the deque.
    pub fn move_queued(&mut self, session_id: &str, idx: usize, delta: i32) {
        // Collect the positions (into the global deque) of this session's
        // Waiting items in display order — the selectable range.
        let positions: Vec<usize> = self
            .pending_dispatch
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                item.session_id == session_id && item.state == QueuedDispatchState::Waiting
            })
            .map(|(pos, _)| pos)
            .collect();
        let count = positions.len();
        if count == 0 {
            return;
        }
        let clamped_idx = idx.min(count - 1);
        let new_idx = (clamped_idx as i32 + delta).clamp(0, count as i32 - 1) as usize;
        if new_idx == clamped_idx {
            return;
        }
        let from = positions[clamped_idx];
        let target = positions[new_idx];
        // Remove the item, then re-insert at `target`. This lands the item at
        // the destination slot while the displaced neighbors shift to fill the
        // gap — a true reorder, not a swap. The single `target` works for both
        // directions: when moving toward the tail, removal of `from` (before
        // `target`) shifts `target` down by one, exactly offset by inserting
        // one past the neighbor; when moving toward the front, no shift occurs
        // and the item lands just before the neighbor. `from` is a valid index
        // by construction (enumerated from the deque above), so the remove is
        // guarded rather than `expect`-ed.
        if let Some(item) = self.pending_dispatch.remove(from) {
            self.pending_dispatch.insert(target, item);
        }
    }

    /// A staged next-round item failed to start its round (e.g. no provider
    /// configured), or a mid-round insert's round ended before admission.
    ///
    /// For an item still in the outbox this just flips it back to `Waiting`.
    /// For a **transcript-owned insert** (`Ctrl+O` handed back by
    /// `UserInputUnavailable`) there is no outbox item — the content lives in
    /// the transcript entry — so the caller stages one here (`text` /
    /// attachments from the held entry) under the same id: the queue then
    /// owns its auto-dispatch / pointer-recall lifecycle, and the entry is
    /// dropped from the outbox when its round starts (`NextRoundStarted`),
    /// exactly like a busy-Enter item. Pushes to the back (FIFO among
    /// handed-back inserts; they left the running round in send order).
    pub fn requeue_dispatch(
        &mut self,
        session_id: &str,
        input_id: &str,
        held: Option<(String, Vec<ImagePart>, Vec<String>)>,
    ) {
        if let Some(item) = self
            .pending_dispatch
            .iter_mut()
            .find(|item| item.session_id == session_id && item.id == input_id)
        {
            item.state = QueuedDispatchState::Waiting;
            return;
        }
        if let Some((text, images, text_pastes)) = held {
            self.pending_dispatch.push_back(QueuedDispatch {
                id: input_id.to_string(),
                session_id: session_id.to_string(),
                state: QueuedDispatchState::Waiting,
                text,
                queued_at_ms: crate::event_loop::now_epoch_ms(),
                images,
                text_pastes,
            });
        }
    }

    /// FIFO next-round dispatch within one session. The entry remains in the
    /// outbox until its fresh round has actually started; route failure can
    /// therefore return it to `Waiting` without reconstructing user content.
    pub fn begin_next_round_dispatch(&mut self, session_id: &str) -> Option<QueuedDispatch> {
        let item = self.pending_dispatch.iter_mut().find(|item| {
            item.session_id == session_id && item.state == QueuedDispatchState::Waiting
        })?;
        item.state = QueuedDispatchState::Dispatching;
        Some(item.clone())
    }

    /// LIFO undo for the viewed session. Every queued dispatch is a
    /// next-round item, so recall pops the newest staged message and restores
    /// it into the composer immediately — no agent roundtrip to cancel.
    pub fn recall_queued(&mut self, session_id: &str) -> Option<RecallQueued> {
        let position = self.pending_dispatch.iter().rposition(|item| {
            item.session_id == session_id && item.state == QueuedDispatchState::Waiting
        })?;
        self.pending_dispatch
            .remove(position)
            .map(RecallQueued::Restored)
    }

    /// Recall a specific outbox item by display index (front-of-queue = 0).
    /// Used by the queue modal's `Enter` re-edit, which keys off the `↑/↓`
    /// selection rather than always targeting the newest — so a mid-queue item
    /// can be pulled back to the composer too. The item is removed from the
    /// outbox, exactly like [`Self::recall_queued`].
    pub fn recall_queued_at(&mut self, session_id: &str, idx: usize) -> Option<RecallQueued> {
        let position = self
            .pending_dispatch
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                item.session_id == session_id && item.state == QueuedDispatchState::Waiting
            })
            .nth(idx)
            .map(|(pos, _)| pos)?;
        self.pending_dispatch
            .remove(position)
            .map(RecallQueued::Restored)
    }

    /// Recall an outbox item back into the composer. A user gesture (queue
    /// recall / modal re-edit), so it replaces whatever the composer holds.
    pub fn restore_dispatch(&mut self, dispatch: QueuedDispatch) {
        self.adopt_as_draft(
            dispatch.text,
            dispatch.images,
            dispatch.text_pastes,
            DraftAdoption::Replace,
        );
    }

    // ── Queue pointer navigation (↑/↓ over the outbox) ──────────────────────
    //
    // The pointer is the queue's edit surface: ↑/↓ walk it without removing
    // anything (the outbox is a list, not a stack), and Enter commits the
    // composer's content back into the pointed-at item — in place, so the
    // queue's length and order survive the edit. This replaces the older
    // destructive `recall_queued` gesture at the top level (the modal's
    // explicit pull-to-composer re-edit keeps that behavior, where removing
    // the item is the point).

    /// The ids of this session's waiting (next-round) items, front-of-queue
    /// first. `Dispatching` items are excluded: their round has already
    /// started, so editing them would be a lie.
    fn queue_pointer_ids(&self, session_id: &str) -> Vec<String> {
        self.pending_dispatch
            .iter()
            .filter(|item| {
                item.session_id == session_id && item.state == QueuedDispatchState::Waiting
            })
            .map(|item| item.id.clone())
            .collect()
    }

    /// Resolve [`Self::queue_pointer`] to the live item it points at, if the
    /// target still exists and still belongs to this session. A vanished
    /// target (dispatched, deleted, recalled elsewhere) is `None` — callers
    /// treat that as "the pointer is empty".
    pub fn queue_pointer_target(&self, session_id: &str) -> Option<&QueuedDispatch> {
        let id = self.queue_pointer.as_deref()?;
        self.pending_dispatch
            .iter()
            .find(|item| item.session_id == session_id && item.id == id)
    }

    /// Load a queue item's content into the composer as the pointer's
    /// projection (text + attachments, cursor at the end, completion latch
    /// held). Shared by the arm and the step so every landing is identical.
    fn load_queue_pointer_row(&mut self, dispatch: &QueuedDispatch) {
        self.input = dispatch.text.clone();
        self.pending_images = dispatch.images.clone();
        self.pending_text_pastes = dispatch.text_pastes.clone();
        self.set_cursor_end();
        self.suggestion_index = None;
        self.completion_dismissed = true;
    }

    /// Stash the live draft into the pointer's draft slots (the counterpart
    /// of the history pointer's stash), so walking back out restores exactly
    /// what the user was composing.
    fn stash_queue_pointer_draft(&mut self) {
        self.queue_pointer_draft = std::mem::take(&mut self.input);
        self.queue_pointer_draft_images = std::mem::take(&mut self.pending_images);
        self.queue_pointer_draft_text_pastes = std::mem::take(&mut self.pending_text_pastes);
    }

    /// `↑` from the draft (or a history row): arm the queue pointer at the
    /// **newest** waiting item (the back of the deque) and project it into the
    /// composer. Returns `false` when the session's queue has no waiting
    /// items — the caller then hands ↑ on to input history.
    pub fn queue_pointer_prev(&mut self, session_id: &str) -> bool {
        let ids = self.queue_pointer_ids(session_id);
        let Some(newest) = ids.last() else {
            return false;
        };
        if self.queue_pointer.is_none() {
            // Leaving the draft (or a history row): stash what the composer
            // held so the exit path can restore it, and leave history mode —
            // the pointer owns the composer now.
            self.history_index = None;
            self.stash_queue_pointer_draft();
        }
        // Already armed → step toward the front (older). `pos == 0` is the
        // oldest item: stay there (clamped) rather than jumping back to the
        // newest. A vanished target (not found in `ids`) resets to the
        // newest, the sensible default when the world changed under us.
        let next_id = match self
            .queue_pointer
            .as_deref()
            .and_then(|cur| ids.iter().position(|id| id == cur))
        {
            Some(pos) if pos > 0 => ids[pos - 1].clone(),
            Some(_) => self.queue_pointer.clone().unwrap_or_else(|| newest.clone()),
            None => newest.clone(),
        };
        self.queue_pointer = Some(next_id);
        if let Some(dispatch) = self.queue_pointer_target(session_id).cloned() {
            self.load_queue_pointer_row(&dispatch);
        }
        true
    }

    /// `↓` while the pointer is armed: step toward the **newer** items and,
    /// past the newest, dissolve the pointer and restore the stashed draft.
    /// Returns `true` whenever the key was consumed by the pointer (stepping
    /// *or* dissolving); `false` only when the pointer was not armed, so the
    /// caller falls through to history navigation.
    pub fn queue_pointer_next(&mut self, session_id: &str) -> bool {
        let Some(cur) = self.queue_pointer.clone() else {
            return false;
        };
        let ids = self.queue_pointer_ids(session_id);
        let pos = ids.iter().position(|id| id == &cur);
        match pos {
            Some(p) if p + 1 < ids.len() => {
                self.queue_pointer = Some(ids[p + 1].clone());
                if let Some(dispatch) = self.queue_pointer_target(session_id).cloned() {
                    self.load_queue_pointer_row(&dispatch);
                }
                true
            }
            // Past the newest item (or the target vanished): back to the
            // draft, exactly as the history pointer restores its stash.
            _ => self.dissolve_queue_pointer(),
        }
    }

    /// Dissolve the pointer and restore the stashed draft. Also the teardown
    /// path for sends and session switches, so a stale pointer never leaks
    /// into the next composer state. Returns `true` so callers can treat the
    /// key as consumed.
    pub fn dissolve_queue_pointer(&mut self) -> bool {
        if self.queue_pointer.is_none() {
            return false;
        }
        self.queue_pointer = None;
        self.input = std::mem::take(&mut self.queue_pointer_draft);
        self.pending_images = std::mem::take(&mut self.queue_pointer_draft_images);
        self.pending_text_pastes = std::mem::take(&mut self.queue_pointer_draft_text_pastes);
        self.set_cursor_end();
        self.suggestion_index = None;
        self.completion_dismissed = true;
        true
    }

    /// Drop the pointer and its stash **without** restoring the stash into
    /// the composer. Used when the composer's content is leaving the
    /// projection for somewhere permanent (an insert entry, a send): the
    /// content in hand supersedes whatever the stash held, and restoring it
    /// would clobber what the user is actively acting on. Idempotent.
    pub fn drop_queue_pointer_without_restore(&mut self) {
        self.queue_pointer = None;
        self.queue_pointer_draft.clear();
        self.queue_pointer_draft_images.clear();
        self.queue_pointer_draft_text_pastes.clear();
    }

    /// Commit the composer's current content into the pointed-at queue item,
    /// **in place** — the queue's length and order are untouched; only the
    /// item's content changes — and dissolve the pointer (the projection has
    /// been written back; the content now lives in the item). Returns:
    ///
    /// - `Some(())` — the item was updated and the pointer dissolved;
    /// - `None` — the pointer was not armed, or its target vanished while
    ///   the user was editing (it shipped, was deleted, or was recalled). In
    ///   the vanished case the pointer is dissolved **without** restoring
    ///   the stashed draft, so the user's edited content stays in the
    ///   composer and the caller sends it as a fresh message — the
    ///   experience must not dead-end on a race.
    pub fn commit_queue_pointer(&mut self, session_id: &str) -> Option<()> {
        let id = self.queue_pointer.clone()?;
        let text = self.input.clone();
        let images = self.pending_images.clone();
        let text_pastes = self.pending_text_pastes.clone();
        let target = self.pending_dispatch.iter_mut().find(|item| {
            item.session_id == session_id
                && item.id == id
                && item.state == QueuedDispatchState::Waiting
        });
        // Either way the pointer is spent; drop its stashed draft too (the
        // projection either landed in the item, or the composer is about to
        // ship as a fresh message — the stash is obsolete in both).
        self.queue_pointer = None;
        self.queue_pointer_draft.clear();
        self.queue_pointer_draft_images.clear();
        self.queue_pointer_draft_text_pastes.clear();
        match target {
            Some(item) => {
                item.text = text;
                item.images = images;
                item.text_pastes = text_pastes;
                Some(())
            }
            None => None,
        }
    }

    /// Stage a composed message as an in-flight mid-round steer (`Ctrl+O`).
    ///
    /// The insert is **transcript-owned** (ADR-0126): it becomes a
    /// `DeliveryStatus::Queued` entry the moment it is sent and never enters
    /// the outbox — so this helper only mints the correlation id the loop
    /// uses to settle that entry when the harness admits it
    /// (`UserInputInserted`) or hands it back (`UserInputUnavailable` →
    /// [`Self::requeue_dispatch`]).
    pub fn new_insert_id(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }

    /// Adopt `text` (plus its staged attachments) as the new **draft** — the
    /// live, editable, remembered input slot — entering draft mode
    /// (`history_index = None`). This is the single entry point for every
    /// path that places input into the composer as "the newest unsent input":
    /// the Phase-1 unsend restore, the Ctrl+R history insert, and the queue
    /// recall. With [`DraftAdoption::Replace`], whatever the draft held
    /// before is replaced (that content was either sent or superseded), so ↓
    /// past the newest history row later restores *this* input, never a
    /// stale one.
    ///
    /// [`DraftAdoption::OnlyIfIdle`] guards the one path that is not a user
    /// gesture: the unsend restore arrives asynchronously and must not eat a
    /// half-typed draft the user was composing while the round ran.
    ///
    /// The staged attachments are stored both in `pending_*` (what ships on
    /// send) and mirrored into the `history_draft_*` slots (what ↓ restores).
    pub fn adopt_as_draft(
        &mut self,
        text: String,
        images: Vec<ImagePart>,
        text_pastes: Vec<String>,
        policy: DraftAdoption,
    ) {
        if policy == DraftAdoption::OnlyIfIdle
            && (!self.input.is_empty()
                || !self.pending_images.is_empty()
                || !self.pending_text_pastes.is_empty())
        {
            return;
        }
        self.history_index = None;
        self.input = text;
        self.set_cursor_end();
        if !images.is_empty() {
            self.pending_images = images;
        }
        if !text_pastes.is_empty() {
            self.pending_text_pastes = text_pastes;
        }
        self.history_draft = self.input.clone();
        self.history_draft_images = self.pending_images.clone();
        self.history_draft_text_pastes = self.pending_text_pastes.clone();
        // Programmatic input replacement: latch the completion dismissal so
        // the popup doesn't flash until the next real edit.
        self.suggestion_index = None;
        self.completion_dismissed = true;
    }

    /// Clear the remembered draft (text + attachments). Called when the
    /// draft's content is successfully sent: the input has been historicised
    /// (`record_input_history` already recorded it), so it is no longer the
    /// "unsent" slot and must not come back on a later ↓. A Phase-1 unsend
    /// re-adopts it via [`Self::adopt_as_draft`] with
    /// [`DraftAdoption::OnlyIfIdle`].
    pub fn clear_history_draft(&mut self) {
        self.history_draft.clear();
        self.history_draft_images.clear();
        self.history_draft_text_pastes.clear();
    }

    /// Reset every piece of composer navigation state that is **scoped to the
    /// viewed session** — the ↑/↓ history cursor and the per-session draft
    /// stash — when the viewed session changes (`/new`, `/session open`,
    /// `/resume`, `/fork`, entering/leaving a `/btw` aside).
    ///
    /// These slots belong to *a conversation's* composer, not the terminal:
    /// carrying a cursor over a session boundary would make the first `↑` in
    /// the new session land on a position clamped against the *old* session's
    /// row count, and a restored draft would leak what the user was typing
    /// into the previous conversation. The composer itself is emptied the
    /// same way the send path empties it, so the new session starts from a
    /// clean slate.
    pub fn on_viewed_session_changed(&mut self) {
        self.history_index = None;
        self.clear_history_draft();
        // Retained view state (ADR-0139) belongs to the conversation being
        // left — a scroll position into Tools/Skills rows or a report page
        // is context about *that* session's data. Forgetting it here is the
        // `close` verb applied wholesale.
        if let Some(sid) = self.queue_exit_session.take() {
            self.resume_queue(&sid);
        }
        self.surfaces.show_chat();
        self.views.close_all();
        for id in crate::views::ViewId::ALL {
            self.reset_view_payload(id);
        }
        self.session_context = None;
        self.view_switcher_query.clear();
        // An armed Esc confirmation targets the conversation being left;
        // carrying it across the boundary could fire session A's interrupt
        // against session B. Disarm so the next Esc starts fresh.
        self.esc_armed_until = None;
        // The queue pointer is scoped like the history cursor: its target
        // belongs to the conversation being left, so a carried pointer would
        // dangle into the new session's outbox. Dissolve without restoring
        // (the composer is emptied right below anyway).
        self.queue_pointer = None;
        self.queue_pointer_draft.clear();
        self.queue_pointer_draft_images.clear();
        self.queue_pointer_draft_text_pastes.clear();
        self.input.clear();
        self.pending_images.clear();
        self.pending_text_pastes.clear();
        self.cursor_position = 0;
        self.input_scroll = 0;
        self.suggestion_index = None;
        self.completion_dismissed = true;
        // The backfill belongs to the conversation being left; the next
        // session rebuilds its own from its transcript.
        self.session_history_backfill.clear();
        self.session_history_backfill_cursor = 0;
    }

    /// Focus a browse view under the ADR-0139 lifecycle. State is initialized
    /// once and restored on later shows. The return value reports first show
    /// for UI defaults only; `enter_view` refreshes authoritative data on
    /// every show.
    pub(crate) fn open_view(&mut self, id: crate::views::ViewId) -> bool {
        if let Some(current) = self.active_view()
            && current != id
        {
            self.deactivate_view(current);
        }
        let first = self.views.open(id).is_none();
        self.surfaces.show_view(id);
        self.restore_view_state(id);
        self.modal_keymap_open = false;
        if id == crate::views::ViewId::Todos {
            self.activity_tab = crate::modal::ActivityTab::Todos;
        } else if id == crate::views::ViewId::Activity {
            self.activity_tab = crate::modal::ActivityTab::Activity;
        }
        first
    }

    /// Whether this view borrows the composer line and therefore owns a
    /// per-view draft slot (Models / Connections / HistorySearch — the
    /// surfaces whose filter field *is* the composer).
    fn owns_composer_draft(&self, id: crate::views::ViewId) -> bool {
        matches!(
            id,
            crate::views::ViewId::Models
                | crate::views::ViewId::Connections
                | crate::views::ViewId::HistorySearch
        )
    }

    /// Park the live composer draft into a view's own slot,
    /// clearing the borrowed line for the view's filter/entry use.
    fn park_draft_into(&mut self, id: crate::views::ViewId) {
        if let Some(state) = self.views.states_mut(&id) {
            state.draft = Some(std::mem::take(&mut self.input));
        }
        self.set_cursor(0);
        self.input_scroll = 0;
        self.suggestion_index = None;
    }

    /// Hand a view's parked draft back to the composer and clear its slot
    /// (the view is leaving the borrowed-line state for chat).
    fn restore_draft_from(&mut self, id: crate::views::ViewId) {
        if let Some(state) = self.views.states_mut(&id) {
            self.input = state.draft.take().unwrap_or_default();
        }
        self.set_cursor_end();
        self.input_scroll = 0;
        self.suggestion_index = None;
    }

    /// The editor chain's "end at chat" teardown (ADR-0139):
    /// whatever picker the chain started from (the nav frame the opener
    /// just popped) hides with its parked composer draft handed back — the
    /// user resumes typing what they were typing before Ctrl+M. The stack
    /// is cleared: nothing between chat and here is reachable via Esc.
    pub(crate) fn restore_chat_after_editor_chain(&mut self) {
        while self.active_view().is_none() && self.transient_return_modal() != Modal::None {
            self.pop_transient_surface();
        }
        if let Some(id) = self.active_view() {
            self.deactivate_view(id);
        }
        self.show_chat_surface();
        self.modal_keymap_open = false;
    }

    /// Snapshot the *current* field values of a browse view into the
    /// registry — the "save on losing focus" half of the contract. The
    /// inverse of the restore in [`Self::open_view`].
    pub(crate) fn save_view_state(&mut self, id: crate::views::ViewId) {
        let scroll = self.view_scroll(id);
        let follow = self.view_follow(id);
        let draft = self.views.states(&id).and_then(|s| s.draft.clone());
        let query = if self.owns_composer_draft(id) {
            self.input.clone()
        } else {
            self.views
                .states(&id)
                .map(|state| state.query.clone())
                .unwrap_or_default()
        };
        let query_active = match id {
            crate::views::ViewId::Models | crate::views::ViewId::Connections => {
                self.model_search
            }
            crate::views::ViewId::HistorySearch => self.history_search,
            _ => false,
        };
        self.views.save(
            id,
            crate::views::ViewState {
                index: self.modal_index,
                scroll,
                follow,
                draft,
                query,
                query_active,
            },
        );
    }

    /// Restore the live fields projected by a retained view. Draft-owning
    /// views first park the chat composer, then load their own retained query.
    fn restore_view_state(&mut self, id: crate::views::ViewId) {
        let state = self.views.states(&id).cloned().unwrap_or_default();
        self.modal_index = state.index;
        self.apply_view_scroll(id, state.scroll);
        self.apply_view_follow(id, state.follow);
        if self.owns_composer_draft(id) {
            if state.draft.is_none() {
                self.park_draft_into(id);
            }
            self.input = state.query;
            self.set_cursor_end();
            self.input_scroll = 0;
            self.suggestion_index = None;
            match id {
                crate::views::ViewId::Models | crate::views::ViewId::Connections => {
                    self.model_search = state.query_active;
                }
                crate::views::ViewId::HistorySearch => {
                    self.history_search = state.query_active;
                }
                _ => {}
            }
        }
    }

    /// Run the exit hook for one exact view without choosing the next
    /// surface. Both hide and switch use this path.
    fn deactivate_view(&mut self, id: crate::views::ViewId) {
        self.save_view_state(id);
        if self.owns_composer_draft(id) {
            self.restore_draft_from(id);
            if id == crate::views::ViewId::HistorySearch {
                self.history_search = false;
                self.history_preview = false;
                self.history_clear_confirm = false;
            } else {
                self.model_search = false;
            }
        }
        if id == crate::views::ViewId::Host {
            self.host_prompting = false;
            self.host_prompt_new = false;
            self.host_preview = None;
            self.host_preview_scroll = 0;
        }
        if id == crate::views::ViewId::Sessions {
            self.session_info_detail = false;
            self.session_detail = None;
            self.session_info_scroll = 0;
        }
        if id == crate::views::ViewId::TokenReport {
            self.token_report_detail = false;
        }
        if id == crate::views::ViewId::Config {
            self.websearch_editing = None;
            if self.config_custom_editing {
                self.theme =
                    Theme::from_color_scheme(&self.color_scheme, &self.custom_color_scheme);
                self.custom_color_draft = self.custom_color_scheme.clone();
                self.config_custom_editing = false;
            }
        }
        if id == crate::views::ViewId::Queue
            && let Some(sid) = self.queue_exit_session.take()
        {
            self.resume_queue(&sid);
        }
        self.views.hide(id);
    }

    /// The `hide` verb (ADR-0139): the active browse view loses focus with
    /// its state retained. Returns `true` when the active surface *was* a
    /// browse view (so callers skip their modal-specific close logic).
    pub(crate) fn hide_active_view(&mut self) -> bool {
        if let Some(id) = self.active_view() {
            self.deactivate_view(id);
            self.show_chat_surface();
            self.modal_keymap_open = false;
            true
        } else {
            false
        }
    }

    /// Explicitly close a retained view, dropping both its navigation state
    /// and its view-owned volatile UI payload. Closing the focused view first
    /// runs the same exit hook as a switch/hide.
    pub(crate) fn close_view(&mut self, id: crate::views::ViewId) {
        if self.active_view() == Some(id) {
            self.deactivate_view(id);
            self.show_chat_surface();
        }
        self.views.close(id);
        self.reset_view_payload(id);
        self.modal_keymap_open = false;
    }

    fn reset_view_payload(&mut self, id: crate::views::ViewId) {
        use crate::views::ViewId;
        match id {
            ViewId::Help => self.help_scroll = 0,
            ViewId::Activity | ViewId::Todos => self.activity_scroll = 0,
            ViewId::Tools | ViewId::Mcp => {
                self.session_scroll = 0;
                self.session_modal_follow = true;
            }
            ViewId::Skills => {
                self.session_scroll = 0;
                self.session_modal_follow = true;
                self.skills_expanded = None;
            }
            ViewId::Permissions => self.permissions_scroll = 0,
            ViewId::UsageStats => {
                self.usage_stats = None;
                self.usage_stats_scroll = 0;
            }
            ViewId::TokenReport => {
                self.token_report = None;
                self.token_report_scroll = 0;
                self.token_report_detail = false;
            }
            ViewId::Btw => {
                self.btw_list.clear();
                self.btw_scroll = 0;
                self.btw_modal_follow = true;
            }
            ViewId::Config => {
                self.config_scroll = 0;
                self.config_detail_scroll = 0;
                self.config_custom_editing = false;
            }
            ViewId::Models | ViewId::Connections => {
                self.model_search = false;
                self.model_scroll = 0;
                self.model_modal_follow = true;
            }
            ViewId::HistorySearch => {
                self.history_search = false;
                self.history_preview = false;
                self.history_clear_confirm = false;
            }
            ViewId::Queue => {
                self.queue_scroll = 0;
                self.queue_modal_follow = true;
            }
            ViewId::Host => {
                self.host_scroll = 0;
                self.host_detail_scroll = 0;
                self.host_preview = None;
                self.host_prompting = false;
                self.host_console_log.clear();
            }
            ViewId::Sessions => {
                self.sessions_overview.clear();
                self.session_info_detail = false;
                self.session_detail = None;
                self.session_info_scroll = 0;
            }
            ViewId::Tree => {
                self.session_tree = muta_contracts::SessionTree::default();
                self.tree_scroll = 0;
                self.tree_modal_follow = true;
            }
        }
    }

    /// Pop the deepest sub-layer of a view (ADR-0139): the single
    /// "one step back" every drill-in routes through — Esc's deepest-first
    /// chain and the outside-click mirror both call this, so the two can
    /// never drift. Returns `true` when a sub-layer was open (the caller
    /// stops: the view itself stays up).
    pub(crate) fn pop_sublayer(&mut self) -> bool {
        match self.active_modal() {
            Modal::Config if self.websearch_editing.is_some() => {
                self.websearch_editing = None;
                self.input.clear();
                self.set_cursor(0);
                true
            }
            Modal::Config if self.config_custom_editing => {
                self.config_custom_editing = false;
                self.theme =
                    Theme::from_color_scheme(&self.color_scheme, &self.custom_color_scheme);
                self.custom_color_draft = self.custom_color_scheme.clone();
                self.input.clear();
                self.set_cursor(0);
                true
            }
            Modal::Config if self.config_focus == crate::overlays::ConfigFocus::Detail => {
                self.config_focus = crate::overlays::ConfigFocus::Categories;
                true
            }
            // Preview is the deepest dashboard layer (painted over the
            // prompting state; the original deepest-first chain popped it
            // first — a preview open while prompting is unreachable in
            // practice, but the order stays explicit here).
            Modal::Host if self.host_preview.is_some() => {
                self.host_preview = None;
                self.host_preview_scroll = 0;
                true
            }
            Modal::Host if self.host_prompting => {
                self.host_prompting = false;
                self.host_prompt_new = false;
                self.input.clear();
                self.set_cursor(0);
                true
            }
            Modal::TokenReport if self.token_report_detail => {
                self.token_report_detail = false;
                self.token_report_scroll = 0;
                true
            }
            Modal::Sessions if self.session_info_detail => {
                self.session_info_detail = false;
                self.session_detail = None;
                self.session_info_scroll = 0;
                true
            }
            _ => false,
        }
    }

    /// The dispatcher-facing dismiss verb (ADR-0139): what Esc /
    /// outside-click / Ctrl+C do to whatever surface is up. The quick
    /// switcher cancels back to the surface it was opened over (it is a
    /// transient chooser, never a view) — and restores that surface's
    /// cursor/scroll from the registry, because the switcher borrowed
    /// `modal_index` and the shared session-scroll slot while it was up.
    /// A retained browse view hides with its state saved.
    /// Returns `true` when either applied, so legacy close paths can skip
    /// their own handling.
    pub(crate) fn dismiss_surface(&mut self) -> bool {
        if self.active_modal() == Modal::ViewSwitcher {
            self.pop_transient_surface();
            self.modal_keymap_open = false;
            return true;
        }
        self.hide_active_view()
    }

    /// The per-view body-scroll slot, mirroring [`Self::modal_scroll_field`]
    /// for the retained views. Tools/Mcp/Skills share `session_scroll`
    /// exactly as `modal_scroll_field` already routes them.
    ///
    /// Config is excluded: its cursor lives in `config_category` /
    /// `config_detail_index` / `config_focus` (not `modal_index`) and its
    /// body scrolls in two pane-specific slots. Its open/close paths never
    /// reset those fields today, so retention there is already the default —
    /// `open_view`'s save/restore of `ViewState` is a no-op for it.
    fn view_scroll(&self, id: crate::views::ViewId) -> usize {
        match id {
            crate::views::ViewId::Help => self.help_scroll,
            crate::views::ViewId::Activity | crate::views::ViewId::Todos => self.activity_scroll,
            crate::views::ViewId::Tools
            | crate::views::ViewId::Mcp
            | crate::views::ViewId::Skills => self.session_scroll,
            crate::views::ViewId::Permissions => self.permissions_scroll,
            crate::views::ViewId::UsageStats => self.usage_stats_scroll,
            crate::views::ViewId::TokenReport => self.token_report_scroll,
            crate::views::ViewId::Btw => self.btw_scroll,
            crate::views::ViewId::HistorySearch => self.history_scroll,
            crate::views::ViewId::Models | crate::views::ViewId::Connections => self.model_scroll,
            crate::views::ViewId::Queue => self.queue_scroll,
            crate::views::ViewId::Host => match self.host_focus {
                crate::overlays::DashboardFocus::List => self.host_scroll,
                crate::overlays::DashboardFocus::Detail => self.host_detail_scroll,
            },
            crate::views::ViewId::Sessions => self.session_scroll,
            crate::views::ViewId::Tree => self.tree_scroll,
            // Config: no single slot (see doc above); the saved state is not
            // used for it.
            crate::views::ViewId::Config => 0,
        }
    }

    fn apply_view_scroll(&mut self, id: crate::views::ViewId, scroll: usize) {
        match id {
            crate::views::ViewId::Help => self.help_scroll = scroll,
            crate::views::ViewId::Activity | crate::views::ViewId::Todos => {
                self.activity_scroll = scroll;
            }
            crate::views::ViewId::Tools
            | crate::views::ViewId::Mcp
            | crate::views::ViewId::Skills => {
                self.session_scroll = scroll;
            }
            crate::views::ViewId::Permissions => self.permissions_scroll = scroll,
            crate::views::ViewId::UsageStats => self.usage_stats_scroll = scroll,
            crate::views::ViewId::TokenReport => self.token_report_scroll = scroll,
            crate::views::ViewId::Btw => self.btw_scroll = scroll,
            crate::views::ViewId::HistorySearch => self.history_scroll = scroll,
            crate::views::ViewId::Models | crate::views::ViewId::Connections => {
                self.model_scroll = scroll;
            }
            crate::views::ViewId::Queue => self.queue_scroll = scroll,
            crate::views::ViewId::Host => match self.host_focus {
                crate::overlays::DashboardFocus::List => self.host_scroll = scroll,
                crate::overlays::DashboardFocus::Detail => self.host_detail_scroll = scroll,
            },
            crate::views::ViewId::Sessions => self.session_scroll = scroll,
            crate::views::ViewId::Tree => self.tree_scroll = scroll,
            // Config: no single slot; retention is field-native (see
            // `view_scroll`).
            crate::views::ViewId::Config => {}
        }
    }

    fn view_follow(&self, id: crate::views::ViewId) -> bool {
        match id {
            crate::views::ViewId::Tools
            | crate::views::ViewId::Mcp
            | crate::views::ViewId::Skills => self.session_modal_follow,
            crate::views::ViewId::Btw => self.btw_modal_follow,
            crate::views::ViewId::HistorySearch => self.history_modal_follow,
            crate::views::ViewId::Models | crate::views::ViewId::Connections => {
                self.model_modal_follow
            }
            crate::views::ViewId::Queue => self.queue_modal_follow,
            crate::views::ViewId::Host => self.host_modal_follow,
            crate::views::ViewId::Sessions => self.session_modal_follow,
            crate::views::ViewId::Tree => self.tree_modal_follow,
            // These surfaces don't track a follow flag (plain scroll bodies).
            _ => true,
        }
    }

    fn apply_view_follow(&mut self, id: crate::views::ViewId, follow: bool) {
        match id {
            crate::views::ViewId::Tools
            | crate::views::ViewId::Mcp
            | crate::views::ViewId::Skills
            | crate::views::ViewId::Sessions => self.session_modal_follow = follow,
            crate::views::ViewId::Btw => self.btw_modal_follow = follow,
            crate::views::ViewId::HistorySearch => self.history_modal_follow = follow,
            crate::views::ViewId::Models | crate::views::ViewId::Connections => {
                self.model_modal_follow = follow;
            }
            crate::views::ViewId::Queue => self.queue_modal_follow = follow,
            crate::views::ViewId::Host => self.host_modal_follow = follow,
            crate::views::ViewId::Tree => self.tree_modal_follow = follow,
            // Plain scroll bodies do not expose a follow flag.
            _ => {}
        }
    }
    /// Splice the `idx`-th live completion's label into [`App::input`] over
    /// its `[replace_start, replace_end)` byte range, landing the cursor
    /// just past the inserted text. Shared by `Tab` cycling and `Enter`
    /// commit.
    ///
    /// **Slash commands are terminal accepts.** Accepting a `/command` is a
    /// commit: no trailing space is appended, the highlight is cleared, and
    /// [`App::completion_dismissed`] is latched so the popup stays hidden
    /// until the next edit. This unifies Tab and Enter — a `/pursue ` (with
    /// the space) would immediately match the subcommand prefix and
    /// re-trigger the menu (defeating the point of accepting), and once a
    /// slash label replaces the whole input the candidate list collapses to
    /// the single exact match anyway, so cycling has nothing to cycle
    /// through. The user opts back into completion by editing the input
    /// (clearing the latch) or, for subcommand discovery, by typing a space.
    ///
    /// **`@path` mentions keep cycling.** Files splice inline, so multiple
    /// candidates survive an accept and Tab is meant to walk them; the popup
    /// therefore re-opens for path accepts and no latch is set. Directories
    /// end in `/` and also skip the trailing space so the popup re-triggers
    /// on the dir's contents.
    pub fn accept_completion(&mut self, idx: usize) {
        let completions = self.completions();
        let Some(comp) = completions.get(idx) else {
            return;
        };
        // Replacement range and inserted bytes are backend-owned completion
        // semantics. The TUI only translates the wire offsets and applies the
        // edit; it does not decide how `@` or trailing whitespace behave.
        let replace_start = comp.replace_start;
        let replace_end = comp.replace_end;
        let insert_text = &comp.insert_text;
        let mut new_input = String::with_capacity(self.input.len() + insert_text.len());
        new_input.push_str(&self.input[..replace_start]);
        new_input.push_str(insert_text);
        let cursor_byte = replace_start + insert_text.len();
        new_input.push_str(&self.input[replace_end..]);
        self.input = new_input;
        self.set_cursor(self.input[..cursor_byte].chars().count());
        // A terminal accept is a commit: exit completion so the popup does
        // not re-open on the just-spliced label (which would collapse to a
        // single exact match and, for slash commands, with a trailing space
        // fire the subcommand menu). Applies equally to Tab and Enter since
        // both route through here. Project-scan `@path` *directory* accepts
        // stay live so Tab keeps descending the directory tree.
        if !matches!(comp.kind, CompletionItemKind::PathDir) {
            self.suggestion_index = None;
            self.completion_dismissed = true;
        }
    }

    /// Toggle the expansion of the tool step / reasoning trace at `mi`,
    /// keeping its header pinned to the screen position the user interacted with.
    ///
    /// A toggle inserts or removes the body lines that sit *below* the header,
    /// so the header's own content-line never moves. That gives a simple rule
    /// for keeping the header where the user clicked:
    ///
    /// - Visible (in-stream) header: leave `scroll` untouched and the header
    ///   stays on the same row; the body grows or shrinks beneath it.
    /// - Sticky-overlay header (its real header is scrolled off the top): point
    ///   `scroll` at the recorded header content-line so the real header lands
    ///   at row 0 where the overlay sat. The line is also recorded in
    ///   `pin_summary_line` so the per-frame clamp does not pull it back down
    ///   once the collapsed body shortens the stream.
    /// - Either way `follow_bottom` is cleared: the user is now pinning their
    ///   attention on this header, so the next frame's auto-follow must not
    ///   yank it away (this is what previously let an expand push the header
    ///   off-screen while the view was following the bottom).
    ///
    /// Returns `true` when a step was actually toggled, so callers can gate
    /// side effects like clearing the text selection.
    pub(crate) fn toggle_step_pinned(
        &mut self,
        messages: &mut [TranscriptMessage],
        mi: usize,
    ) -> bool {
        let pinned_to_top = self.sticky_step == Some(mi);
        let sticky_summary_line = self.sticky_summary_line;

        let transcript_top_y = self
            .layout_map
            .transcript_content_rect()
            .map(|r| r.y)
            .unwrap_or(0);
        let prev_region = self.layout_map.first_region_for_message(mi);
        let summary_screen_y = prev_region.map(|r| r.rect.y);
        let msg_line_index = summary_screen_y
            .map(|y| self.scroll as usize + (y.saturating_sub(transcript_top_y) as usize));

        let toggled = resolve_focused_mut(messages, &self.focus_stack, mi).and_then(|message| {
            if let Some(expanded) = message.tool_step_expanded() {
                message.pin_tool_step_expanded(!expanded);
                Some(!expanded)
            } else if let Some(expanded) = message.command_result_expanded() {
                message.pin_command_result_expanded(!expanded);
                Some(!expanded)
            } else if let Some(expanded) = message.thinking_expanded() {
                message.pin_thinking_expanded(!expanded);
                Some(!expanded)
            } else if let Some(expanded) = message.provider_retry_expanded() {
                message.pin_provider_retry_expanded(!expanded);
                Some(!expanded)
            } else if let Some(expanded) = message.notice_expanded() {
                message.pin_notice_expanded(!expanded);
                Some(!expanded)
            } else {
                None
            }
        });

        let Some(newly_expanded) = toggled else {
            return false;
        };

        self.follow_bottom = false;

        // `[tui] expand_auto_scroll` (default off): a toggle is a read
        // interaction, so by default the scroll offset is the user's and
        // stays put — the card grows or shrinks in place. Only the sticky
        // header's collapse still re-anchors, because that overlay's row
        // must land where the summary it covered sits. The settle request
        // latches either way: the toggle changed the stream's height, so the
        // clamp must validate the (untouched) offset against the *new*
        // measurement — a hard collapse can shrink the tail below it.
        if !self.expand_auto_scroll {
            if !newly_expanded && pinned_to_top {
                if let Some(summary_line) = sticky_summary_line {
                    self.scroll = summary_line.min(u16::MAX as usize) as u16;
                    self.pin_summary_line = Some(summary_line);
                }
            } else {
                self.pin_summary_line = None;
            }
            self.scroll_settle_pending = true;
            return true;
        }

        if newly_expanded {
            // When expanding, if the summary line was not already at the top of the viewport,
            // scroll down so that the summary line shifts up toward the top of the viewport (row 0 or 1),
            // giving maximum vertical space for the newly revealed body content to be visible.
            if let Some(y) = summary_screen_y
                && let Some(line_idx) = msg_line_index
            {
                let rel_y = y.saturating_sub(transcript_top_y);
                if rel_y > 1 {
                    self.scroll = line_idx.saturating_sub(1).min(u16::MAX as usize) as u16;
                }
            }
            self.pin_summary_line = None;
        } else if pinned_to_top {
            if let Some(summary_line) = sticky_summary_line {
                self.scroll = summary_line.min(u16::MAX as usize) as u16;
                self.pin_summary_line = Some(summary_line);
            }
        } else if let Some(line_idx) = msg_line_index {
            // If collapsing a step that was scrolled above the viewport, keep the collapsed summary visible
            if line_idx < self.scroll as usize {
                self.scroll = line_idx.min(u16::MAX as usize) as u16;
                self.pin_summary_line = Some(line_idx);
            } else {
                self.pin_summary_line = None;
            }
        } else {
            self.pin_summary_line = None;
        }

        // The toggle changed the transcript's height, so the scroll target
        // computed above is only valid against the *new* layout — which does
        // not exist until the next frame renders. Latch the settle request so
        // the event loop stages that frame (measure first, paint the final
        // offset second) instead of painting an intermediate viewport that
        // the post-draw clamp then has to correct.
        self.scroll_settle_pending = true;

        true
    }
    pub(crate) fn visible_interactive_targets(&self) -> Vec<InteractiveTarget> {
        let mut targets = self.layout_map.interactive_targets();
        if let Some(message_idx) = self.sticky_step
            && let Some(message) = self.focused_messages().get(message_idx)
        {
            let target = if message.is_thinking() {
                InteractiveTarget::thinking(message_idx)
            } else if message.is_provider_retry() {
                InteractiveTarget::provider_retry(message_idx)
            } else if message.is_tool_step() || message.is_envoy_task() {
                InteractiveTarget::tool_step(message_idx)
            } else {
                return targets;
            };
            if !targets.contains(&target) {
                targets.insert(0, target);
            }
        }
        targets
    }

    pub(crate) fn retain_visible_focused_target(&mut self) {
        if self.active_modal() != Modal::None {
            self.focused_target = None;
            return;
        }
        if let Some(target) = self.focused_target
            && !self.visible_interactive_targets().contains(&target)
        {
            self.focused_target = None;
        }
    }

    pub(crate) fn focus_interactive_target(&mut self, direction: i8) {
        let targets = self.visible_interactive_targets();
        if targets.is_empty() {
            self.focused_target = None;
            return;
        }

        let current = self
            .focused_target
            .and_then(|target| targets.iter().position(|candidate| *candidate == target));
        let next = match (current, direction < 0) {
            (Some(0), true) => targets.len() - 1,
            (Some(idx), true) => idx - 1,
            (Some(idx), false) => (idx + 1) % targets.len(),
            (None, true) => targets.len() - 1,
            (None, false) => 0,
        };

        self.focused_target = Some(targets[next]);
        self.selection = SelectionState::None;
        self.drag.cancel();
    }

    /// Whether the view is currently zoomed into an envoy task.
    pub fn in_envoy_view(&self) -> bool {
        !self.focus_stack.is_empty()
    }

    /// The message slice currently in view: the `/btw` side transcript when
    /// the side view is active (ADR-0017), the focused envoy task's child
    /// messages when zoomed, or the root conversation otherwise.
    pub fn focused_messages(&self) -> &[TranscriptMessage] {
        if self.in_side_view {
            return &self.side_messages;
        }
        let Some(frame) = self.focus_stack.last() else {
            return &self.messages;
        };
        self.messages
            .iter()
            .find_map(|message| {
                if message.is_envoy_task()
                    && message.tool_step_call_id() == Some(frame.call_id.as_str())
                {
                    message.envoy_children()
                } else {
                    None
                }
            })
            .unwrap_or(&[])
    }

    /// Reset transient view state (scroll, selection, sticky pinning) when the
    /// focused message slice changes.
    pub(crate) fn reset_view_state(&mut self) {
        self.scroll = 0;
        self.follow_bottom = true;
        self.selection = SelectionState::None;
        self.drag.cancel();
        self.sticky_step = None;
        self.sticky_rect = None;
        self.sticky_summary_line = None;
        self.pin_summary_line = None;
        self.scroll_settle_pending = false;
        self.focused_target = None;
    }

    /// The chrome of whichever session the user is currently viewing: the
    /// focused aside's entry while in the aside view, the primary's
    /// (carried by the legacy `App` fields) otherwise. Renderers must read
    /// activity/round state through this accessor — never the bare fields —
    /// so a view can only ever display its own session's status.
    pub fn viewed_chrome(&self) -> SessionChrome {
        if self.in_side_view
            && let Some(side_id) = self.side_session_id.as_deref()
            && let Some(chrome) = self.session_chrome.get(side_id)
        {
            return chrome.clone();
        }
        SessionChrome {
            activity: self.activity_status.clone(),
            responding: self.round_started_at.is_some() || !self.activity_status.is_empty(),
            round_count: self.round_count,
            current_turn: self.current_turn,
            round_started_at: self.round_started_at,
            can_retry: self.loop_status.is_idle() && self.harness_retry_pending,
        }
    }

    /// Zoom into an envoy task's child messages.
    pub fn enter_envoy(&mut self, call_id: String) {
        let saved_scroll = ScrollSnapshot {
            offset: self.scroll,
            follow_bottom: self.follow_bottom,
        };
        self.focus_stack.push(ZoomFrame {
            call_id,
            saved_scroll,
        });
        self.reset_view_state();
    }

    /// Return from the current envoy view to its parent. Returns true if a
    /// view was actually popped.
    pub fn exit_envoy(&mut self) -> bool {
        if let Some(frame) = self.focus_stack.pop() {
            self.reset_view_state();
            self.scroll = frame.saved_scroll.offset;
            self.follow_bottom = frame.saved_scroll.follow_bottom;
            true
        } else {
            false
        }
    }

    /// Enter the `/btw` aside view (ADR-0017, ADR-0103). The side transcript
    /// ([`App::side_messages`]) becomes the viewed stream and the aside page
    /// header reports the primary session's coarse status. The buffer itself
    /// was already back-filled from `SideViewOpened`'s payload by the
    /// listener (ADR-0103 §6), so entering never clears it. Reuses the envoy
    /// zoom's `reset_view_state` so the swap feels identical to focusing a
    /// task step.
    pub fn enter_side_view(&mut self, side_id: String) {
        self.side_session_id = Some(side_id.clone());
        self.in_side_view = true;
        self.parent_status = ParentStatus::Idle;
        // An armed Esc confirmation is view-scoped: entering the aside must
        // not inherit the primary's arm (a second Esc here would otherwise
        // fire the *aside's* interrupt off a confirmation aimed at the
        // primary's round).
        self.esc_armed_until = None;
        // View-scoped chrome (the aside-view activity-bar fix): snapshot the
        // primary's live chrome, then swap the displayed chrome to the
        // aside's own `SessionChrome` entry. A primary round still streaming
        // in the background keeps its activity text, elapsed timer, and
        // counters parked in `saved_primary_chrome`; the aside view shows
        // only the aside's state — typically idle on entry ("new aside, no
        // round"), or streaming if re-entering a running aside.
        //
        // The snapshot is taken only when none is parked: jumping between
        // asides (A → B, or re-entering A) must not re-snapshot, because the
        // displayed chrome at that moment is the *previous aside's* —
        // overwriting would silently destroy the primary's parked state.
        if self.saved_primary_chrome.is_none() {
            self.saved_primary_chrome = Some(SessionChrome {
                activity: self.activity_status.clone(),
                responding: self.round_started_at.is_some() || !self.activity_status.is_empty(),
                round_count: self.round_count,
                current_turn: self.current_turn,
                round_started_at: self.round_started_at,
                can_retry: self.loop_status.is_idle() && self.harness_retry_pending,
            });
        }
        if let Some(chrome) = self.session_chrome.get(&side_id).cloned() {
            self.apply_chrome(&chrome);
        } else {
            // First entry: the aside has no chrome history yet — a fresh,
            // idle surface. Clearing rather than inheriting is the point.
            self.activity_status.clear();
            self.round_started_at = None;
            self.round_count = 0;
            self.current_turn = 0;
        }
        self.reset_view_state();
    }

    /// Leave the `/btw` aside view and return to the primary transcript
    /// (ADR-0103). Detach is non-destructive: the aside keeps running and its
    /// buffer is **retained** (clipped out of view), so re-entering shows the
    /// full history without a refetch. The aside session stays live on the
    /// harness side until explicitly closed.
    pub fn exit_side_view(&mut self) {
        // Restore the primary's parked chrome (the aside-view activity-bar
        // fix): whatever the primary was doing when the user entered the
        // aside — idle, or a round still streaming in the background — its
        // activity bar, elapsed timer, and counters come back exactly as
        // they were. Without this, exiting into a running primary would show
        // the aside's (or a cleared) bar until the next primary event.
        if let Some(primary) = self.saved_primary_chrome.take() {
            self.apply_chrome(&primary);
        } else {
            // No snapshot exists only in a legacy in-process state that
            // predates the snapshot write; clear to a neutral surface and
            // let the next frame's per-session bookkeeping rebuild it.
            self.activity_status.clear();
            self.round_started_at = None;
        }
        self.in_side_view = false;
        self.side_session_id = None;
        // Dropping any armed Esc confirmation is part of leaving: the arm
        // targeted the aside's round, and a carried arm would fire the
        // *primary's* interrupt on the next Esc. Covers the Ctrl+C detach
        // and the `SideViewSignal::Closed` backstop alike.
        self.esc_armed_until = None;
        self.reset_view_state();
    }

    /// Overwrite the display chrome (the `App`-level fields the renderers
    /// read) from a [`SessionChrome`] entry. The single write path for
    /// view swaps; per-event updates during a round go through the
    /// listener's routing instead.
    fn apply_chrome(&mut self, chrome: &SessionChrome) {
        self.activity_status = chrome.activity.clone();
        self.round_started_at = chrome.round_started_at;
        self.round_count = chrome.round_count;
        self.current_turn = chrome.current_turn;
    }

    /// Cycle to the previous (`dir < 0`) or next (`dir > 0`) sibling envoy
    /// task at the current focus level. No-op when not in an envoy view or
    /// when there are no siblings.
    pub fn cycle_sibling(&mut self, dir: i8) {
        let Some(current) = self.focus_stack.last() else {
            return;
        };
        let current_id = current.call_id.clone();
        let task_ids: Vec<String> = self
            .messages
            .iter()
            .filter_map(|message| {
                if message.is_envoy_task() {
                    message.tool_step_call_id().map(String::from)
                } else {
                    None
                }
            })
            .collect();
        let Some(idx) = task_ids.iter().position(|id| *id == current_id) else {
            return;
        };
        if task_ids.len() < 2 {
            return;
        }
        let n = task_ids.len() as isize;
        let next = ((idx as isize + dir as isize).rem_euclid(n)) as usize;
        if let Some(frame) = self.focus_stack.last_mut() {
            frame.call_id = task_ids[next].clone();
        }
        self.reset_view_state();
    }

    /// Rows shown in the Ctrl+R history panel, as `(original_index,
    /// FuzzyMatch)` pairs indexing into [`App::input_history`]. The single
    /// source of truth for navigation (Up/Down clamp), Enter-accept, and
    /// rendering — they all index into this same vector so the cursor never
    /// lands on a row the user cannot see.
    ///
    /// The list is always the **whole cross-session history**, independent of
    /// which session or workspace produced each entry — that is the entire
    /// point of Ctrl+R (the inline ↑/↓ recall, by contrast, is scoped to the
    /// current session via [`App::current_session_history`]). Entries are
    /// ordered newest-first by `created_at_ms`.
    ///
    /// With an empty query (`App::input`, which the panel borrows as its live
    /// filter) every entry shows, unhighlighted. Once a query is present the
    /// rows are the fuzzy-ranked matches, best score first, with the original
    /// newest-first order as the stable tiebreaker. Recomputed from scratch
    /// each call: history is small and this runs at most a few times per
    /// frame, so caching would only add stale-state risk.
    pub fn history_rows(&self) -> Vec<(usize, fuzzy::FuzzyMatch)> {
        // The display order: newest-first. The on-disk file is already stored
        // newest-first, but in-memory appends during this run land at the
        // tail, so re-sort by created_at_ms (stable) to keep the panel's order
        // correct without mutating the stored Vec.
        let order: Vec<usize> = self.history_order();
        let texts: Vec<&str> = order
            .iter()
            .map(|&i| {
                self.input_history
                    .get(i)
                    .map(|e| e.text.as_str())
                    .unwrap_or("")
            })
            .collect();
        if self.input.is_empty() {
            // Empty query → show everything newest-first, unhighlighted.
            return order
                .into_iter()
                .map(|i| {
                    (
                        i,
                        fuzzy::FuzzyMatch {
                            score: 0,
                            positions: Vec::new(),
                        },
                    )
                })
                .collect();
        }
        // `rank` returns indices into `texts`; map them back to the original
        // `input_history` indices via `order`. The matched char positions are
        // indices into the entry text itself, so they need no remap.
        let mut ranked = fuzzy::rank(&texts, &self.input);
        fuzzy::sort_by_score(&mut ranked);
        ranked.into_iter().map(|(ti, m)| (order[ti], m)).collect()
    }

    /// The newest-first ordering of [`App::input_history`] by `created_at_ms`,
    /// as original indices into that Vec. Stable on ties so the on-disk order
    /// survives. Shared by [`Self::history_rows`] (Ctrl+R) and
    /// [`Self::current_session_history`] (inline ↑/↓) so both surfaces agree
    /// on what "newest" means.
    pub fn history_order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.input_history.len()).collect();
        // Newest-first by `created_at_ms`; entries stamped within the same
        // millisecond (a fast send burst) break ties by insertion order — the
        // later index is the newer prompt — so "newest-first" stays
        // well-defined instead of degrading to oldest-first on a tie.
        order.sort_by(|&a, &b| {
            self.input_history[b]
                .created_at_ms
                .cmp(&self.input_history[a].created_at_ms)
                .then_with(|| b.cmp(&a))
        });
        order
    }

    /// The current session's history, newest-first. This is what the inline
    /// ↑/↓ recall walks: the union of the **persisted** history
    /// ([`Self::input_history`], filtered to entries whose `session_id`
    /// matches [`App::current_session_id`]) and the **derived** transcript
    /// rows ([`Self::session_history_backfill`]), so arrow-key recall
    /// surfaces exactly the prompts of *this* conversation — including ones
    /// this client never recorded (a session resumed from elsewhere). Ctrl+R
    /// is unaffected — it searches the whole persisted list regardless of
    /// session.
    ///
    /// Returns indices into the combined row space: `0..input_history.len()`
    /// address the persisted store, `input_history.len() + i` addresses the
    /// `i`-th backfill row. [`Self::history_entry`] resolves either kind, so
    /// callers never branch on the boundary.
    pub fn current_session_history(&self) -> Vec<usize> {
        let sid = self.current_session_id.as_str();
        let mut rows: Vec<(u64, usize)> = self
            .input_history
            .iter()
            .enumerate()
            .filter(|(_, e)| e.session_id.as_deref() == Some(sid))
            .map(|(i, e)| (e.created_at_ms, i))
            .collect();
        let base = self.input_history.len();
        rows.extend(
            self.session_history_backfill
                .iter()
                .enumerate()
                // Walked newest-first below, so the backfill's oldest-first
                // storage order must be reversed to reach `created_at_ms`
                // parity — ties against persisted rows resolve to the
                // transcript's own (older-first) order via the stable sort.
                .map(|(i, e)| (e.created_at_ms, base + i)),
        );
        // Newest-first: stable sort keeps within-store order on ties, and the
        // backfill rows (transcript append order) follow persisted rows of the
        // same millisecond.
        rows.sort_by_key(|&(created_at_ms, _)| std::cmp::Reverse(created_at_ms));
        rows.into_iter().map(|(_, i)| i).collect()
    }

    /// Resolve a row index from [`Self::current_session_history`] to its
    /// entry, transparently spanning the persisted store (`0..len`) and the
    /// session backfill (`len..`). `None` when the index is out of range.
    pub fn history_entry(&self, idx: usize) -> Option<&muta_contracts::HistoryEntry> {
        if idx < self.input_history.len() {
            self.input_history.get(idx)
        } else {
            self.session_history_backfill
                .get(idx - self.input_history.len())
        }
    }

    /// Drop backfill rows whose text this session has since **recorded**
    /// (the send path persisted it, possibly by re-tagging an existing
    /// global-dedup row into this session). Called after
    /// [`Self::record_input_history`] so the union the ↑/↓ walk sees never
    /// contains the same prompt twice: without this, a prompt that was
    /// backfilled on resume and then re-sent through this client would
    /// surface as two adjacent rows.
    pub fn prune_backfill_after_record(&mut self, text: &str) {
        self.session_history_backfill.retain(|e| e.text != text);
    }

    /// Seed [`Self::session_history_backfill`] with the **viewed
    /// transcript's** genuine chat prompts, so the inline ↑/↓ recall reflects
    /// the conversation the user is actually looking at rather than only what
    /// this client's `history.json` happens to contain.
    ///
    /// This is the resume path: `ConversationReplaced` hands the TUI another
    /// session's transcript, and prompts typed into that session by a
    /// *different* client (or before this `history.json` existed) were never
    /// recorded locally. Without the backfill, `↑` after a resume comes up
    /// empty even though the conversation visibly contains prompts. The
    /// initial startup transcript is backfilled the same way before the
    /// first frame.
    ///
    /// Only `UserMessageOrigin::Chat` rows count — slash commands
    /// (`/model`, …) and `!shell` passthroughs are UI gestures excluded from
    /// the history by contract (`[input_history] record_commands = false`),
    /// and queued-but-unsent rows are not prompts yet. A prompt already
    /// recorded by this client (present in the persisted history under this
    /// session) is skipped, so live sends and backfills never duplicate a
    /// row.
    ///
    /// The backfill is **derived state, never persisted**: transcript rows
    /// already live in the session file (the durable source of truth,
    /// ADR-0018), so writing them into `history.json` would duplicate the
    /// store and race the cross-process merge. Timestamps come from the
    /// transcript where available (`sent_at_ms`, falling back to `now_ms`
    /// for legacy rows so ordering stays stable).
    ///
    /// `tail` is the transcript's unconsumed suffix as `(text, is_chat,
    /// sent_at_ms)` triples — copied out by the caller, which cannot lend
    /// the transcript while `App` is borrowed mutably here.
    pub fn backfill_session_history(&mut self, tail: &[(String, bool, u64)], now_ms: u64) {
        let sid = self.current_session_id.as_str();
        let recorded: HashSet<&str> = self
            .input_history
            .iter()
            .filter(|e| e.session_id.as_deref() == Some(sid))
            .map(|e| e.text.as_str())
            .collect();
        for (text, is_chat, sent_at_ms) in tail {
            if !is_chat || text.is_empty() || recorded.contains(text.as_str()) {
                continue;
            }
            // Same prompt twice in one conversation (an intentional resend)
            // is one recallable row — the newest position wins, matching the
            // persisted history's newest-first contract.
            if let Some(existing) = self
                .session_history_backfill
                .iter_mut()
                .find(|e| e.text == *text)
            {
                existing.created_at_ms = (*sent_at_ms).max(existing.created_at_ms);
                continue;
            }
            self.session_history_backfill
                .push(muta_contracts::HistoryEntry::new(
                    text.clone(),
                    Some(self.current_session_id.clone()),
                    Some(self.current_workspace.clone()),
                    if *sent_at_ms == 0 {
                        now_ms
                    } else {
                        *sent_at_ms
                    },
                ));
        }
    }

    /// Record `entry` in the cross-session input history, tagged with the
    /// current session id + workspace and stamped "now": reset the up/down
    /// recall cursor, dedup against the most recent same-text+same-session
    /// entry, and persist the new entry to disk immediately (off-thread) so
    /// it survives an unclean exit and is visible to concurrent sessions
    /// right away rather than only on exit.
    ///
    /// `images` / `text_pastes` are the attachments staged behind the chips
    /// in `entry` at send time. They are **not** persisted (history.json is
    /// rebuildable cosmetic telemetry, never conversation data — ADR-0018)
    /// but are cached in memory keyed by the entry's `(text, session_id)`
    /// identity, so the ↑/↓ and Ctrl+R recall paths can restore a just-sent
    /// or interrupted message's attachments instead of shipping a bare chip
    /// label the model would read as literal text.
    ///
    /// The origin (session/workspace) is what separates Ctrl+R (searches the
    /// whole history) from inline ↑/↓ (walks only this session's entries).
    pub fn record_input_history(
        &mut self,
        entry: String,
        images: Vec<ImagePart>,
        text_pastes: Vec<String>,
    ) {
        self.history_index = None;
        if entry.is_empty() && images.is_empty() && text_pastes.is_empty() {
            return;
        }
        // Slash-command invocations (`/model`, `/new`, …) are UI gestures,
        // not prompts: they are already visible in the transcript, and most
        // users don't want `/model` noise cluttering the Ctrl+R picker. Skip
        // them unless `[input_history] record_commands` opts them back in.
        if entry.starts_with('/') && !self.input_history_record_commands {
            return;
        }
        let now = crate::event_loop::now_epoch_ms();
        // Ensure strictly-increasing timestamps. `now_epoch_ms()` can return
        // the same millisecond for a rapid burst of sends, and the history
        // order's stable sort would then keep input order — putting the
        // older prompt ahead of the newer one and breaking the newest-first
        // contract (the inline ↑ would land on the stale entry first). The
        // wall clock stays the baseline; when it has not advanced past the
        // newest recorded entry, nudge the stamp forward by one.
        let latest_ts = self
            .input_history
            .iter()
            .map(|e| e.created_at_ms)
            .max()
            .unwrap_or(0);
        let now = if now > latest_ts {
            now
        } else {
            latest_ts.saturating_add(1)
        };
        let session_id = if self.current_session_id.is_empty() {
            None
        } else {
            Some(self.current_session_id.clone())
        };
        let workspace = if self.current_workspace.is_empty() {
            None
        } else {
            Some(self.current_workspace.clone())
        };
        // Cache the attachments first (before the dedup early-return) so a
        // repeat send of the same prompt refreshes the payloads a recall
        // will restore, even though no new history row is pushed.
        if !images.is_empty() || !text_pastes.is_empty() {
            let identity = (entry.clone(), session_id.clone());
            if !self.history_attachments.contains_key(&identity) {
                self.history_attachments_order.push_back(identity.clone());
            }
            self.history_attachments.insert(
                identity,
                HistoryAttachments {
                    images,
                    text_pastes,
                },
            );
            self.prune_history_attachments();
        }
        // With `[input_history] dedup` (default on) the prompt text alone is
        // the identity: the same prompt sent twice — even in a different
        // session — stays one row. Re-sending refreshes the timestamp (so the
        // entry bubbles to the top of the newest-first picker) and adopts the
        // newest known origin (so ↑/↓ in the session that last sent it still
        // finds it), then persists the refreshed entry.
        if self.input_history_dedup {
            if let Some(existing) = self.input_history.iter_mut().find(|e| e.text == entry) {
                existing.created_at_ms = now;
                if session_id.is_some() {
                    existing.session_id = session_id;
                }
                if workspace.is_some() {
                    existing.workspace = workspace;
                }
                let refreshed = existing.clone();
                // The text is now recorded under this session: drop any
                // transcript-derived backfill row for it so the ↑/↓ union
                // never shows the same prompt twice.
                self.prune_backfill_after_record(&refreshed.text);
                if self.input_history_persist {
                    tokio::task::spawn_blocking(move || {
                        let _ = muta_persistence::config::Config::save_history(
                            std::slice::from_ref(&refreshed),
                            true,
                        );
                    });
                }
                return;
            }
            let recorded = muta_contracts::HistoryEntry::new(entry, session_id, workspace, now);
            self.push_history(recorded.clone());
            if self.input_history_persist {
                tokio::task::spawn_blocking(move || {
                    let _ = muta_persistence::config::Config::save_history(
                        std::slice::from_ref(&recorded),
                        true,
                    );
                });
            }
            return;
        }
        // Dedup disabled: dedup against the newest same-text entry in *this*
        // session — typing the same prompt twice in a row should not produce
        // two adjacent rows, but the same words typed in a different session
        // legitimately are a distinct history entry (each keeps its own
        // origin).
        let already_latest_in_session = self
            .current_session_history()
            .first()
            .and_then(|&i| self.history_entry(i))
            .is_some_and(|e| e.text == entry && e.session_id == session_id);
        if already_latest_in_session {
            return;
        }
        let recorded = muta_contracts::HistoryEntry::new(entry, session_id, workspace, now);
        self.push_history(recorded.clone());
        // Same dedup guard as above: a backfilled row for this text is now
        // redundant with the recorded one.
        self.prune_backfill_after_record(&recorded.text);
        // `save_history` lock+merges into the on-disk union, so persisting just
        // the new entry is enough and cheap. Off-thread: the write takes a file
        // lock and must not block the event loop. Skipped entirely when disk
        // persistence is disabled (tests).
        if self.input_history_persist {
            tokio::task::spawn_blocking(move || {
                let _ = muta_persistence::config::Config::save_history(
                    std::slice::from_ref(&recorded),
                    false,
                );
            });
        }
    }

    /// Wipe the entire input history — the Ctrl+R picker's "clear" action.
    /// Clears the in-memory list, the attachment cache, and truncates the
    /// on-disk history file so the change survives an unclean exit. The caller
    /// is responsible for confirming first (see [`Self::history_clear_confirm`]).
    pub fn clear_input_history(&mut self) {
        self.input_history.clear();
        self.history_attachments.clear();
        self.history_attachments_order.clear();
        self.history_index = None;
        self.history_clear_confirm = false;
        // The modal stays open after a clear; reset its selection/preview so
        // it re-anchors to the (now empty) list instead of a stale index.
        self.modal_index = 0;
        self.history_scroll = 0;
        self.history_preview = false;
        // Only truncate the real file when disk persistence is enabled — a
        // test invoking the clear action must never wipe the user's history.
        if self.input_history_persist {
            tokio::task::spawn_blocking(|| {
                let _ = muta_persistence::config::Config::clear_history();
            });
        }
    }

    /// Cap on [`App::history_attachments`] — an in-memory cache, so it only
    /// needs to cover the recall window a user can actually walk (a handful
    /// of ↑/↓ presses), not the full 10k-entry disk history. Each entry may
    /// hold multi-megabyte base64 image payloads, so the bound also keeps
    /// the process's memory footprint predictable.
    pub(crate) const HISTORY_ATTACHMENTS_CAP: usize = 32;

    /// Drop the oldest cached attachment entries (FIFO) once the cache
    /// exceeds [`Self::HISTORY_ATTACHMENTS_CAP`]. `history_attachments_order`
    /// records first-seen order; a re-sent identity keeps its original slot.
    fn prune_history_attachments(&mut self) {
        while self.history_attachments.len() > Self::HISTORY_ATTACHMENTS_CAP {
            let Some(key) = self.history_attachments_order.pop_front() else {
                break;
            };
            self.history_attachments.remove(&key);
        }
    }

    /// Restore the attachments cached behind the history entry at
    /// `orig_idx` (an index into [`App::input_history`], as returned by
    /// `current_session_history` / `history_rows`) into the composer's
    /// `pending_images` / `pending_text_pastes`, or clear them when the
    /// entry has no cache (e.g. loaded from disk before this process
    /// recorded it). The recalled input text already carries the matching
    /// `[Image #N …]` / `[Pasted text #N …]` chips, so staging the payloads
    /// is all that is needed to re-arm a resend.
    pub fn restore_history_attachments(&mut self, orig_idx: usize) {
        let Some(entry) = self.history_entry(orig_idx) else {
            return;
        };
        let identity = (entry.text.clone(), entry.session_id.clone());
        match self.history_attachments.get(&identity) {
            Some(attachments) => {
                self.pending_images = attachments.images.clone();
                self.pending_text_pastes = attachments.text_pastes.clone();
            }
            None => {
                // No cached payloads: a fresh send must not inherit
                // attachments staged for some other entry, so clear them.
                self.pending_images.clear();
                self.pending_text_pastes.clear();
            }
        }
    }

    /// Load the history entry at `orig_idx` (an index from
    /// [`Self::current_session_history`] — spanning the persisted store and
    /// the session backfill) into the composer: its text, its cached
    /// attachments, cursor at the end, completion popup latched closed.
    /// Shared by the ↑/↓ walk and Ctrl+R insert so every recall path stays
    /// identical on the details.
    fn load_history_row(&mut self, orig_idx: usize) {
        let Some(entry) = self.history_entry(orig_idx) else {
            return;
        };
        self.input = entry.text.clone();
        self.set_cursor_end();
        self.restore_history_attachments(orig_idx);
        // History navigation is a programmatic input replacement, not an
        // edit — so it latches `completion_dismissed` like a slash-command
        // accept rather than re-enabling the popup the way InsertChar /
        // Backspace do. This keeps a recalled slash command from flashing
        // its completion menu until the next real keystroke clears the latch.
        self.suggestion_index = None;
        self.completion_dismissed = true;
    }

    /// Advance the inline ↑/↓ history cursor one step toward **older**
    /// entries (the ↑ key). `session_rows` is the newest-first index slice
    /// from [`App::current_session_history`], so position 0 is the newest
    /// entry and larger positions are older.
    ///
    /// The first ↑ stashes the in-progress draft — text and any staged
    /// attachments together — so a later ↓ past the newest entry restores
    /// it instead of leaving the composer empty. Subsequent ↑ walk further
    /// back and clamp at the oldest entry. Returns `true` when a row was
    /// loaded; `false` when the slice is empty.
    pub fn history_prev(&mut self, session_rows: &[usize]) -> bool {
        if session_rows.is_empty() {
            return false;
        }
        let new_pos = match self.history_index {
            Some(p) => (p + 1).min(session_rows.len() - 1),
            None => {
                // First ↑: stash the in-progress draft (and its staged
                // attachments) so a later ↓ past the newest entry restores
                // it instead of leaving the composer empty.
                self.history_draft = std::mem::take(&mut self.input);
                self.history_draft_images = std::mem::take(&mut self.pending_images);
                self.history_draft_text_pastes = std::mem::take(&mut self.pending_text_pastes);
                0
            }
        };
        self.history_index = Some(new_pos);
        self.load_history_row(session_rows[new_pos]);
        true
    }

    /// Move the inline history cursor one step toward **newer** entries
    /// (the ↓ key), mirroring [`App::history_prev`]. Walking past the
    /// newest entry (position 0) restores the draft stashed on the first ↑
    /// — text and attachments together. Returns `true` when a row was
    /// loaded; `false` when the cursor is already at the newest edge (or
    /// was never armed), in which case the draft has been restored.
    pub fn history_next(&mut self, session_rows: &[usize]) -> bool {
        let Some(pos) = self.history_index else {
            return false;
        };
        if pos == 0 {
            // Walked back to the newest entry: restore the draft the user
            // was composing before the first ↑ — text and any staged
            // attachments together — rather than blanking the composer.
            self.history_index = None;
            self.input = std::mem::take(&mut self.history_draft);
            self.pending_images = std::mem::take(&mut self.history_draft_images);
            self.pending_text_pastes = std::mem::take(&mut self.history_draft_text_pastes);
            self.set_cursor_end();
            // The restored draft may be a partial slash/path the user was
            // mid-edit on, but it still arrived via navigation rather than
            // a keystroke, so hold the latch until the next edit.
            self.suggestion_index = None;
            self.completion_dismissed = true;
            return false;
        }
        let new_pos = pos - 1;
        self.history_index = Some(new_pos);
        self.load_history_row(session_rows[new_pos]);
        true
    }

    /// Tear down the history modal's borrowed state: hand the parked composer
    /// draft back, drop any filter query, and clear the search/preview
    /// sub-flags. Shared by the Esc (`CloseModal`) and click-outside dismiss
    /// paths so the two can never drift. Does **not** touch `active_modal` —
    /// the caller owns that transition.
    pub fn restore_history_draft(&mut self) {
        self.input = std::mem::take(&mut self.injection_stashed_input);
        self.set_cursor_end();
        self.input_scroll = 0;
        self.suggestion_index = None;
        self.modal_index = 0;
        self.history_search = false;
        self.history_preview = false;
        self.modal_keymap_open = false;
    }

    /// Tear down the model picker's borrowed state: hand the parked composer
    /// draft back, drop any filter query, and clear the search/scroll sub-flags.
    /// Shared by the Esc (`CloseModal`), click-outside dismiss, and activation
    /// paths so they can never drift. Mirrors [`Self::restore_history_draft`];
    /// does **not** touch `active_modal` — the caller owns that transition.
    pub fn restore_model_draft(&mut self) {
        self.input = std::mem::take(&mut self.injection_stashed_input);
        self.set_cursor_end();
        self.input_scroll = 0;
        self.suggestion_index = None;
        self.modal_index = 0;
        self.model_search = false;
        self.model_scroll = 0;
        self.model_modal_follow = true;
        self.modal_keymap_open = false;
    }

    /// Open the provider-template chooser — the "＋ Add connection" entry point.
    /// The chat draft is already parked in `stashed_input` (the Connections list
    /// stashed it on open); the chooser is a pure list, so the composer line
    /// stays clear.
    pub fn open_provider_template_chooser(&mut self) {
        if self.active_view() == Some(crate::views::ViewId::Connections) {
            self.push_transient_surface(Modal::ProviderTemplate);
        } else {
            self.replace_transient_surface(Modal::ProviderTemplate);
        }
        self.template_choice = 0;
        self.template_scroll = 0;
        self.input.clear();
        self.set_cursor(0);
    }

    /// Move the template-chooser selection, wrapping at the ends.
    pub fn move_template_choice(&mut self, forward: bool) {
        let n = crate::PROVIDER_TEMPLATES.len();
        if n == 0 {
            return;
        }
        self.template_choice = if forward {
            (self.template_choice + 1) % n
        } else {
            (self.template_choice + n - 1) % n
        };
    }

    /// Seed create-mode buffers from `template` without opening the editor.
    pub fn seed_custom_provider_from_template(&mut self, template: &ProviderTemplate) {
        self.custom_edit_id = None;
        self.custom_fields = template.fields();
        self.custom_field = 0;
        self.custom_protocol_wire = template.protocol.to_string();
        self.custom_models = template.models.iter().map(|m| m.to_string()).collect();
        self.custom_url_hint = template.url_hint.to_string();
        self.custom_user_agent = template.user_agent.map(str::to_string);
        self.custom_auth = template.auth;
        self.custom_template_id = Some(template.id.to_string());
        self.custom_suggest_index = 0;
        self.custom_name.clear();
        self.custom_base_url = template.default_url.map(str::to_string).unwrap_or_default();
        self.custom_token.clear();
        self.custom_model = self
            .custom_model_candidates()
            .first()
            .map(|m| m.to_string())
            .unwrap_or_default();
    }

    /// Open the provider editor seeded from `template` (create mode) on the Name
    /// field. The composer line is borrowed for the focused Name field.
    pub fn open_custom_provider_editor(&mut self, template: &ProviderTemplate) {
        self.seed_custom_provider_from_template(template);
        self.replace_transient_surface(Modal::CustomProvider);
        self.input.clear();
        self.set_cursor(0);
    }

    /// Open the OAuth waiting sheet and seed create buffers from `template`.
    pub fn begin_oauth_add(&mut self, template: &ProviderTemplate) {
        self.seed_custom_provider_from_template(template);
        self.awaiting_oauth_add = true;
        // The default message mirrors the provider's default login method: the
        // device flow (Copilot/xAI/ChatGPT default) prints a URL + user code,
        // while the browser flow opens a loopback callback. The auth runner
        // overwrites this with the live URL/code as soon as the device-code
        // request returns.
        self.oauth_pending_message = match template.auth.default_login_method() {
            Some(muta_contracts::LoginMethod::Device) => "Requesting device code…".to_string(),
            _ => "Complete authorization in your browser (or open the link below).".to_string(),
        };
        self.oauth_pending_url.clear();
        self.oauth_pending_user_code.clear();
        self.oauth_pending_error = None;
        self.oauth_scroll = 0;
        self.replace_transient_surface(Modal::OauthPending);
        self.input.clear();
        self.set_cursor(0);
    }

    /// After OAuth succeeds: name-only editor (default name derived from the
    /// in-flight auth — "xAI" for SuperGrok, "ChatGPT" for the ChatGPT plan).
    pub fn open_oauth_instance_name_editor(&mut self) {
        self.awaiting_oauth_add = false;
        self.oauth_pending_url.clear();
        self.oauth_pending_user_code.clear();
        self.oauth_pending_message.clear();
        self.oauth_pending_error = None;
        self.oauth_scroll = 0;
        self.replace_transient_surface(Modal::CustomProvider);
        self.custom_fields = vec![CustomField::Name];
        self.custom_field = 0;
        self.custom_edit_id = None;
        let default_name = match self.custom_auth {
            muta_contracts::ChannelAuth::ChatGptOAuth => "ChatGPT",
            muta_contracts::ChannelAuth::CopilotOAuth => "Copilot",
            muta_contracts::ChannelAuth::AntigravityOAuth => "Google Antigravity",
            _ => "xAI",
        };
        self.custom_name = default_name.to_string();
        self.input = default_name.to_string();
        self.set_cursor_end();
    }

    /// Return which OAuth target is currently selected.
    pub fn oauth_selected_target(&self) -> crate::input::OauthCopyTarget {
        if self.oauth_selected_item == 1 && !self.oauth_pending_user_code.is_empty() {
            crate::input::OauthCopyTarget::UserCode
        } else {
            crate::input::OauthCopyTarget::Url
        }
    }

    /// Cycle selection between URL (0) and Code (1) in OAuth Pending sheet.
    pub fn cycle_oauth_selection(&mut self) {
        if !self.oauth_pending_user_code.is_empty() {
            self.oauth_selected_item = if self.oauth_selected_item == 0 { 1 } else { 0 };
        } else {
            self.oauth_selected_item = 0;
        }
    }

    /// Auth mode of a provider picker row (for OAuth re-connect routing).
    pub fn provider_row_auth(&self, id: &str) -> muta_contracts::ChannelAuth {
        self.provider_picker
            .rows
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.auth)
            .unwrap_or_default()
    }

    /// Open the provider editor in **edit** mode for an existing user provider,
    /// pre-filling its metadata. The visible fields depend on the channel's auth
    /// type: an API-key channel shows Name / Base URL / Token, while an OAuth
    /// channel (ChatGPT/Codex, xAI) shows Name only — its endpoint and token are
    /// owned by the auth flow and must not be hand-edited. The Model field is
    /// always hidden (models are managed in the Models picker).
    pub fn open_edit_provider_editor(
        &mut self,
        id: String,
        name: String,
        protocol: String,
        base_url: String,
        auth: ChannelAuth,
    ) {
        if self.active_view() == Some(crate::views::ViewId::Connections) {
            self.push_transient_surface(Modal::CustomProvider);
        } else {
            self.replace_transient_surface(Modal::CustomProvider);
        }
        self.custom_edit_id = Some(id);
        self.custom_fields = edit_fields(&protocol, auth);
        self.custom_field = 0;
        self.custom_protocol_wire = protocol;
        self.custom_models.clear();
        self.custom_url_hint.clear();
        self.custom_user_agent = None;
        self.custom_auth = auth;
        // Edit mode never carries a template id: edits to an existing provider
        // are sent as `EditProvider`, which ignores template_id anyway, and a
        // stray id here must not leak into a later create flow.
        self.custom_template_id = None;
        self.custom_suggest_index = 0;
        self.custom_name = name.clone();
        self.custom_base_url = base_url;
        self.custom_token.clear();
        self.custom_model.clear();
        self.input = name;
        self.set_cursor_end();
    }

    /// Whether the provider editor is editing an existing provider.
    pub fn custom_is_editing(&self) -> bool {
        self.custom_edit_id.is_some()
    }

    /// The currently focused editor field, or `None` when no editor is open.
    pub fn current_custom_field(&self) -> Option<CustomField> {
        self.custom_fields.get(self.custom_field as usize).copied()
    }

    /// Number of visible fields the editor exposes for the active template.
    fn custom_field_count(&self) -> u8 {
        self.custom_fields.len().max(1) as u8
    }

    /// The registry model ids matching the editor's protocol wire format — the
    /// Model filter field's candidate pool.
    pub fn custom_model_candidates(&self) -> Vec<&'static str> {
        crate::protocol_model_candidates(&self.custom_protocol_wire)
    }

    /// The model suggestions matching the live filter (`self.input` while the
    /// Model field is focused): protocol candidates that fuzzy-match, plus the
    /// raw typed text as a custom id when it is not already a candidate.
    pub fn custom_model_suggestions(&self) -> Vec<String> {
        let q = self.input.trim();
        let q_clean = muta_contracts::sanitize_model_id(q);
        let mut out: Vec<String> = self
            .custom_model_candidates()
            .into_iter()
            .filter(|id| {
                q.is_empty()
                    || id.contains(q)
                    || (!q_clean.is_empty() && id.contains(&q_clean))
                    || crate::fuzzy::fuzzy_match(id, q).is_some()
                    || (!q_clean.is_empty() && crate::fuzzy::fuzzy_match(id, &q_clean).is_some())
            })
            .map(|s| s.to_string())
            .collect();
        let custom_id = if !q_clean.is_empty() {
            q_clean
        } else {
            q.to_string()
        };
        if !custom_id.is_empty() && !out.iter().any(|m| m == &custom_id) {
            out.push(custom_id.clone());
        }
        // Stable sort: an exact match floats to the top so typing a known id
        // selects it rather than a longer id that merely contains it as a
        // substring (e.g. "gpt-4o" beats "gpt-4o-mini").
        if !custom_id.is_empty() {
            out.sort_by_key(|m| m != &custom_id && m != q);
        }
        out
    }

    /// Commit the highlighted Model suggestion into `custom_model`. No-op off the
    /// Model field (the only filter field).
    fn commit_custom_suggestion(&mut self) {
        if self.current_custom_field() == Some(CustomField::Model) {
            let suggestions = self.custom_model_suggestions();
            if let Some(value) = suggestions.get(self.custom_suggest_index) {
                self.custom_model = value.clone();
            }
        }
    }

    /// Move the Model suggestion highlight, committing the newly-highlighted
    /// suggestion live. When the Model field is NOT focused, scrolls the modal
    /// body instead.
    pub fn move_custom_suggestion(&mut self, forward: bool) {
        if self.current_custom_field() == Some(CustomField::Model) {
            let len = self.custom_model_suggestions().len();
            if len == 0 {
                return;
            }
            self.custom_suggest_index = if forward {
                (self.custom_suggest_index + 1) % len
            } else {
                (self.custom_suggest_index + len - 1) % len
            };
            self.commit_custom_suggestion();
        } else {
            // Non-Model fields: ↑/↓ scroll the modal body.
            if forward {
                self.custom_scroll = self.custom_scroll.saturating_add(1);
            } else {
                self.custom_scroll = self.custom_scroll.saturating_sub(1);
            }
        }
    }

    /// React to a change in the Model filter query: reset the highlight to the
    /// best (first) match and commit it.
    pub fn on_custom_filter_changed(&mut self) {
        if self.current_custom_field() == Some(CustomField::Model) {
            self.custom_suggest_index = 0;
            self.commit_custom_suggestion();
        }
    }

    /// Save the composer line into the focused text field's buffer (Name / Base
    /// URL / Token). The Model field is a filter whose value is already committed
    /// live, so its transient query is discarded.
    pub fn stash_custom_field(&mut self) {
        let value = std::mem::take(&mut self.input);
        match self.current_custom_field() {
            Some(CustomField::Name) => self.custom_name = value,
            Some(CustomField::BaseUrl) => self.custom_base_url = value,
            Some(CustomField::Token) => self.custom_token = value,
            _ => {} // Model filter field: value already committed live.
        }
    }

    /// Load the focused field into the composer line: the buffer for a text
    /// field, or a fresh (empty) filter for the Model field, with the suggestion
    /// highlight positioned on the current committed value.
    pub fn load_custom_field(&mut self) {
        self.input = match self.current_custom_field() {
            Some(CustomField::Name) => self.custom_name.clone(),
            Some(CustomField::BaseUrl) => self.custom_base_url.clone(),
            Some(CustomField::Token) => self.custom_token.clone(),
            _ => String::new(),
        };
        self.set_cursor_end();
        if self.current_custom_field() == Some(CustomField::Model) {
            self.custom_suggest_index = self
                .custom_model_suggestions()
                .iter()
                .position(|v| v == &self.custom_model)
                .unwrap_or(0);
        }
    }

    /// Move the provider editor focus (`Tab` / `BackTab`), wrapping across the
    /// active template's visible fields.
    pub fn cycle_custom_field(&mut self, forward: bool) {
        self.stash_custom_field();
        let n = self.custom_field_count();
        self.custom_field = if forward {
            (self.custom_field + 1) % n
        } else {
            (self.custom_field + n - 1) % n
        };
        self.load_custom_field();
    }

    /// Park the composer draft into `stashed_input` and clear the live line so
    /// the input-injection modal (L3.5 β) can borrow it for free-text entry.
    /// Mirrors the stash half of the provider/history pickers.
    pub fn park_input_draft(&mut self) {
        self.injection_stashed_input = std::mem::take(&mut self.input);
        self.set_cursor(0);
        self.input_scroll = 0;
        self.suggestion_index = None;
    }

    /// Tear down the input-injection modal's borrowed state: hand the parked
    /// composer draft back. Does **not** touch `active_modal`.
    pub fn restore_input_draft(&mut self) {
        self.input = std::mem::take(&mut self.injection_stashed_input);
        self.set_cursor_end();
        self.input_scroll = 0;
        self.suggestion_index = None;
        self.modal_index = 0;
    }

    /// The active fuzzy query for the picker: the borrowed composer line while
    /// the search sub-layer is active, else empty (browse mode shows every row).
    fn picker_query(&self) -> &str {
        if self.model_search {
            self.input.trim()
        } else {
            ""
        }
    }

    /// Compute the **Connections** provider rows. Delegates to
    /// `providers_filtered_from` so the input handler and the renderer share
    /// one filter+sort implementation.
    pub fn providers_filtered(&self) -> Vec<RankedProvider> {
        providers_filtered_from(&self.provider_picker, self.picker_query())
    }

    /// Compute the **flat Models** rows — every (provider, model) pair in the
    /// snapshot, filtered by the current picker query. Delegates to
    /// `models_flat_filtered_from` so the input handler and the renderer
    /// share one filter+sort implementation.
    pub fn models_flat_filtered(&self) -> Vec<RankedModel> {
        models_flat_filtered_from(
            &self.provider_picker,
            &self.current_provider,
            &self.current_model,
            self.picker_query(),
        )
    }

    /// Whether the provider with this snapshot id is user-defined (not a
    /// built-in preset). Drives the Connections `e`/`Shift+D` routing and the
    /// Models `d` (remove-model) gate.
    pub fn provider_is_custom(&self, id: &str) -> bool {
        self.provider_picker
            .rows
            .iter()
            .find(|row| row.id == id)
            .map(|row| !row.builtin)
            .unwrap_or(false)
    }

    /// Number of selectable rows in the active picker. Connections counts only
    /// the provider rows (adding a connection is a footer shortcut now, not a
    /// synthetic list row); Models counts the flat (provider, model) rows. Used
    /// to clamp the ↑/↓ selection cursor. Returns 0 when no picker is open.
    pub fn picker_row_count(&self) -> usize {
        match self.active_modal() {
            Modal::Connections => self.providers_filtered().len(),
            Modal::Models => self.models_flat_filtered().len(),
            _ => 0,
        }
    }

    /// Stage the highlighted custom provider for deletion: open the confirm
    /// overlay ([`App::pending_provider_delete`]) over the Connections list
    /// without destroying anything yet. No-op for built-in providers or when an
    /// overlay is already open (prevents re-staging). Driven by the `Shift+D`
    /// → `DeleteProvider` arm.
    pub fn stage_provider_delete(&mut self) {
        if self.active_modal() != Modal::Connections || self.pending_provider_delete.is_some() {
            return;
        }
        let ranked = self.providers_filtered();
        if let Some(row) = ranked.get(self.modal_index).or_else(|| ranked.first())
            && !row.builtin
        {
            self.pending_provider_delete = Some(row.id.clone());
            self.provider_delete_focus = ProviderDeleteChoice::default();
        }
    }

    /// Confirm the staged deletion: dispatch `AgentRequest::DeleteProvider` for
    /// the staged id and tear the overlay down. Returns `Some(request)` when a
    /// deletion was staged (the harness applies it), `None` when the overlay
    /// was not open. Driven by the overlay's Enter-on-Delete. Decrementing
    /// `modal_index` mirrors the picker's other removal paths so the cursor
    /// lands on a valid row once this row vanishes.
    pub fn confirm_provider_delete(&mut self) -> Option<AgentRequest> {
        let id = self.pending_provider_delete.take()?;
        self.modal_index = self.modal_index.saturating_sub(1);
        self.provider_delete_focus = ProviderDeleteChoice::default();
        Some(AgentRequest::DeleteProvider { id })
    }

    /// Cancel the staged deletion: drop the staged id and return keyboard
    /// focus to the Connections list. The modal itself stays open.
    /// Driven by the overlay's Esc / Ctrl+C / Enter-on-Cancel.
    pub fn cancel_provider_delete(&mut self) {
        self.pending_provider_delete = None;
        self.provider_delete_focus = ProviderDeleteChoice::default();
    }

    /// Number of selectable rows in the Tools modal — the tool list, the
    /// only interactive surface. Used to clamp the Up/Down selection cursor.
    pub fn session_tools_len(&self) -> usize {
        self.session_context
            .as_ref()
            .map(|s| s.tools.len())
            .unwrap_or(0)
    }

    /// Build the mutation request implied by toggling the selected tool in the
    /// Tools modal, or `None` when there is no snapshot or the selection
    /// is out of range. The harness applies it and replies with a fresh
    /// snapshot that re-renders the modal.
    pub fn session_activate_request(&self) -> Option<AgentRequest> {
        let tool = self.session_context.as_ref()?.tools.get(self.modal_index)?;
        Some(AgentRequest::ToggleTool {
            name: tool.name.clone(),
            enabled: !tool.enabled,
        })
    }
}

#[cfg(test)]
mod displayed_input_tests {
    //! Behavior locks for `App::displayed_input_with_cursor` — the single
    //! pairing of displayed text and caret byte offset. The renderer and the
    //! geometry probe both resolve through it, so a masked state must never
    //! pair the `•` string with the *unmasked* buffer's byte offset.

    use super::*;

    fn app_with(modal: Modal, input: &str, cursor: usize) -> App {
        let mut app = crate::tests::new_app_for_relay_tests();
        app.set_active_modal_for_test(modal);
        app.input = input.to_string();
        app.editor_field = 0;
        app.set_cursor(cursor);
        app
    }

    #[test]
    fn unmasked_state_returns_raw_buffer_pair() {
        let app = app_with(Modal::None, "héllo", 3); // é is 2 bytes
        let (text, cursor) = app.displayed_input_with_cursor().unwrap();
        assert_eq!(text, "héllo");
        assert_eq!(cursor, app.byte_cursor());
    }

    #[test]
    fn masked_state_pairs_mask_text_with_masked_offset() {
        // 5 chars, 3 of them multi-byte in the raw buffer; the mask is 5 ×
        // `•` (3 bytes each) and the caret at char 3 must be byte 3×3 = 9 —
        // NOT the raw buffer's byte offset (which would land mid-`•`).
        let app = app_with(Modal::ModelEditor, "a中b文c", 3);
        let (text, cursor) = app.displayed_input_with_cursor().unwrap();
        assert_eq!(text.chars().count(), 5);
        assert!(
            text.chars().all(|c| c == '•'),
            "text is fully masked: {text:?}"
        );
        assert_eq!(cursor, 9, "caret offset is measured in mask bytes");
        // The offset must be a char boundary of the masked string.
        assert!(
            text.is_char_boundary(cursor),
            "offset lands on a mask boundary"
        );
    }

    #[test]
    fn masked_state_caret_at_end_maps_to_mask_end() {
        let app = app_with(Modal::ModelEditor, "abc", 3);
        let (text, cursor) = app.displayed_input_with_cursor().unwrap();
        assert_eq!(cursor, text.len(), "end caret maps to the mask's end");
    }

    #[test]
    fn masked_state_caret_past_end_clamps() {
        // A stale cursor beyond the buffer (defensive) must clamp, not
        // produce an out-of-bounds offset.
        let mut app = app_with(Modal::ModelEditor, "abc", 3);
        app.cursor_position = 10;
        let (text, cursor) = app.displayed_input_with_cursor().unwrap();
        assert_eq!(cursor, text.len(), "clamped to the mask's end");
    }
}
