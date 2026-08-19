//! Transcript-area renderer: draws the transcript (and footer chrome) into the
//! neenee-tui-engine grid while recording semantic-to-screen layout
//! information. This is the entry point the app drives each frame
//! ([`draw_transcript`] / [`TranscriptView`]); it also re-exports the drawing
//! surface (chrome, composer, overlays, theme, …) the shell consumes.

pub use crate::chrome::{
    HintBarView, QueueBarView, QueueItemView, draw_completion_menu, draw_hint_bar, draw_queue_bar,
};
pub use crate::chrome::{draw_activity_bar, draw_todo_bar};
pub use crate::composer::{
    INPUT_MSG_IDX, cursor_screen_pos, draw_composer, draw_composer_highlighted,
    draw_composer_igniting,
};
// Design tokens are re-exported crate-visibility so the drawing leaves that
// used to reach them via the old `paint` parent's namespace still resolve.
pub(crate) use crate::design::{
    ACTIVITY_BAR_ROWS, BASH_FOLD_HEAD_ROWS, BASH_FOLD_TAIL_ROWS, CODE_BAND_GUTTER_GAP,
    CODE_BAND_GUTTER_MIN_WIDTH, COMPOSER_MAX_HEIGHT_DIVISOR, COMPOSER_MIN_HEIGHT,
    COMPOSER_PROMPT_PREFIX_COLS, COMPOSER_RIGHT_PAD_COLS, COMPOSER_VERTICAL_CHROME_ROWS,
    ENVOY_FOOTER_ROWS, FOOTER_H_INSET, FOOTER_TOP_GAP_ROWS, HINT_BAR_ROWS, MIN_TERMINAL_COLS,
    MIN_TERMINAL_ROWS, PAGE_HEADER_ROWS, QUEUE_BAR_ROWS, REASONING_TRACE_BLOCK_GAP_ROWS,
    REASONING_TRACE_BODY_TOP_GAP_ROWS, STEP_MIN_WIDTH, TODO_BAR_ROWS, TOOL_STEP_BODY_INDENT_COLS,
    TOOL_STEP_BODY_TOP_GAP_ROWS, TOOL_STEP_CHILDREN_GAP_ROWS, TRANSCRIPT_BODY_LEADING_INDENT,
    TRANSCRIPT_H_INSET,
};
use crate::disclosure::{StickyStep, draw_sticky_summary_if_needed};
/// Which guidance copy the empty-state hero shows beneath the logo (ADR-0057).
/// Re-exported so the app shell selects the variant and the renderer paints it.
pub use crate::empty_state::EmptyStateGuidance;
/// Parse a raw logo file into clamped display lines for the empty-state hero.
/// Re-exported so the startup loader and the renderer share one clamp rule.
pub use crate::empty_state::parse_logo;
use crate::footer_stack;
pub(crate) use crate::footer_stack::{
    FooterRow, FooterRowId, PlacedFooter, rect_of as footer_rect,
};
/// Transcript arrangement strategy (`turn_band`).
pub(crate) use crate::layout;
#[cfg(test)]
use crate::markdown_table::{build_table_render, shrink_column_widths};
pub use crate::overlays::provider_delete_confirm::ProviderDeleteChoice as ProviderDeleteChoiceView;
#[allow(unused_imports)]
pub use crate::overlays::{
    ActivityModalView, BtwModalView, ConfigFocus, ConfigViewProps, ContextUsageView,
    CustomEditorView, HelpBinding, QueueModalView, draw_activity_modal, draw_armed_toast,
    draw_btw_modal, draw_config_view, draw_connections_modal, draw_copy_toast,
    draw_custom_provider_editor, draw_dashboard, draw_help_modal, draw_history_panel,
    draw_input_injection, draw_mcp_modal, draw_model_editor, draw_models_modal, draw_notice_toast,
    draw_oauth_pending, draw_permission_sheet, draw_permissions_manager,
    draw_provider_delete_confirm, draw_provider_template_chooser, draw_question_modal,
    draw_queue_modal, draw_session_preview, draw_sessions_modal, draw_skills_modal,
    draw_token_report_modal, draw_tools_modal, token_report_round_count,
};
use crate::page_header;
pub(crate) use crate::page_header::{
    AsidesChip, BtwHead, PageHeader, PageHints, PageKind, SessionHead, draw_envoy_footer,
    draw_page_header, draw_page_header_hints,
};
pub use crate::primitives::recess_backdrop;
use crate::primitives::{VIEWPORT_BOTTOM_MARGIN, viewport_rect};
#[cfg(test)]
use crate::text_layout::WrappedLine;
#[cfg(test)]
use crate::text_layout::{block_selection_range, line_selection};
pub use crate::theme::{COLOR_SCHEMES, CUSTOM_COLOR_FIELDS, Theme};
#[cfg(test)]
use neenee_tui_engine::text::{prohibited_line_end, prohibited_line_start};
// Re-export the drawing sub-trees so consumers that used to reach them through
// the old `paint` parent can drill in via this module (overlays/tools/…).
pub(crate) use crate::tools;
// Modules referenced by their bare name within this file (formerly declared
// here as submodules; now siblings at the crate root).
use crate::{composer, empty_state};
// Re-exported so layout-strategy code reaches them via the crate-root facade.
pub(crate) use crate::message_body::draw_message_body;
pub(crate) use crate::notice::draw_notice;

use neenee_tui_engine::{
    Alignment, Block as RtBlock, Constraint, Direction, Frame, Layout, Line, Paragraph, Rect, Span,
    Style,
};

use crate::model::document::TranscriptMessage;
use crate::model::layout::{InteractiveTarget, LayoutMap};
use crate::model::selection::{CellDragInfo, SelectionState};
#[cfg(test)]
use neenee_contracts::{PermissionRequest, UserQuestionRequest};

/// Inner rect of a transcript-area region after reserving the uniform
/// [`TRANSCRIPT_H_INSET`] left+right `app_bg` gutters. This is the **single
/// point** where the horizontal inset is applied for the content stream (the
/// `band` every downstream component receives). Individual components no
/// longer clip or hand-pad their own gutter; they trust the rect they
/// receive. The page header is *not* inset here — it spans the terminal's
/// full width and re-applies the inset as text padding inside
/// `draw_page_header`.
pub(crate) fn transcript_band_rect(area: Rect) -> Rect {
    Rect::new(
        area.x + TRANSCRIPT_H_INSET,
        area.y,
        area.width.saturating_sub(2 * TRANSCRIPT_H_INSET).max(1),
        area.height,
    )
}

/// Draw the "terminal too small" notice centered in `area`. Replaces the whole
/// UI when the terminal is resized below [`MIN_TERMINAL_COLS`] ×
/// [`MIN_TERMINAL_ROWS`]. Renders nothing but the notice so the user knows
/// exactly what to fix instead of seeing a broken/blank screen.
///
/// The message degrades gracefully on very narrow terminals: each line is
/// truncated to the available width so the notice never overflows or wraps
/// into the dimensions it is complaining about.
fn draw_too_small_notice(frame: &mut Frame, area: Rect, theme: &Theme) {
    let title = Span::styled("Terminal too small", Style::default().fg(theme.warn()));
    let detail = Span::styled(
        format!(
            "Please resize to at least {} × {}.",
            MIN_TERMINAL_COLS, MIN_TERMINAL_ROWS
        ),
        Style::default().fg(theme.muted()),
    );

    // Truncate each visible line to the available width so the notice never
    // overflows the very geometry it is complaining about. A degenerate 0-width
    // terminal shows nothing; a 1–2 width terminal shows a sliver.
    let avail = (area.width as usize).max(1);
    let truncate = |span: Span<'_>| -> Span<'_> {
        let total: String = span.content.into_owned();
        if total.chars().count() <= avail {
            return Span::styled(total, span.style);
        }
        let kept: String = total.chars().take(avail).collect();
        Span::styled(kept, span.style)
    };
    let lines: Vec<Line> = vec![
        Line::from(vec![truncate(title)]),
        Line::raw(""),
        Line::from(vec![truncate(detail)]),
    ];

    // Vertically center the block in whatever height is available.
    let slack = area.height.saturating_sub(lines.len() as u16) / 2;
    let para = Paragraph::new(lines).alignment(Alignment::Center);
    frame.render_widget(
        para,
        Rect::new(area.x, area.y + slack, area.width, area.height - slack),
    );
}

pub struct TranscriptView<'a> {
    pub messages: &'a [TranscriptMessage],
    pub scroll: u16,
    pub selection: &'a SelectionState,
    pub cell_selection: Option<&'a CellDragInfo>,
    /// Transient running status shown in a thin bar above the input box.
    /// Empty / "idle" means the status bar is hidden; every other value
    /// (including "responding") keeps the bar up for the full round lifecycle.
    pub activity: &'a str,
    /// Whether a tool permission request is awaiting the user's decision. When
    /// true the activity bar is forced visible even if the loop has gone idle,
    /// and its label reads as a permission state so the live status surface
    /// above the input signals the pending decision (the permission sheet
    /// replaces the input + bars below it; this bar stays on as the one piece
    /// of live status that survives). See ADR-0089 footer chrome.
    pub awaiting_permission: bool,
    /// Animation phase for the breathing dot and status-text shimmer.
    pub spinner_phase: usize,
    /// The current input-box text (masked while the API-key modal is open). The
    /// transcript layout reads this so the input box can grow to fit its wrapped text.
    pub input: &'a str,
    /// Byte offset of the caret inside `input` (mirrors `App::byte_cursor`).
    /// The box grows one extra row when the caret rests past the last wrapped
    /// line (e.g. just after an inserted newline), so its height matches what
    /// [`composer::draw_composer`] actually renders.
    pub byte_cursor: usize,
    /// When true, the hint bar and input box are hidden (overlay modal open).
    pub chrome_hidden: bool,
    /// The persistent one-row outbox summary pinned below the transcript gap.
    /// Its `items` slice is the viewed session's queued dispatches; an empty
    /// slice renders a muted empty state so the bar is always present (the
    /// permanent home for queue affordances).
    pub queue_bar: QueueBarView<'a>,
    /// When set, the view is zoomed into an envoy task: a contextual page
    /// header is rendered and `messages` is the focused task's child stream.
    pub envoy_bar: Option<EnvoyBarInfo>,
    /// When set, the view is inside a `/btw` aside (ADR-0017/0103): the
    /// contextual page header carries the coarse primary-session status on
    /// row 1 and the aside's affordance legend on row 2.
    pub side_banner: Option<page_header::BtwHead>,
    /// Live-asides chip + interruptibility for the header band's row-2
    /// legend (ADR-0103 §3). `None` suppresses the legend entirely (non-app
    /// contexts).
    pub page_hints: Option<page_header::PageHints<'a>>,
    /// Session identity for the Main view's head row: the persistent-id tail
    /// plus the tilde-shortened workspace on the left, and the session mode
    /// (`autopilot`) on the right. `None` only in non-session contexts
    /// (tests/showcase) where no ambient session exists.
    pub session_head: Option<SessionHead<'a>>,
    /// Live unified task list, if any. Surfaced on the todo bar (a one-row
    /// summary: tag · progress · current item); the full per-item breakdown
    /// lives in the Activity modal.
    pub todos: Option<&'a neenee_contracts::TodoList>,
    /// Wall-clock instant the current round started, or `None` between rounds.
    /// Drives the muted `<elapsed>` segment in the activity bar.
    pub round_started_at: Option<std::time::Instant>,
    /// Message index of the step (tool step or reasoning trace) whose header
    /// currently rests under the mouse pointer (inline or sticky pinned), so
    /// the next draw lights it up to the intermediate hover tone as a click
    /// affordance. `None` whenever the pointer is elsewhere or an overlay
    /// modal is open.
    pub hovered_step: Option<usize>,
    /// Keyboard-focused activatable target. When `Some`, the matching step's
    /// summary line is painted with the focus-ring cue (a reversed fg/bg bar)
    /// so keyboard navigation via `Ctrl+↑`/`Ctrl+↓` has a clear, unambiguous
    /// visual indicator that does not compete with the hover/expand luminance
    /// channel. `None` means no step is focused.
    pub focused_target: Option<InteractiveTarget>,
    /// User-supplied ASCII logo lines (from `$XDG_CONFIG_HOME/neenee/logo.txt`)
    /// that replace the built-in wordmark on the empty-state hero. `None` when
    /// no user logo is configured; the hero falls back to the built-in art.
    /// Ignored entirely when the transcript is non-empty.
    pub logo: Option<&'a [String]>,
    /// Which guidance variant the empty-state hero renders beneath the logo
    /// (ADR-0057). The app shell computes this from its onboarding + provider
    /// state; the view layer only paints what it is handed. Ignored entirely
    /// when the transcript is non-empty.
    pub guidance: EmptyStateGuidance,
    /// Carousel page index for the empty-state tour (ADR-0104). The caller
    /// derives it from wall-clock elapsed time (`carousel_page_for`), the
    /// same pattern the breathing indicator uses, so the slide cadence is
    /// independent of draw frequency. Ignored when the transcript is
    /// non-empty or the guidance variant does not rotate.
    pub carousel_index: usize,
    pub theme: &'a Theme,
    /// Which layout strategy to arrange messages with. Selectable via
    /// `[tui] transcript_layout`; defaults to [`layout::Strategy::TurnBand`].
    pub layout: layout::Strategy,
    /// Per-message laid-out height cache (Stage 2). Lets the transcript pass
    /// skip the expensive text-wrapping of messages that are entirely outside
    /// the viewport, turning per-frame layout from O(transcript) into
    /// O(visible). The caller clears it whenever the transcript mutates, so an
    /// entry is only ever read while its message's content is unchanged.
    /// `None` outside the app loop (tests / showcase), where every lookup is a
    /// miss — correct, just unoptimized.
    pub height_cache: Option<&'a mut HeightCache>,
}

/// Caches each transcript message's fully-laid-out height (in rows), keyed by
/// the message's stable [`TranscriptMessage::id`](crate::model::document::TranscriptMessage::id).
///
/// Correctness rests on one invariant, enforced by the caller: the cache is
/// invalidated for every changed message (or cleared for a structural change)
/// and whenever the wrap width changes ([`Self::prepare`]). So a cached height
/// is only ever consulted while the message's content **and** the
/// layout width are identical to when it was measured — making the cached row
/// count exactly reproduce a fresh layout.
#[derive(Default)]
pub struct HeightCache {
    width: u16,
    heights: std::collections::HashMap<u64, u16>,
    virtual_index: Option<layout::VirtualLayoutIndex>,
    /// Width-independent, render-only rows derived from completed edit
    /// patches. Unlike height entries, these survive structural transcript
    /// invalidation and resize; their own source identity controls reuse.
    pub(crate) diff_cache: tools::DiffCache,
}

impl HeightCache {
    /// Reset the cache if the wrap width changed since the last frame; heights
    /// are width-dependent, so a resize invalidates every entry. Call once at
    /// the start of a transcript pass before any [`Self::get`]/[`Self::set`].
    pub fn prepare(&mut self, width: u16) {
        if self.width != width {
            self.heights.clear();
            self.virtual_index = None;
            self.width = width;
        }
    }

    /// The cached height for message `id`, or `None` if it must be measured.
    pub fn get(&self, id: u64) -> Option<u16> {
        self.heights.get(&id).copied()
    }

    /// Record the freshly-measured height for message `id`.
    pub fn set(&mut self, id: u64, height: u16) {
        if self.heights.insert(id, height) != Some(height) {
            self.virtual_index = None;
        }
    }

    /// Drop every entry. Called when the transcript mutates (the version moved).
    pub fn clear(&mut self) {
        self.heights.clear();
        self.virtual_index = None;
    }

    /// Drop only the messages whose wrapped height changed. Streaming modifies
    /// the live tail one message at a time; retaining the frozen history is
    /// what lets a long transcript keep taking the off-screen fast path while
    /// that tail grows.
    pub fn invalidate_messages(&mut self, ids: impl IntoIterator<Item = u64>) {
        let mut changed = false;
        for id in ids {
            changed |= self.heights.remove(&id).is_some();
        }
        if changed {
            self.virtual_index = None;
        }
    }

    /// Return the exact message chunks that intersect the viewport when every
    /// settled message height is known. The index is built once after a stable
    /// transcript is measured; later frames use binary search and only draw
    /// the selected chunks.
    pub fn virtual_window(
        &mut self,
        messages: &[TranscriptMessage],
        strategy: layout::Strategy,
        scroll: usize,
        view_height: u16,
    ) -> Option<layout::VirtualWindow> {
        if self
            .virtual_index
            .as_ref()
            .is_none_or(|index| !index.matches(messages, strategy))
        {
            self.virtual_index = layout::build_virtual_index(messages, self, strategy);
        }
        self.virtual_index
            .as_ref()
            .and_then(|index| index.window(scroll, view_height))
    }
}

/// Page-header context for an Envoy view (shown when zoomed into a task).
pub struct EnvoyBarInfo {
    /// The envoy's role (`explore` / `plan` / …), when the `Started` event
    /// has identified it. Rendered as the `[ROLE]` tag between the `ENVOY`
    /// identity and the title; omitted before the role is known.
    pub role: Option<String>,
    /// Title of the focused envoy (its task description).
    pub label: String,
    /// 1-based index of the focused envoy among its siblings.
    pub index: usize,
    /// Total number of sibling envoy tasks.
    pub total: usize,
}

/// Layout information returned by [`draw_transcript`].
pub struct TranscriptRender {
    /// The input box area.
    pub input_rect: Rect,
    /// The hint-bar area pinned directly below the input box (zero-sized when
    /// hidden).
    pub hint_rect: Rect,
    /// The placed footer stack for this frame — every visible row's rect in
    /// stack order. Hit-test consumers resolve bar rects through
    /// `footer_stack::rect_of` on this registry rather than one bespoke
    /// `Option<Rect>` field per bar (todo/queue/activity click routing,
    /// history-panel height reservation).
    pub footer: PlacedFooter,
    /// Total height (in lines) of the rendered message stream, ignoring the
    /// viewport clip. Used by the app loop to pin the view to the bottom.
    pub content_lines: usize,
    /// Height of the transcript viewport.
    pub view_height: u16,
    /// The expanded step whose body is currently scrolled into view, so the app
    /// can render/click a sticky header pinned under the HUD bar. `None` when no
    /// expanded step body covers the top of the viewport.
    pub sticky: Option<StickyInfo>,
}

/// A sticky pinned step summary (returned to the app for click handling).
pub struct StickyInfo {
    pub message_idx: usize,
    pub rect: Rect,
    /// The content-line index of the real summary inside the stream. The app
    /// uses this to re-anchor the scroll offset when the user collapses the
    /// pinned step, so the real summary takes the sticky's place at the top of
    /// the viewport instead of jumping to unrelated content.
    pub summary_line: usize,
}

/// Draw the main transcript area, recording layout info.
pub fn draw_transcript(
    frame: &mut Frame,
    layout_map: &mut LayoutMap,
    view: TranscriptView<'_>,
) -> TranscriptRender {
    let TranscriptView {
        messages,
        scroll,
        selection,
        cell_selection,
        activity,
        awaiting_permission,
        spinner_phase,
        input,
        byte_cursor,
        chrome_hidden,
        queue_bar,
        envoy_bar,
        side_banner,
        page_hints,
        session_head,
        todos,
        round_started_at,
        hovered_step,
        focused_target,
        logo,
        guidance,
        carousel_index,
        theme,
        layout,
        height_cache,
    } = view;
    // Outside the app loop (tests/showcase) no persistent cache is supplied;
    // fall back to a throwaway so every lookup simply misses and the renderer
    // behaves exactly as before the cache existed.
    let mut fallback_height_cache = HeightCache::default();
    let height_cache = height_cache.unwrap_or(&mut fallback_height_cache);
    let full = frame.area();

    // Paint the entire frame with the app background so the TUI owns every
    // pixel rather than leaving gaps at the terminal emulator's default color.
    frame.render_widget(
        RtBlock::default().style(Style::default().bg(theme.surface())),
        full,
    );

    // ── Too-small terminal guard ──────────────────────────────────────────
    // When the terminal is resized below the usable minimum, the layout math
    // (footer split, composer height, gutter columns) would underflow or
    // produce an unusable UI — and a degenerate 0×0 / 1×1 geometry risks an
    // integer-underflow panic deep in a subtraction chain. Instead of drawing
    // garbage, hide the entire UI and show a single centered notice telling
    // the user how large the terminal must be. The footer chrome is suppressed
    // by returning zero-sized rects, so the app loop renders nothing else.
    if full.width < MIN_TERMINAL_COLS || full.height < MIN_TERMINAL_ROWS {
        draw_too_small_notice(frame, full, theme);
        return TranscriptRender {
            input_rect: Rect::default(),
            hint_rect: Rect::default(),
            footer: PlacedFooter::default(),
            content_lines: 0,
            view_height: 0,
            sticky: None,
        };
    }

    // Resolve every transcript page to one page-header model. The Main
    // session view always carries a head (its ambient session state — id
    // tail, workspace, mode — replaces the old bottom status bar). Envoy
    // and `/btw` keep their contextual headers. Envoy and `/btw` are
    // mutually exclusive in the app; preferring Envoy here is a defensive
    // fallback that keeps rendering deterministic if a malformed caller
    // supplies both.
    let page_header = envoy_bar
        .as_ref()
        .map(PageHeader::Envoy)
        .or_else(|| side_banner.map(PageHeader::Btw))
        .or_else(|| session_head.as_ref().map(PageHeader::Session));
    // The row-2 affordance legend (ADR-0103 §3, demand-gated by ADR-0104).
    // The destructured `page_hints` is pre-resolved by the caller (it needs
    // app-level state — the aside chip and interruptibility — that the view
    // struct carries precisely so this stays allocation-free here). Row 2 is
    // reserved only while the view has page-specific affordances that no
    // other surface already carries; otherwise the band collapses to the
    // single identity row and the transcript reclaims the line.
    let page_hints_view = page_hints.filter(|hints: &PageHints<'_>| hints.has_content());

    // When a head band is present it occupies the top rows of the terminal
    // directly — the head is a sibling of the transcript, not content inside
    // it, so it replaces the viewport's top margin rather than nesting under
    // it. The band is identity/status on row 1 plus — only while the view
    // has page-specific affordances to announce (ADR-0104; see
    // `PageHints::has_content`) — the view-affordance legend on row 2, both
    // carved off with one layout split. Without a head, the standard
    // viewport margins apply. The Envoy page additionally owns the terminal's
    // last rows for its permanent key-legend footer (three background-painted
    // rows whose middle row carries the shortcuts), so the transcript ends
    // above that band.
    let (head_rect, hints_rect, envoy_footer_rect, viewport) = if page_header.is_some() {
        let full = frame.area();
        let footer_rows = if envoy_bar.is_some() {
            ENVOY_FOOTER_ROWS
        } else {
            0
        };
        // The head band's height is demand-driven (ADR-0104): row 2 is
        // reserved only while the view has page-specific affordances.
        // `PAGE_HEADER_ROWS` stays the recorded ceiling.
        let band_rows = (1 + u16::from(page_hints_view.is_some())).min(PAGE_HEADER_ROWS);
        let sub = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(band_rows.saturating_sub(1)),
                Constraint::Min(0),
                Constraint::Length(footer_rows),
            ])
            .split(full);
        (
            // The head band spans the terminal's full width — it is top-level
            // chrome pinned to the top edge, the counterpart of the Envoy
            // key-legend band at the bottom edge, not a transcript-area
            // component. Its *text* keeps the shared horizontal inset (applied
            // inside `draw_page_header` as pad spans) so it stays aligned with
            // the transcript band below.
            Some(sub[0]),
            (band_rows > 1).then_some(sub[1]),
            (footer_rows > 0).then_some(sub[3]),
            // The remaining area keeps the bottom viewport margin (0) but
            // drops the top one (the head owns that row now).
            Rect::new(
                sub[2].x,
                sub[2].y,
                sub[2].width,
                sub[2].height.saturating_sub(VIEWPORT_BOTTOM_MARGIN),
            ),
        )
    } else {
        (None, None, None, viewport_rect(frame))
    };

    let size = viewport;

    // When zoomed into an envoy task, the footer (status bar, plan panel,
    // input box, hint bar) is hidden: the task detail page is a read-only view
    // whose only chrome is its page header.
    let in_envoy = envoy_bar.is_some();

    // The activity bar (animated spinner + activity text) sits directly above the
    // input box, below the ambient todo/queue meta bars. It is shown for every
    // active phase — including streaming ("responding"), which is the longest
    // phase and the one where the breathing dot's liveness signal matters most
    // — and hidden only when the harness is idle, so the row returns to the
    // transcript.
    //
    // A pending permission request is an exception: the permission sheet
    // replaces the input box and the bars beneath it, so the activity bar is
    // the only live status surface left. Force it on so the user always has a
    // visible "awaiting permission" anchor above the sheet — even if the loop
    // has nominally gone idle (e.g. right after an interrupt that rejects
    // permissions but before the stale round's terminal snapshot lands).
    let activity_active = !chrome_hidden
        && !in_envoy
        && (awaiting_permission || (!activity.is_empty() && activity != "idle"));
    // The activity bar is purely transient now: it shows only while a round is
    // active and hides when idle, so the row returns to the transcript (the
    // persistent task-list summary has its own bar above).
    let activity_row_needed = activity_active;
    let activity_height: u16 = if activity_row_needed {
        ACTIVITY_BAR_ROWS
    } else {
        0
    };

    // The todo bar leads the footer stack and surfaces the live task list —
    // a `TODOS d/t` identity and a preview of the current item. It is
    // hidden only while an overlay owns the chrome, inside an envoy zoom, or
    // when the list is empty.
    let has_visible_todos = todos.map(|l| !l.items.is_empty()).unwrap_or(false);
    let todo_row_needed = !chrome_hidden && !in_envoy && has_visible_todos;
    let todo_height: u16 = if todo_row_needed { TODO_BAR_ROWS } else { 0 };

    // The queue bar surfaces pending outbox messages. It is hidden while the
    // viewed session's queue is empty (the common idle case) so an ordinary
    // session reclaims the row; it appears the moment a message is staged
    // and stays up until the outbox drains, so the user always has a glanceable
    // surface while there is pending work.
    let queue_row_needed = !chrome_hidden && !in_envoy && !queue_bar.items.is_empty();
    let queue_height: u16 = if queue_row_needed { QUEUE_BAR_ROWS } else { 0 };

    // The input box grows with its content: the typed text wraps onto new
    // lines and the box expands to fit, up to roughly half the terminal so the
    // transcript history always stays visible. The inner text width reserves the
    // footer insets, the `> ` prompt prefix, and the matching right pad so the
    // height calculation wraps at the same width the composer renders.
    let input_text_width = (size.width as usize)
        .saturating_sub(
            (2 * FOOTER_H_INSET) as usize + COMPOSER_PROMPT_PREFIX_COLS + COMPOSER_RIGHT_PAD_COLS,
        )
        .max(1);
    let input_wrapped_lines = composer::input_row_count(input, input_text_width, byte_cursor);
    let desired_input_height = input_wrapped_lines as u16 + COMPOSER_VERTICAL_CHROME_ROWS;
    let max_input_height = (size.height / COMPOSER_MAX_HEIGHT_DIVISOR).max(COMPOSER_MIN_HEIGHT);
    let input_box_height = if in_envoy {
        0
    } else {
        desired_input_height.min(max_input_height)
    };
    // The hint bar is a single-line status strip pinned flush below the input
    // box — the composer's own bottom panel-bg padding row is all the
    // separation it needs (COMPOSER_HINT_GAP_ROWS = 0). It carries the
    // next Enter action plus ambient model/context info. Hidden alongside the
    // rest of the chrome while an overlay is open.
    let hint_height: u16 = if chrome_hidden || in_envoy {
        0
    } else {
        HINT_BAR_ROWS
    };
    // The composer/hint gap is 0 and the activity/composer gap is 0: the
    // hint bar sits flush against the composer's bottom edge and the activity
    // bar flush against the composer's top edge (each side's panel-bg padding
    // row is the separation). These zero-gap tokens are therefore structural:
    // adjacent rows in the footer stack are flush by construction, and no gap
    // row is ever placed. The tokens stay in `design.rs` as the recorded
    // decision, asserted by the layout tests.
    // The footer stack is declared once, in draw order — the single-pass
    // placer derives both the band's total height (for the layout split) and
    // each row's rect, so the height arithmetic can no longer exist in two
    // copies that drift. Order, top → bottom: gap, todo bar, queue bar,
    // activity bar, input box, hint bar. The ambient meta bars (todo = task
    // list, queue = outbox) lead; the activity bar sits flush above the input
    // box so the live status reads as part of the composer; the hint bar sits
    // flush below it and carries the next input action + model/context.
    // Session-level state (workspace, mode flags such as `autopilot`) lives on
    // the head row at the top of the view, not on a bottom status bar.
    //
    // The zero-gap tokens (activity→composer, composer→hint) collapse to no
    // row at all rather than a zero-height placeholder: the stack lists only
    // rows that exist, and their flush-ness is a property of adjacency.
    let footer_rows: Vec<FooterRow> = if chrome_hidden || in_envoy {
        Vec::new()
    } else {
        vec![
            FooterRow {
                id: FooterRowId::TopGap,
                height: FOOTER_TOP_GAP_ROWS,
            },
            FooterRow {
                id: FooterRowId::Todos,
                height: todo_height,
            },
            FooterRow {
                id: FooterRowId::Queue,
                height: queue_height,
            },
            FooterRow {
                id: FooterRowId::Activity,
                height: activity_height,
            },
            FooterRow {
                id: FooterRowId::Composer,
                height: input_box_height,
            },
            FooterRow {
                id: FooterRowId::Hint,
                height: hint_height,
            },
        ]
    };
    let footer_height: u16 = footer_stack::measure(&footer_rows);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),                // Transcript page
            Constraint::Length(footer_height), // Activity? + input box + hint bar + status bar
        ])
        .split(size);

    // 1. Head band — drawn at the very top of the terminal, before the
    // transcript. The band is a sibling of the transcript, not content inside
    // it, so it was already split from `full` above; just paint it here.
    // Row 1 carries identity/status; row 2 the view-affordance legend.
    if let (Some(header), Some(rect)) = (page_header.as_ref(), head_rect) {
        draw_page_header(frame, rect, header, theme);
    }
    if let (Some(hints), Some(rect)) = (page_hints_view.as_ref(), hints_rect) {
        draw_page_header_hints(frame, rect, hints, theme);
    }

    // 1b. Envoy key-legend footer — pinned to the terminal's last rows (its
    // rect came out of the same layout split as the head). Painted on the
    // page-body background with the shortcuts on its middle row.
    if let (Some(info), Some(rect)) = (envoy_bar.as_ref(), envoy_footer_rect) {
        draw_envoy_footer(frame, rect, info, theme);
    }

    // 2. Transcript History — the transcript area is the whole `chunks[0]`
    // when a head is present (it was already accounted for above), or the
    // entire viewport-minus-footer when there is no head.
    let transcript_area = chunks[0];
    // Apply the uniform horizontal inset (`TRANSCRIPT_H_INSET` on each side)
    // exactly once, here at the transcript-stream entry point. Every
    // downstream component receives `band` — an already-inset rect — so none
    // of them re-clips or hand-pads a leading gutter. The empty-state hero is
    // the sole exception: it centers across the full viewport, so it keeps
    // `transcript_area` (un-inset). The page header is rendered from its own
    // layout-split rect before this point, so it is unaffected.
    let band = transcript_band_rect(transcript_area);
    let mut current_y = band.y;
    // Account for scroll. Owned by the layout `Stream` once the loop runs; not
    // mutated locally here unless a virtual index selects a later chunk.
    let mut skip_rows = scroll as usize;
    // Total stream height, counted independently of the viewport clip so the
    // app loop can follow the bottom.
    let mut content_lines: usize = 0;
    let mut message_start = 0usize;
    let mut message_end = messages.len();
    let mut virtual_total_lines = None;
    // Expanded steps collected during the pass, for the sticky pinned header.
    let mut sticky_steps: Vec<StickyStep> = Vec::new();

    // Empty-state replacement (ADR-0033): when the session has no messages and
    // no envoy/side view is open, the transcript is replaced by a centered
    // logo hero rather than rendering an empty stream. This is a component
    // substitution, not transcript content — the hero never participates in
    // scroll, selection, or attribution, so the whole message-rendering
    // pipeline (loop, badges, sticky pinning) is skipped. The footer below
    // renders exactly as in a live session.
    let show_empty_state = messages.is_empty() && envoy_bar.is_none() && side_banner.is_none();

    if show_empty_state {
        empty_state::draw_empty_state(
            frame,
            transcript_area,
            logo,
            guidance,
            carousel_index,
            theme,
        );
        // Account for the hero so the app loop does not treat the session as a
        // zero-height stream (which would mis-pin the scroll position).
        content_lines = empty_state::empty_state_content_lines(logo, guidance);
    } else {
        // Stage 2: heights are wrap-width-dependent, so drop the cache on a
        // resize. Within a stable width + unchanged transcript every entry
        // reproduces a fresh layout exactly, so off-screen messages can be
        // advanced from their cached height instead of being re-wrapped.
        height_cache.prepare(band.width);

        if let Some(window) =
            height_cache.virtual_window(messages, layout, scroll as usize, band.height)
        {
            message_start = window.message_start;
            message_end = window.message_end;
            content_lines = window.prefix_lines;
            skip_rows = window.skip_rows;
            virtual_total_lines = Some(window.total_lines);
        }

        // Delegate message arrangement to the selected layout strategy. The
        // `Stream` carries every piece of shared render state (scroll/Y
        // accounting, layout map, height cache, theme, hover/focus) and
        // exposes `badge` / `dispatch` / `gap` as the sanctioned mutations, so
        // every layout agrees on scroll semantics. The layout leaves
        // `content_lines`, `sticky_steps`, and `current_y` populated for the
        // post-processing below.
        let mut stream = layout::Stream {
            frame,
            band,
            messages,
            theme,
            layout_map,
            height_cache,
            selection,
            cell_selection,
            hovered_step,
            focused_target,
            message_start,
            message_end,
            virtual_total_lines,
            current_y,
            skip_rows,
            content_lines,
            sticky_steps,
        };
        layout.build().run(&mut stream);
        // Recover the loop state the post-processing below needs.
        // (`skip_rows` is loop-internal and is not read after the layout
        // returns, so it is not recovered.)
        current_y = stream.current_y;
        content_lines = stream.content_lines;
        sticky_steps = stream.sticky_steps;
    } // end else (non-empty transcript branch)

    // Record the visible transcript content rect so clicks on gap rows
    // (which carry no registered region) still switch keyboard focus to
    // Browse. The rect spans the horizontal band inside the gutters —
    // matching the user's mental model that the outer gutters are not
    // transcript clicks — and the rows where content was actually drawn,
    // clamped to the viewport so empty space below the last message stays
    // inert. `current_y` already stops advancing once it reaches the
    // viewport bottom, so this is a faithful bound on visible content.
    // Skipped for the empty-state hero, which owns its own rect and is not
    // part of the interactive transcript surface.
    if !show_empty_state {
        let content_bottom = current_y.min(band.y + band.height);
        if content_bottom > band.y {
            layout_map.set_transcript_content_rect(Rect::new(
                band.x,
                band.y,
                band.width,
                content_bottom - band.y,
            ));
        }
    }

    // The footer stacks, from top to bottom: a permanent blank separator,
    // the persistent todo bar (when the task list is non-empty), the
    // persistent queue bar (when the outbox is non-empty), the transient
    // activity bar (when active), the input box, and the hint bar. The
    // activity bar sits flush above the input box so the live "what the
    // agent is doing right now" status reads as part of the composer cluster;
    // the todo and queue bars are ambient meta-info and float above it. The
    // separator keeps the latest response visually distinct from the controls
    // even when the activity row appears or disappears. The activity bar
    // doubles as the click target that opens the Activity modal (the
    // pursuit and plan summaries that used to live here now scroll inside that
    // modal and as inline notices in the transcript).
    // One `place` pass walks the declared stack and yields every row's rect
    // plus the hit-test registry; each draw call below just looks its own
    // rect up. The height sum (which feeds the layout split) and the per-row
    // offsets can no longer drift apart — they are the same traversal now.
    // `place` applies the shared `FOOTER_H_INSET` extent itself, so no
    // hand-derived `footer_x`/`footer_w` remains.
    let placed_footer = footer_stack::place(chunks[1], &footer_rows);

    // The persistent todo bar leads the footer stack. It surfaces the live task
    // list — the `TODOS d/t` identity and a preview of the current item — and
    // is the click target that opens the Activity modal on the Todos section
    // (the event loop resolves the click from the placed registry).
    footer_stack::rect_of(&placed_footer, FooterRowId::Todos)
        .filter(|_| todo_row_needed)
        .and_then(|rect| todos.map(|list| draw_todo_bar(frame, rect, list, theme)));

    // The persistent queue bar sits directly below the todo bar. It is a
    // stable one-row outbox summary so pending messages never have to be
    // inferred from the hint bar. The whole bar is the click target that
    // expands the full Queue modal.
    footer_stack::rect_of(&placed_footer, FooterRowId::Queue)
        .filter(|_| queue_row_needed)
        .map(|rect| draw_queue_bar(frame, rect, queue_bar, theme));

    // The transient activity bar sits directly above the input box so the live
    // "what the agent is doing right now" status reads as part of the composer
    // cluster rather than floating above the ambient meta bars. It stays up for
    // the entire active round lifecycle (queued → responding → tool work →
    // finalizing), including the streaming phase, and hides only when idle.
    // Keeping it up during "responding" avoids a layout shift at the stream
    // boundary and sustains the breathing-dot liveness anchor (ADR-0008)
    // through the longest phase. The bar is the click target that opens the
    // Activity modal.
    footer_stack::rect_of(&placed_footer, FooterRowId::Activity)
        .filter(|_| activity_row_needed)
        .and_then(|rect| {
            draw_activity_bar(
                frame,
                rect,
                round_started_at,
                activity,
                awaiting_permission,
                spinner_phase,
                theme,
            )
        });

    // The input box sits flush directly below the activity bar — the
    // composer's top panel-bg padding row already separates its text from
    // the live status line, so no `surface` gap row is reserved between them.
    let input_rect = footer_stack::rect_of(&placed_footer, FooterRowId::Composer)
        .filter(|_| input_box_height > 0)
        .unwrap_or_default();

    // The hint bar sits directly below the input box, carrying the input action
    // plus ambient model/context info. Its rect is computed even though
    // its draw call is delegated to the app loop (which owns the masked input
    // state and the context-token source).
    let hint_rect = footer_stack::rect_of(&placed_footer, FooterRowId::Hint)
        .filter(|_| hint_height > 0)
        .unwrap_or_default();

    // Sticky pinned summary: if an expanded step's body covers the top of the
    // viewport (its summary is scrolled out of view), pin its summary to the
    // line directly under the HUD bar so the user can always collapse it.
    let sticky_info = draw_sticky_summary_if_needed(frame, band, &sticky_steps, scroll, theme);

    TranscriptRender {
        input_rect,
        hint_rect,
        footer: placed_footer,
        content_lines,
        view_height: transcript_area.height,
        sticky: sticky_info,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_layout::wrap_text;
    use unicode_width::UnicodeWidthStr;

    /// Smoke-render every redesigned component into a buffer to catch panics
    /// (border math, rect underflows, empty content) without a live terminal.
    #[test]
    fn redesigned_components_render_without_panicking() {
        let theme = Theme::default();
        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 30);

        terminal
            .draw(|f| {
                let mut layout_map = LayoutMap::new();
                let mut thinking = TranscriptMessage::thinking("Reasoning about the task step by step.");
                thinking.set_thinking_expanded(true);
                let mut tool = TranscriptMessage::tool_step("call_1", "list_dir", r#"{"path":"."}"#);
                tool.set_tool_step_expanded(true);
                tool.finish_tool_step("call_1", "file_a\nfile_b", neenee_contracts::ToolOutput::text("file_a\nfile_b"), 12);
                let messages = vec![
                    TranscriptMessage::new(neenee_contracts::Role::User, "hi"),
                    TranscriptMessage::new(
                        neenee_contracts::Role::Assistant,
                        "Here is a table:\n\n| Tool | Count |\n| --- | ---: |\n| read | 1 |\n| webfetch | 250 |",
                    ),
                    thinking,
                    tool,
                ];
                let _ = draw_transcript(
                    f,
                    &mut layout_map,
                    TranscriptView {
                        messages: &messages,
                        scroll: 0,
                        selection: &SelectionState::None,
                        cell_selection: None,
                        activity: "waiting for model",
                        awaiting_permission: false,                        spinner_phase: 0,
                        input: "hello",
                        byte_cursor: 5,
                        chrome_hidden: false,
                        queue_bar: QueueBarView {
                            items: &[],
                            paused: false,
                            blocked: false,
                        },
                        envoy_bar: None,
                        side_banner: None,
                        page_hints: None,
                    session_head: None,
                        todos: None,
                                        round_started_at: None,
                        hovered_step: None,
                        focused_target: None,
                        logo: None,
                        guidance: EmptyStateGuidance::Tour,
                        carousel_index: 0,
                        theme: &theme,
                        layout: crate::layout::Strategy::default(),
                        height_cache: None,
                    },
                );
                draw_composer(
                    f,
                    Rect::new(0, 21, 80, 3),
                    "hello",
                    5,
                    true,
                    true,
                    &theme,
                    &mut LayoutMap::new(),
                    true,
                    &mut 0,
                    &SelectionState::None,
                    0,
                    0,
                );
                draw_completion_menu(
                    f,
                    &mut layout_map,
                    &[
                        crate::completion::Completion {
                            label: "/new".to_string(),
                            description: "New".to_string(),
                            replace_start: 0,
                            replace_end: 0,
                            kind: crate::completion::CompletionItemKind::Slash,
                            doc: None,
                        },
                    ],
                    Some(0),
                    Rect::new(0, 20, 80, 3),
                    2,
                    &theme,
                );
                draw_copy_toast(f, "copied to clipboard", false, &theme);
                draw_armed_toast(f, "press Ctrl+C again to exit", &theme);
            });

        // Modals + permission sheet on a fresh frame.
        terminal.draw(|f| {
            draw_connections_modal(
                f,
                &mut LayoutMap::new(),
                &[],
                "mock",
                0,
                "",
                0,
                &mut 0,
                true,
                false,
                false,
                &theme,
            );
            draw_models_modal(
                f,
                &mut LayoutMap::new(),
                &[],
                "mock",
                "mock-model",
                0,
                "",
                0,
                &mut 0,
                true,
                false,
                false,
                &theme,
            );
            let history_roster: Vec<neenee_contracts::HistoryEntry> =
                [neenee_contracts::HistoryEntry::new(
                    "a".to_string(),
                    None,
                    None,
                    0,
                )]
                .into_iter()
                .collect();
            let ranked: Vec<(usize, crate::fuzzy::FuzzyMatch)> = crate::fuzzy::rank(&["a"], "");
            let input_rect = neenee_tui_engine::Rect::new(0, 20, 80, 3);
            let _ = draw_history_panel(
                f,
                &history_roster,
                &ranked,
                0,
                &mut 0,
                true,
                false,
                false,
                input_rect,
                0,
                &theme,
            );
            draw_model_editor(f, "OpenAI", "", 0, true, 0, None, &[], None, &theme);
            // Provider-template chooser.
            let mut template_scroll = 0;
            draw_provider_template_chooser(0, f, &theme, &mut template_scroll);
            // Provider editor on the Model filter field.
            use crate::providers::CustomField;
            let mut scroll = 0;
            draw_custom_provider_editor(
                CustomEditorView {
                    fields: &[
                        CustomField::Name,
                        CustomField::BaseUrl,
                        CustomField::Token,
                        CustomField::Model,
                    ],
                    field: 3,
                    editing: false,
                    title: "Custom OpenAI",
                    name_buf: "My Relay",
                    base_url_buf: "https://relay/v1/chat/completions",
                    token_buf: "tok",
                    model_display: "GPT-4o",
                    url_hint: "https://relay.example.com/v1/chat/completions",
                    suggestions: &["GPT-4o".to_string(), "GPT-4o mini".to_string()],
                    suggest_index: 0,
                    input: "gpt",
                    cursor_position: 3,
                },
                f,
                &theme,
                &mut scroll,
            );
            {
                let mut scroll = 0;
                let bindings: &[HelpBinding] = &[];
                draw_help_modal(f, &mut scroll, bindings, &theme);
            }
            draw_sessions_modal(
                f,
                &[
                    neenee_contracts::SessionOverview {
                        id: "abc123".to_string(),
                        overview: "Refactor the renderer".to_string(),
                        created_at: 0,
                        updated_at: 0,
                        message_count: 12,
                        active: true,
                    },
                    neenee_contracts::SessionOverview {
                        id: "def456".to_string(),
                        overview: "Fix the tool_call_id bug".to_string(),
                        created_at: 0,
                        updated_at: 0,
                        message_count: 4,
                        active: false,
                    },
                ],
                0,
                false,
                &mut scroll,
                true,
                &theme,
                false,
                0,
                false,
                None,
                &mut 0,
            );
            let question_request = UserQuestionRequest {
                id: "q1".to_string(),
                questions: vec![neenee_contracts::UserQuestion {
                    header: Some("Style".to_string()),
                    question: "Which error handling crate?".to_string(),
                    options: vec![
                        neenee_contracts::UserQuestionOption {
                            label: "anyhow (Recommended)".to_string(),
                            description: Some("Simple".to_string()),
                        },
                        neenee_contracts::UserQuestionOption {
                            label: "thiserror".to_string(),
                            description: Some("Structured".to_string()),
                        },
                    ],
                    multi_select: false,
                }],
            };
            let mut hit_map = crate::model::layout::ModalHitMap::new();
            draw_question_modal(
                f,
                &mut hit_map,
                &question_request,
                0,
                &[vec![1]],
                &[String::new()],
                1,
                &mut 0,
                true,
                &theme,
            );
        });

        terminal.draw(|f| {
            let request = PermissionRequest {
                id: "p1".to_string(),
                tool: "bash".to_string(),
                label: "bash".to_string(),
                description: "run a command".to_string(),
                arguments: r#"{"command":"ls"}"#.to_string(),
                scope: "*".to_string(),
                elevation: false,
                one_off: false,
            };
            let rect = neenee_tui_engine::Rect::new(0, 0, 60, 3);
            let mut hit_map = crate::model::layout::ModalHitMap::new();
            let _ =
                draw_permission_sheet(f, &mut hit_map, &request, 0, false, false, 0, rect, &theme);
        });
    }

    #[test]
    fn config_appearance_pages_render_at_minimum_terminal_size() {
        let theme = Theme::default();
        let custom = neenee_contracts::ColorSchemeConfig::default();
        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 24);

        terminal.draw(|frame| {
            draw_config_view(
                frame,
                ConfigViewProps {
                    category_index: 0,
                    detail_index: 0,
                    focus: ConfigFocus::Categories,
                    color_scheme: "zen",
                    custom_color_scheme: &custom,
                    custom_color_draft: &custom,
                    custom_editing: false,
                    input: "",
                    cursor_position: 0,
                    transcript_layout: crate::layout::Strategy::TurnBand,
                    expand_auto_scroll: false,
                    click_outside_dismiss: true,
                    workspace: "~/workspace",
                    category_scroll: &mut 0,
                    detail_scroll: &mut 0,
                    theme: &theme,
                },
            );
        });
        assert!(grid_row(&terminal, 0).contains("SETTINGS"));
        assert!(grid_row(&terminal, 0).contains("Appearance"));
        assert!(
            grid_row(&terminal, 1).trim().is_empty(),
            "Row 1 must be an empty spacer line"
        );
        assert!(
            !grid_row(&terminal, 2).contains("CATEGORIES"),
            "Panel title row must be removed"
        );

        terminal.draw(|frame| {
            draw_config_view(
                frame,
                ConfigViewProps {
                    category_index: 0,
                    detail_index: 5,
                    focus: ConfigFocus::Detail,
                    color_scheme: "custom",
                    custom_color_scheme: &custom,
                    custom_color_draft: &custom,
                    custom_editing: true,
                    input: "#8ea191",
                    cursor_position: 7,
                    transcript_layout: crate::layout::Strategy::TurnBand,
                    expand_auto_scroll: false,
                    click_outside_dismiss: true,
                    workspace: "~/workspace",
                    category_scroll: &mut 0,
                    detail_scroll: &mut 0,
                    theme: &theme,
                },
            );
        });
        assert!(grid_row(&terminal, 0).contains("SETTINGS"));
    }

    /// Render both the compact Envoy step (root view) and the zoomed-in
    /// Envoy view with its page header, ensuring no layout panics.
    /// Visual verification (run with NEENEE_VISUAL=1 --nocapture): an envoy
    /// zoom view with two ReAct turns, each emitting a concurrent tool-call
    /// batch, groups into turn bands with flush same-turn calls and a blank
    /// line between turns — exactly like the main session.
    #[test]
    fn envoy_view_groups_children_into_turn_bands() {
        let theme = Theme::default();
        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 30);
        let mut task = TranscriptMessage::tool_step(
            "task_1",
            "envoy",
            r#"{"description":"explore the codebase","prompt":"..."}"#,
        );
        let call = |id: &str, name: &str, round: u64, turn: usize| {
            neenee_contracts::EnvoyEvent::ToolCall {
                id: id.into(),
                name: name.into(),
                arguments: r#"{"p":"x"}"#.into(),
                round,
                turn,
            }
        };
        let result = |id: &str, name: &str| neenee_contracts::EnvoyEvent::ToolResult {
            id: id.into(),
            name: name.into(),
            output: "done".into(),
            duration_ms: 5,
        };
        // Turn 1: a 3-call concurrent batch.
        for (id, name) in [("a", "read_text"), ("b", "grep"), ("c", "list_dir")] {
            task.push_envoy_event(&call(id, name, 1, 0));
            task.push_envoy_event(&result(id, name));
        }
        // Turn 2: a 2-call concurrent batch.
        for (id, name) in [("d", "websearch"), ("e", "webfetch")] {
            task.push_envoy_event(&call(id, name, 1, 1));
            task.push_envoy_event(&result(id, name));
        }
        let children = task.envoy_children().unwrap().to_vec();
        terminal.draw(|f| {
            let mut layout_map = LayoutMap::new();
            let _ = draw_transcript(
                f,
                &mut layout_map,
                TranscriptView {
                    messages: &children,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    envoy_bar: Some(EnvoyBarInfo {
                        role: Some("explore".to_string()),
                        label: "the codebase".to_string(),
                        index: 1,
                        total: 1,
                    }),
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
        });
        let width = terminal.buffer().area().width as usize;
        let rows: Vec<String> = (0..terminal.buffer().area().height as usize)
            .map(|row| {
                terminal.buffer().content[row * width..(row + 1) * width]
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect()
            })
            .collect();
        if std::env::var("NEENEE_VISUAL").is_ok() {
            eprintln!("\n┌─ Envoy zoom (turn-banded) ─");
            for r in &rows {
                eprintln!("│{r}");
            }
            eprintln!("└────\n");
        }
        // Two turn headers appear (turn 1 and turn 2 of the envoy's round 1).
        let body = rows.join("\n");
        assert!(body.contains("turn 1"), "expected a `turn 1` band: {body}");
        assert!(body.contains("turn 2"), "expected a `turn 2` band: {body}");
        // Same-turn sibling calls are flush (no blank row between `read_text`
        // and `grep` inside turn 1); the two turns are separated by a blank.
        let line_of = |needle: &str| {
            rows.iter()
                .position(|r| r.contains(needle))
                .unwrap_or_else(|| panic!("no row containing {needle}"))
        };
        let t1_first = line_of("Read");
        let t1_second = line_of("Grep");
        assert_eq!(t1_second, t1_first + 1, "same-turn calls stay flush");
        // turn 2's header sits at least one blank row after turn 1's batch.
        let t2_header = line_of("turn 2");
        assert!(t2_header > t1_second + 1, "turns are separated");
    }

    #[test]
    fn envoy_step_and_view_render_without_panicking() {
        let theme = Theme::default();
        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 30);

        // Root view: a completed envoy task renders as a compact step.
        let mut task = TranscriptMessage::tool_step(
            "task_1",
            "envoy",
            r#"{"description":"explore the codebase","prompt":"..."}"#,
        );
        task.push_envoy_event(&neenee_contracts::EnvoyEvent::ToolCall {
            id: "inner".into(),
            name: "grep".into(),
            arguments: r#"{"pattern":"foo"}"#.into(),
            round: 1,
            turn: 0,
        });
        task.finish_tool_step(
            "task_1",
            "found 3 matches",
            neenee_contracts::ToolOutput::text("found 3 matches"),
            1200,
        );
        let root_messages = vec![
            TranscriptMessage::new(neenee_contracts::Role::User, "explore please"),
            task,
        ];

        terminal.draw(|f| {
            let mut layout_map = LayoutMap::new();
            let _ = draw_transcript(
                f,
                &mut layout_map,
                TranscriptView {
                    messages: &root_messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "running envoy",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    envoy_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
        });

        // Zoomed-in Envoy view: the task's children are the message stream
        // and the contextual header is shown on the first row.
        let children = root_messages[1].envoy_children().unwrap().to_vec();
        terminal.draw(|f| {
            let mut layout_map = LayoutMap::new();
            let _ = draw_transcript(
                f,
                &mut layout_map,
                TranscriptView {
                    messages: &children,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    envoy_bar: Some(EnvoyBarInfo {
                        role: Some("explore".to_string()),
                        label: "the codebase".to_string(),
                        index: 1,
                        total: 2,
                    }),
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
        });

        let width = terminal.buffer().area().width as usize;
        let row_text = |row: usize| -> String {
            terminal.buffer().content[row * width..(row + 1) * width]
                .iter()
                .map(|cell| cell.symbol())
                .collect()
        };
        let head_row = row_text(0);
        // The symbol row is unchanged by the full-width band (the pads and the
        // old inset columns were all spaces); only the background differs —
        // the whole row now paints `body`, asserted in page_header's tests.
        assert_eq!(
            head_row,
            "   ENVOY [EXPLORE] the codebase                                         (1/2)   ",
            "Envoy identity, role tag, title and sibling index on the head row"
        );
        // The permanent key legend occupies the last three terminal rows,
        // with the shortcuts on its middle row.
        let legend = row_text(28);
        assert!(
            legend.contains("Esc back") && legend.contains("[ prev") && legend.contains("] next"),
            "Envoy shortcuts pinned on the footer's middle row: {legend:?}"
        );
        assert!(
            row_text(27).trim().is_empty() && row_text(29).trim().is_empty(),
            "The footer's top and bottom rows are blank padding"
        );
    }

    #[test]
    fn height_cache_skip_path_matches_full_layout() {
        // Stage 2 invariant: a warm height cache (which lets the transcript
        // pass *skip* re-wrapping off-screen messages) must produce byte-for-
        // byte the same frame — and the same total `content_lines` — as a cold
        // render that lays every message out in full. If the skip arithmetic
        // (`skip_rows` / `current_y` / `content_lines`) drifted, this fails.
        use crate::model::layout::LayoutMap;
        let theme = Theme::default();

        // A tall transcript: enough wrapped plain-text messages to overflow an
        // 80x24 viewport several times, so both skip branches are exercised —
        // messages scrolled above the viewport (fully_above) and messages below
        // its bottom (fully_below).
        let messages: Vec<TranscriptMessage> = (0..40)
            .map(|i| {
                TranscriptMessage::new(
                    neenee_contracts::Role::Assistant,
                    format!(
                        "Message number {i} with enough words to wrap across a \
                         couple of lines in an eighty column terminal so the \
                         per-message heights are non-trivial and varied."
                    ),
                )
            })
            .collect();
        let (width, height, scroll) = (80u16, 24u16, 30u16);

        let dump = |cache: &mut HeightCache| -> (String, usize) {
            let mut terminal = neenee_tui_engine::TestTerminal::new(width, height);
            let mut layout_map = LayoutMap::new();
            let mut content_lines = 0usize;
            terminal.draw(|f| {
                let r = draw_transcript(
                    f,
                    &mut layout_map,
                    TranscriptView {
                        messages: &messages,
                        scroll,
                        selection: &SelectionState::None,
                        cell_selection: None,
                        activity: "",
                        awaiting_permission: false,
                        spinner_phase: 0,
                        input: "",
                        byte_cursor: 0,
                        chrome_hidden: false,
                        queue_bar: QueueBarView {
                            items: &[],
                            paused: false,
                            blocked: false,
                        },
                        envoy_bar: None,
                        side_banner: None,
                        page_hints: None,
                        session_head: None,
                        todos: None,
                        round_started_at: None,
                        hovered_step: None,
                        focused_target: None,
                        logo: None,
                        guidance: EmptyStateGuidance::Tour,
                        carousel_index: 0,
                        theme: &theme,
                        layout: crate::layout::Strategy::default(),
                        height_cache: Some(cache),
                    },
                );
                content_lines = r.content_lines;
            });
            let buf = terminal.buffer();
            let bw = buf.area().width as usize;
            let mut s = String::new();
            for y in 0..height as usize {
                for x in 0..width as usize {
                    s.push_str(buf.content[y * bw + x].symbol());
                }
                s.push('\n');
            }
            (s, content_lines)
        };

        let mut cache = HeightCache::default();
        // Cold: cache empty, every message laid out in full (and measured).
        let (cold_grid, cold_lines) = dump(&mut cache);
        // Warm: off-screen messages now take the skip path.
        let (warm_grid, warm_lines) = dump(&mut cache);

        assert_eq!(
            cold_lines, warm_lines,
            "content_lines must match between full and skip layout"
        );
        assert_eq!(
            cold_grid, warm_grid,
            "rendered frame must be identical between full and skip layout"
        );
        // The skip path must actually have been reachable (cache populated).
        assert!(cache.get(messages[0].id).is_some());
    }

    #[test]
    fn expanded_edit_diff_height_is_scroll_independent() {
        // Regression: the expanded edit-diff renderer must account every
        // logical row in `content_lines` even when the viewport clips the
        // body mid-hunk. An early return once the viewport filled made the
        // measured height depend on the scroll offset; the app loop derives
        // `max_scroll` from it, so the scroll position oscillated and the
        // frame flickered during the animation heartbeat.
        let theme = Theme::default();

        // A completed edit whose diff body is several times taller than the
        // viewport, so mid-range scroll offsets clip inside the hunk rows.
        let old: String = (1..=60).map(|i| format!("let v{i} = {i};\n")).collect();
        let new: String = (1..=60)
            .map(|i| format!("let v{i} = {};\n", i * 10))
            .collect();
        let mut m = TranscriptMessage::tool_step(
            "call_test",
            "edit_file",
            r#"{"path":"a.rs","old_string":"…","new_string":"…"}"#,
        );
        let structured = neenee_contracts::ToolOutput::Patch {
            path: "a.rs".into(),
            op: neenee_contracts::PatchOp::Edit,
            old,
            new,
            start_line: 0,
        };
        m.finish_tool_step("call_test", structured.to_text(), structured, 0);
        if let crate::model::document::MessageKind::ToolStep { expanded, .. } = &mut m.kind {
            *expanded = true;
        }
        let messages = vec![m];

        let (width, height) = (80u16, 24u16);
        let measure = |scroll: u16, cache: &mut HeightCache| -> usize {
            let mut terminal = neenee_tui_engine::TestTerminal::new(width, height);
            let mut layout_map = LayoutMap::new();
            let mut lines = 0usize;
            terminal.draw(|f| {
                let r = draw_transcript(
                    f,
                    &mut layout_map,
                    TranscriptView {
                        messages: &messages,
                        scroll,
                        selection: &SelectionState::None,
                        cell_selection: None,
                        activity: "",
                        awaiting_permission: false,
                        spinner_phase: 0,
                        input: "",
                        byte_cursor: 0,
                        chrome_hidden: false,
                        queue_bar: QueueBarView {
                            items: &[],
                            paused: false,
                            blocked: false,
                        },
                        envoy_bar: None,
                        side_banner: None,
                        page_hints: None,
                        session_head: None,
                        todos: None,
                        round_started_at: None,
                        hovered_step: None,
                        focused_target: None,
                        logo: None,
                        guidance: EmptyStateGuidance::Tour,
                        carousel_index: 0,
                        theme: &theme,
                        layout: crate::layout::Strategy::default(),
                        height_cache: Some(cache),
                    },
                );
                lines = r.content_lines;
            });
            lines
        };

        let mut cache = HeightCache::default();
        let at_top = measure(0, &mut cache);
        assert!(
            at_top > height as usize,
            "the diff must overflow the viewport for this test to mean anything"
        );
        // Every offset that clips into the diff body must report the same
        // total height, through both cold and warm height-cache paths.
        for scroll in [1u16, 7, 20, 40, 60] {
            assert_eq!(
                measure(scroll, &mut cache),
                at_top,
                "content_lines must not depend on the scroll offset (scroll = {scroll})"
            );
        }
        let mut fresh_cache = HeightCache::default();
        assert_eq!(
            measure(20, &mut fresh_cache),
            at_top,
            "a cold height cache must measure the same height as a warm one"
        );
    }

    #[test]
    fn completed_diff_cache_survives_height_invalidation_and_resize() {
        let mut cache = HeightCache::default();
        let first = cache.diff_cache.patch(42, "old", "new", 10);

        cache.clear();
        cache.prepare(120);

        let second = cache.diff_cache.patch(42, "old", "new", 10);
        assert!(
            std::sync::Arc::ptr_eq(&first, &second),
            "width-dependent height invalidation must retain semantic diff rows"
        );
    }

    #[test]
    fn virtual_index_selects_only_chunks_intersecting_the_viewport() {
        let messages = (0..4)
            .map(|i| TranscriptMessage::new(neenee_contracts::Role::Assistant, format!("m{i}")))
            .collect::<Vec<_>>();
        let mut cache = HeightCache::default();
        cache.prepare(80);
        // Four-line bodies plus one boundary row owned by each following
        // message: chunks begin at 0, 4, 9, and 14.
        for message in &messages {
            cache.set(message.id, 4);
        }

        let window = cache
            .virtual_window(&messages, crate::layout::Strategy::TurnBand, 6, 3)
            .expect("all message heights are cached");
        assert_eq!(window.message_start, 1);
        assert_eq!(window.message_end, 2);
        assert_eq!(window.prefix_lines, 4);
        assert_eq!(window.skip_rows, 2);
        assert_eq!(window.total_lines, 19);
    }

    #[test]
    fn virtual_index_uses_segmented_same_turn_geometry() {
        let mut thinking = TranscriptMessage::thinking("reasoning").with_turn(3);
        thinking.set_thinking_duration(1);
        let first = TranscriptMessage::tool_step("a", "read_text", r#"{"path":"a"}"#).with_turn(3);
        let second = TranscriptMessage::tool_step("b", "read_text", r#"{"path":"b"}"#).with_turn(3);
        let messages = vec![thinking, first, second];
        let mut cache = HeightCache::default();
        cache.prepare(80);
        for message in &messages {
            cache.set(message.id, 2);
        }

        let window = cache
            .virtual_window(&messages, crate::layout::Strategy::TurnBand, 0, 20)
            .expect("all message heights are cached");
        assert_eq!(window.message_start, 0);
        assert_eq!(window.message_end, 3);
        assert_eq!(
            window.total_lines, 9,
            "header + header gap + thinking + segment gap + flush tool batch"
        );
    }

    #[test]
    fn line_selection_intersects_wrapped_lines() {
        use crate::model::layout::SemanticCursor;
        let sel = SelectionState::Range {
            anchor: SemanticCursor::new(0, 0, 2),
            head: SemanticCursor::new(0, 0, 8),
        };
        let range = block_selection_range(&sel, 0, 0);

        // Line covering bytes 0..5 ("hello"): selected from 2 to end.
        let first = WrappedLine {
            text: "hello".to_string(),
            start_byte: 0,
            end_byte: 5,
        };
        assert_eq!(line_selection(range, &first), Some((2, 5)));

        // Line covering bytes 5..10 ("world"): selected up to head char (8 → rel 3, inclusive → 4).
        let second = WrappedLine {
            text: "world".to_string(),
            start_byte: 5,
            end_byte: 10,
        };
        assert_eq!(line_selection(range, &second), Some((0, 4)));

        // A line after the selection has no overlap.
        let third = WrappedLine {
            text: "after".to_string(),
            start_byte: 10,
            end_byte: 15,
        };
        assert_eq!(line_selection(range, &third), None);
    }

    #[test]
    fn block_selection_covers_middle_blocks_fully() {
        use crate::model::layout::SemanticCursor;
        let sel = SelectionState::Range {
            anchor: SemanticCursor::new(0, 0, 3),
            head: SemanticCursor::new(0, 2, 1),
        };
        assert_eq!(block_selection_range(&sel, 0, 0), Some((3, None)));
        assert_eq!(block_selection_range(&sel, 0, 1), Some((0, None)));
        assert_eq!(block_selection_range(&sel, 0, 2), Some((0, Some(1))));
        assert_eq!(block_selection_range(&sel, 0, 3), None);
        assert_eq!(block_selection_range(&sel, 1, 0), None);
    }

    #[test]
    fn test_wrap_text() {
        let lines = wrap_text("hello world", 5);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text, "hello");
        assert_eq!(lines[1].text, " worl");
        assert_eq!(lines[2].text, "d");
    }

    #[test]
    fn test_wrap_with_newlines() {
        let lines = wrap_text("hi\nthere", 10);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "hi");
        assert_eq!(lines[1].text, "there");
    }

    #[test]
    fn wrap_avoids_cjk_punctuation_at_line_start() {
        let lines = wrap_text("人生需要坚持，才能前进。", 12);
        assert!(lines.len() > 1);
        assert!(lines.iter().skip(1).all(|line| {
            line.text
                .chars()
                .next()
                .is_none_or(|ch| !prohibited_line_start(ch))
        }));
        assert!(lines.iter().all(|line| {
            line.text
                .chars()
                .last()
                .is_none_or(|ch| !prohibited_line_end(ch))
        }));
    }

    /// The input box must reserve only a single content row for a short input
    /// but grow to fit wrapped text when the input is long.
    #[test]
    fn input_box_grows_with_wrapped_content() {
        let theme = Theme::default();
        let messages: Vec<TranscriptMessage> = Vec::new();

        fn render_with(theme: &Theme, messages: &[TranscriptMessage], input: &str) -> Rect {
            let mut terminal = neenee_tui_engine::TestTerminal::new(40, 24);
            let mut rect = Rect::default();
            terminal.draw(|f| {
                let mut layout_map = LayoutMap::new();
                let r = draw_transcript(
                    f,
                    &mut layout_map,
                    TranscriptView {
                        messages,
                        scroll: 0,
                        selection: &SelectionState::None,
                        cell_selection: None,
                        activity: "",
                        awaiting_permission: false,
                        spinner_phase: 0,
                        input,
                        byte_cursor: input.len(),
                        chrome_hidden: false,
                        queue_bar: QueueBarView {
                            items: &[],
                            paused: false,
                            blocked: false,
                        },
                        envoy_bar: None,
                        side_banner: None,
                        page_hints: None,
                        session_head: None,
                        todos: None,
                        round_started_at: None,
                        hovered_step: None,
                        focused_target: None,
                        logo: None,
                        guidance: EmptyStateGuidance::Tour,
                        carousel_index: 0,
                        theme,
                        layout: crate::layout::Strategy::default(),
                        height_cache: None,
                    },
                );
                rect = r.input_rect;
            });
            rect
        }

        // Short input: one content line + two padding rows = 3.
        let short = render_with(&theme, &messages, "hi");
        assert_eq!(short.height, 3);

        // Long input wraps across many lines on a 40-wide terminal; the box
        // must grow beyond the single-line baseline.
        let long_input = "word ".repeat(40);
        let tall = render_with(&theme, &messages, &long_input);
        assert!(
            tall.height > 3,
            "wrapped input should grow the box, got height {}",
            tall.height
        );
        // ...but never more than half the terminal.
        assert!(tall.height <= 12);
    }

    #[test]
    fn footer_keeps_one_blank_row_below_transcript_when_active_or_idle() {
        fn assert_gap(activity: &str) {
            let theme = Theme::default();
            let messages = vec![TranscriptMessage::new(
                neenee_contracts::Role::Assistant,
                "A finished response above the footer.",
            )];
            let mut terminal = neenee_tui_engine::TestTerminal::new(60, 20);
            let mut footer_anchor_y = 0;
            let mut transcript_height = 0;

            terminal.draw(|frame| {
                let mut layout_map = LayoutMap::new();
                let rendered = draw_transcript(
                    frame,
                    &mut layout_map,
                    TranscriptView {
                        messages: &messages,
                        scroll: 0,
                        selection: &SelectionState::None,
                        cell_selection: None,
                        activity,
                        awaiting_permission: false,
                        spinner_phase: 0,
                        input: "",
                        byte_cursor: 0,
                        chrome_hidden: false,
                        queue_bar: QueueBarView {
                            items: &[],
                            paused: false,
                            blocked: false,
                        },
                        envoy_bar: None,
                        side_banner: None,
                        page_hints: None,
                        session_head: None,
                        todos: None,
                        round_started_at: None,
                        hovered_step: None,
                        focused_target: None,
                        logo: None,
                        guidance: EmptyStateGuidance::Tour,
                        carousel_index: 0,
                        theme: &theme,
                        layout: crate::layout::Strategy::default(),
                        height_cache: None,
                    },
                );
                footer_anchor_y = footer_stack::rect_of(&rendered.footer, FooterRowId::Activity)
                    .map(|rect| rect.y)
                    .unwrap_or(rendered.input_rect.y);
                transcript_height = rendered.view_height;
            });

            // The footer always begins after a permanent one-row gap below the
            // transcript. The queue bar in this fixture is empty, so it is
            // hidden; the anchor is whichever region leads the footer — the
            // activity bar when responding, the input box when idle — both of
            // which sit directly under the gap.
            let expected_anchor = 1 + transcript_height + FOOTER_TOP_GAP_ROWS;
            assert_eq!(footer_anchor_y, expected_anchor);
            // The permanent one-row gap sits directly below the transcript,
            // above whichever footer region leads (activity bar when
            // responding, queue bar when idle).
            let separator_y = 1 + transcript_height;
            let width = terminal.buffer().area().width as usize;
            let row_start = separator_y as usize * width;
            let separator = &terminal.buffer().content[row_start..row_start + width];
            assert!(
                separator.iter().all(|cell| cell.symbol() == " "),
                "separator row must stay blank while activity={activity:?}"
            );
        }

        assert_gap("responding");
        assert_gap("idle");
    }

    /// The declarative footer stack must place every row exactly where the
    /// old hand-rolled offset arithmetic did. This test keeps the legacy
    /// formula as an oracle: with a full chrome (todo + queue + activity +
    /// composer + hint all visible) each bar's rect must equal the
    /// `status_y + Σ(prior heights)` it replaced, so the refactor is provably
    /// behavior-preserving.
    #[test]
    fn footer_stack_places_rows_where_the_legacy_offsets_did() {
        let theme = Theme::default();
        let messages = vec![TranscriptMessage::new(
            neenee_contracts::Role::User,
            "hello",
        )];
        let todos = neenee_contracts::TodoList {
            items: vec![neenee_contracts::TodoItem {
                id: neenee_contracts::TodoId(1),
                content: "one".into(),
                status: neenee_contracts::TodoStatus::InProgress,
                created_at: 0,
                updated_at: 0,
            }],
            next_id: 2,
            updated_at_round: 0,
        };
        let queue_items = [crate::chrome::QueueItemView {
            queued_at_ms: 1_700_000_000_000,
            text: "next".into(),
            steering: false,
        }];

        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 30);
        let mut render_opt: Option<TranscriptRender> = None;
        terminal.draw(|f| {
            render_opt = Some(draw_transcript(
                f,
                &mut LayoutMap::new(),
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "responding",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: crate::chrome::QueueBarView {
                        items: &queue_items,
                        paused: false,
                        blocked: false,
                    },
                    envoy_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: Some(&todos),
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            ));
        });
        let rendered = render_opt.expect("render result");

        // Legacy oracle, verbatim from the pre-stack code: footer_x/w from the
        // shared inset, status_y after the top gap, then each row's y is the
        // cumulative sum of the rows above it.
        let footer_h = crate::design::FOOTER_TOP_GAP_ROWS
            + crate::design::TODO_BAR_ROWS
            + crate::design::QUEUE_BAR_ROWS
            + crate::design::ACTIVITY_BAR_ROWS
            + rendered.input_rect.height // composer
            + crate::design::HINT_BAR_ROWS;
        // The terminal is 30 rows; the head is absent here, so the footer
        // band starts at 30 - footer_h.
        let band_y = 30 - footer_h;
        let footer_x = crate::design::FOOTER_H_INSET;
        let footer_w = 80 - 2 * crate::design::FOOTER_H_INSET;
        let status_y = band_y + crate::design::FOOTER_TOP_GAP_ROWS;

        let expect = |y: u16, h: u16| neenee_tui_engine::Rect::new(footer_x, y, footer_w, h);
        assert_eq!(
            footer_stack::rect_of(&rendered.footer, FooterRowId::Todos),
            Some(expect(status_y, TODO_BAR_ROWS)),
            "todos bar rect"
        );
        assert_eq!(
            footer_stack::rect_of(&rendered.footer, FooterRowId::Queue),
            Some(expect(status_y + TODO_BAR_ROWS, QUEUE_BAR_ROWS)),
            "queue bar rect"
        );
        assert_eq!(
            footer_stack::rect_of(&rendered.footer, FooterRowId::Activity),
            Some(expect(
                status_y + TODO_BAR_ROWS + QUEUE_BAR_ROWS,
                ACTIVITY_BAR_ROWS
            )),
            "activity bar rect"
        );
        assert_eq!(
            Some(rendered.input_rect),
            footer_stack::rect_of(&rendered.footer, FooterRowId::Composer),
            "composer rect appears in the registry exactly as returned"
        );
        assert_eq!(
            rendered.input_rect,
            expect(
                status_y + TODO_BAR_ROWS + QUEUE_BAR_ROWS + ACTIVITY_BAR_ROWS,
                rendered.input_rect.height
            ),
            "composer rect matches the legacy offset"
        );
        assert_eq!(
            Some(rendered.hint_rect),
            footer_stack::rect_of(&rendered.footer, FooterRowId::Hint),
            "hint bar rect appears in the registry exactly as returned"
        );
        assert_eq!(
            rendered.hint_rect,
            expect(
                status_y
                    + TODO_BAR_ROWS
                    + QUEUE_BAR_ROWS
                    + ACTIVITY_BAR_ROWS
                    + rendered.input_rect.height,
                HINT_BAR_ROWS
            ),
            "hint bar rect matches the legacy offset"
        );
        // The registry is complete: gap + the five interactive rows.
        assert_eq!(rendered.footer.rows.len(), 6, "registry completeness");
        assert_eq!(
            footer_stack::rect_of(&rendered.footer, FooterRowId::TopGap),
            Some(expect(band_y, crate::design::FOOTER_TOP_GAP_ROWS)),
            "the top gap is part of the stack's geometry"
        );
    }

    /// When the terminal is resized below the usable minimum,
    /// `draw_transcript` must not render the normal UI (which would underflow
    /// the footer layout math). Instead it hides everything, shows a centered
    /// "terminal too small" notice, and returns a zeroed `TranscriptRender` so
    /// the app loop draws no chrome over it.
    #[test]
    fn too_small_terminal_shows_notice_and_zeroed_render() {
        let theme = Theme::default();
        let messages = vec![TranscriptMessage::new(
            neenee_contracts::Role::User,
            "hello",
        )];

        let mut terminal = neenee_tui_engine::TestTerminal::new(20, 8);
        let mut render_opt: Option<TranscriptRender> = None;
        terminal.draw(|f| {
            render_opt = Some(draw_transcript(
                f,
                &mut LayoutMap::new(),
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    envoy_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            ));
        });

        let render = render_opt.expect("draw_transcript must return a render");
        // The guard suppresses all chrome geometry.
        assert_eq!(render.input_rect, Rect::default());
        assert_eq!(render.hint_rect, Rect::default());
        assert_eq!(render.view_height, 0);
        assert_eq!(render.content_lines, 0);

        // The notice text must be present somewhere in the rendered buffer.
        let buffer = terminal.buffer();
        let rendered: String = (0..buffer.area().height)
            .flat_map(|y| {
                (0..buffer.area().width).map(move |x| buffer[(x, y)].symbol().to_string())
            })
            .collect::<String>();
        assert!(
            rendered.contains("Terminal too small"),
            "expected the too-small notice in the rendered buffer"
        );
    }

    /// An empty composer must still record a layout-map region for its single
    /// text row. Without it a click inside the empty box can't resolve to a
    /// cursor, so the click handler can't clear a focused step to hand typing
    /// back to the prompt. See `draw_composer` / `composer_wrapped`.
    #[test]
    fn draw_composer_records_region_for_empty_input() {
        let theme = Theme::default();
        let mut terminal = neenee_tui_engine::TestTerminal::new(30, 5);
        let mut layout_map = LayoutMap::new();
        let input_rect = Rect::new(0, 0, 30, 3);
        terminal.draw(|f| {
            draw_composer(
                f,
                input_rect,
                "",
                0,
                true,
                true,
                &theme,
                &mut layout_map,
                true,
                &mut 0,
                &SelectionState::None,
                0,
                0,
            );
        });

        // The empty text row sits one line below the box's top edge.
        let cursor = layout_map
            .cursor_at(
                input_rect.x + COMPOSER_PROMPT_PREFIX_COLS as u16,
                input_rect.y + 1,
            )
            .expect("click inside empty input box must resolve to a cursor");
        assert_eq!(cursor.message_idx, INPUT_MSG_IDX);
        assert_eq!(cursor.byte_offset, 0);
    }

    /// `draw_composer` must not panic for tricky inputs and should place the caret
    /// on the second wrapped line when the cursor sits past the first wrap.
    #[test]
    fn draw_composer_wraps_and_positions_caret() {
        let theme = Theme::default();
        let mut terminal = neenee_tui_engine::TestTerminal::new(20, 12);
        // "aaaa bbbb cccc" wraps within the ~17-wide inner area; cursor at the
        // very end should be on a later line, not off the box.
        let input = "aaaa bbbb cccc dddd eeee";
        terminal.draw(|f| {
            draw_composer(
                f,
                Rect::new(0, 0, 20, 8),
                input,
                input.len(),
                true,
                true,
                &theme,
                &mut LayoutMap::new(),
                true,
                &mut 0,
                &SelectionState::None,
                0,
                0,
            );
        });
    }

    /// The caret must land flush against the final glyph at the end of the
    /// input, measured in display columns — i.e. exactly where the grid painted
    /// the text. This is the CJK regression: a buggy grapheme-floor returned the
    /// last grapheme *start*, leaving the caret two columns short of a wide
    /// glyph (one for ASCII). The caret column must equal the rendered width of
    /// the text, for both wide and narrow glyphs.
    #[test]
    fn draw_composer_caret_flush_against_final_grapheme() {
        let theme = Theme::default();

        for (label, input, expected_cols) in [
            ("cjk", "中文", 4usize),
            ("ascii", "ab", 2),
            ("mixed", "a中", 3),
        ] {
            let mut terminal = neenee_tui_engine::TestTerminal::new(20, 5);
            terminal.draw(|f| {
                draw_composer(
                    f,
                    Rect::new(0, 0, 20, 4),
                    input,
                    input.len(),
                    true,
                    true,
                    &theme,
                    &mut LayoutMap::new(),
                    false,
                    &mut 0,
                    &SelectionState::None,
                    0,
                    0,
                );
            });
            let cursor = match terminal.cursor() {
                neenee_tui_engine::CursorState::Visible(x, y) => (x, y),
                other => panic!("{label}: caret should be visible, got {other:?}"),
            };
            // The text row sits one line below the box's top padding row, and
            // the caret follows the `› ` prefix plus the full rendered width.
            assert_eq!(
                cursor,
                (
                    (COMPOSER_PROMPT_PREFIX_COLS + expected_cols) as u16,
                    crate::design::COMPOSER_TEXT_ROW_OFFSET,
                ),
                "{label}: caret not flush with end of {input:?}"
            );
        }
    }

    /// A resolved `/command` token renders in bold + the theme accent color,
    /// and the accent stops at the token boundary — the argument tail keeps
    /// the normal text color so the two read as command + payload.
    #[test]
    fn draw_composer_highlighted_accents_only_the_command_token() {
        let theme = Theme::default();
        let mut terminal = neenee_tui_engine::TestTerminal::new(30, 4);
        let input = "/repeat every minute";
        terminal.draw(|f| {
            draw_composer_highlighted(
                f,
                Rect::new(0, 0, 30, 3),
                input,
                input.len(),
                true,
                true,
                &theme,
                &mut LayoutMap::new(),
                false,
                &mut 0,
                &SelectionState::None,
                "/repeat".len(),
                0,
                0,
            );
        });
        let buf = terminal.buffer();
        let text_y = crate::design::COMPOSER_TEXT_ROW_OFFSET;
        let text_x = COMPOSER_PROMPT_PREFIX_COLS as u16;
        // Every glyph of `/repeat` is bold + brand-colored on the panel bg.
        for (i, ch) in "/repeat".chars().enumerate() {
            let cell = buf.get(text_x + i as u16, text_y).expect("command cell");
            assert_eq!(cell.symbol(), ch.to_string());
            assert_eq!(cell.fg, theme.brand(), "command glyph {ch} lost the accent");
            assert!(
                cell.style.add.contains(neenee_tui_engine::Modifier::BOLD),
                "command glyph {ch} lost bold"
            );
        }
        // The argument tail (`every minute`) keeps the default text color.
        let arg_start = text_x + "/repeat ".len() as u16;
        let cell = buf.get(arg_start, text_y).expect("argument cell");
        assert_eq!(cell.symbol(), "e");
        assert_eq!(cell.fg, theme.fg(), "argument text must not be accented");
    }

    /// The accent must not bleed past the first wrapped row: when the command
    /// token itself fits but the highlight length would cover the wrap
    /// boundary, the continuation row renders in the normal text color.
    #[test]
    fn draw_composer_highlight_clamps_at_wrap_boundary() {
        let theme = Theme::default();
        let mut terminal = neenee_tui_engine::TestTerminal::new(13, 6);
        // 10-column text area (13 - 2 prefix - 2 right pad + 1): `/sessions`
        // fills row 0 exactly; ` abc` wraps to row 1.
        let input = "/sessions abc";
        terminal.draw(|f| {
            draw_composer_highlighted(
                f,
                Rect::new(0, 0, 13, 5),
                input,
                input.len(),
                true,
                true,
                &theme,
                &mut LayoutMap::new(),
                false,
                &mut 0,
                &SelectionState::None,
                "/sessions".len(),
                0,
                0,
            );
        });
        let buf = terminal.buffer();
        let row1_y = crate::design::COMPOSER_TEXT_ROW_OFFSET + 1;
        // The continuation row keeps the two-column prompt indent before the
        // wrapped text (`/sessions` fills row 0 exactly).
        let cell = buf
            .get(COMPOSER_PROMPT_PREFIX_COLS as u16 + 1, row1_y)
            .expect("continuation cell");
        assert_eq!(cell.symbol(), "a", "continuation row should start with 'a'");
        assert_eq!(
            cell.fg,
            theme.fg(),
            "accent must not bleed onto the wrapped argument row"
        );
    }

    /// Attachment chips render as distinct colored "pills": a pasted-text
    /// chip in the calm text-block blue and an image chip in the warm amber,
    /// each bold on a tinted band, while the surrounding prose keeps the
    /// normal text color. The color is the identifier's second channel —
    /// kind at a glance, payload size in the label.
    #[test]
    fn draw_composer_paints_paste_and_image_chips_distinctly() {
        let theme = Theme::default();
        let paste_chip = crate::composer_attachments::paste_chip(1, 3, 2048);
        let image_chip = crate::composer_attachments::image_chip(1, 1536);
        let input = format!("see {paste_chip} plus {image_chip} end");
        let mut terminal = neenee_tui_engine::TestTerminal::new(120, 5);
        terminal.draw(|f| {
            draw_composer(
                f,
                Rect::new(0, 0, 120, 3),
                &input,
                input.len(),
                true,
                true,
                &theme,
                &mut LayoutMap::new(),
                false,
                &mut 0,
                &SelectionState::None,
                1,
                1,
            );
        });
        let buf = terminal.buffer();
        let text_y = crate::design::COMPOSER_TEXT_ROW_OFFSET;
        let text_x = COMPOSER_PROMPT_PREFIX_COLS as u16;
        let panel_bg = theme.input_surface();

        // Chip labels are ASCII plus a multi-byte `·` badge; display columns
        // come from `str_len`, never from the raw byte length.
        let paste_width = neenee_tui_engine::text::str_len(&paste_chip);
        let paste_start = text_x + "see ".len() as u16;
        let paste_end = paste_start + paste_width as u16;
        for col in paste_start..paste_end {
            let cell = buf.get(col, text_y).expect("paste chip cell");
            assert_eq!(
                cell.fg,
                theme.chip_paste_fg(),
                "paste chip glyph lost its blue"
            );
            assert_eq!(
                cell.bg,
                theme.chip_paste_bg(panel_bg),
                "paste chip lost its pill band"
            );
            assert!(
                cell.style.add.contains(neenee_tui_engine::Modifier::BOLD),
                "paste chip glyph lost bold"
            );
        }

        let image_width = neenee_tui_engine::text::str_len(&image_chip);
        let image_start = text_x + ("see ".len() + paste_width + " plus ".len()) as u16;
        let image_end = image_start + image_width as u16;
        for col in image_start..image_end {
            let cell = buf.get(col, text_y).expect("image chip cell");
            assert_eq!(
                cell.fg,
                theme.chip_image_fg(),
                "image chip glyph lost its amber"
            );
            assert_eq!(
                cell.bg,
                theme.chip_image_bg(panel_bg),
                "image chip lost its pill band"
            );
            assert!(
                cell.style.add.contains(neenee_tui_engine::Modifier::BOLD),
                "image chip glyph lost bold"
            );
        }

        // The prose around the chips keeps the normal text color on the panel.
        for col in [
            text_x,
            text_x + 2,
            text_x + ("see ".len() + paste_width) as u16,
        ] {
            let cell = buf.get(col, text_y).expect("prose cell");
            assert_eq!(cell.fg, theme.fg(), "prose next to a chip must stay plain");
            assert_eq!(cell.bg, panel_bg, "prose must not pick up a chip band");
        }
    }

    /// A chip label with **no staged payload** — typed by hand, or left over
    /// after the paste was undone — must render as ordinary text, never as a
    /// colored pill. The color marks a real attachment; a literal
    /// `[Image #1]` that the user merely typed must not pretend one exists.
    #[test]
    fn draw_composer_leaves_orphan_chip_labels_as_plain_text() {
        let theme = Theme::default();
        // No payload staged at all: `image_count = 0`, `paste_count = 0`.
        let orphan_image = "[Image #1]".to_string();
        let orphan_paste = "[Pasted text #1 +5 lines]".to_string();
        let input = format!("typed {orphan_image} and {orphan_paste} here");
        let mut terminal = neenee_tui_engine::TestTerminal::new(100, 5);
        terminal.draw(|f| {
            draw_composer(
                f,
                Rect::new(0, 0, 100, 3),
                &input,
                input.len(),
                true,
                true,
                &theme,
                &mut LayoutMap::new(),
                false,
                &mut 0,
                &SelectionState::None,
                0,
                0,
            );
        });
        let buf = terminal.buffer();
        let text_y = crate::design::COMPOSER_TEXT_ROW_OFFSET;
        let text_x = COMPOSER_PROMPT_PREFIX_COLS as u16;
        let panel_bg = theme.input_surface();

        // Every glyph of both orphan labels keeps the plain text color on the
        // plain panel background — no pill band, no kind color, no bold.
        for (offset, label) in [
            ("typed ".len(), &orphan_image),
            ("typed [Image #1] and ".len(), &orphan_paste),
        ] {
            let start = text_x + offset as u16;
            let end = start + neenee_tui_engine::text::str_len(label) as u16;
            for col in start..end {
                let cell = buf.get(col, text_y).expect("orphan chip cell");
                assert_eq!(
                    cell.fg,
                    theme.fg(),
                    "orphan label {label:?} must keep plain text color at col {col}"
                );
                assert_eq!(
                    cell.bg, panel_bg,
                    "orphan label {label:?} must not get a pill band at col {col}"
                );
                assert!(
                    !cell.style.add.contains(neenee_tui_engine::Modifier::BOLD),
                    "orphan label {label:?} must not be bold at col {col}"
                );
            }
        }
    }

    /// A real chip (payload staged) is colored while an orphan label typed
    /// next to it stays plain — the pill reflects the actual staged state of
    /// each block, so one never masks the other.
    #[test]
    fn draw_composer_colors_only_backed_chips_when_mixed() {
        let theme = Theme::default();
        let real_paste = crate::composer_attachments::paste_chip(1, 3, 2048);
        let orphan_image = "[Image #1]".to_string();
        // One paste payload staged; the image chip is a typed orphan.
        let input = format!("{real_paste} then {orphan_image} end");
        let mut terminal = neenee_tui_engine::TestTerminal::new(100, 5);
        terminal.draw(|f| {
            draw_composer(
                f,
                Rect::new(0, 0, 100, 3),
                &input,
                input.len(),
                true,
                true,
                &theme,
                &mut LayoutMap::new(),
                false,
                &mut 0,
                &SelectionState::None,
                0, // image_count: no image payload staged
                1, // paste_count: one paste payload staged
            );
        });
        let buf = terminal.buffer();
        let text_y = crate::design::COMPOSER_TEXT_ROW_OFFSET;
        let text_x = COMPOSER_PROMPT_PREFIX_COLS as u16;
        let panel_bg = theme.input_surface();

        // The backed paste chip gets the blue pill.
        let paste_width = neenee_tui_engine::text::str_len(&real_paste);
        let paste_end = text_x + paste_width as u16;
        for col in text_x..paste_end {
            let cell = buf.get(col, text_y).expect("backed paste cell");
            assert_eq!(
                cell.fg,
                theme.chip_paste_fg(),
                "backed paste chip lost its blue"
            );
            assert_eq!(
                cell.bg,
                theme.chip_paste_bg(panel_bg),
                "backed paste chip lost its band"
            );
        }

        // The orphan image label stays plain text.
        let orphan_start = text_x + ("".len() + paste_width + " then ".len()) as u16;
        let orphan_end = orphan_start + neenee_tui_engine::text::str_len(&orphan_image) as u16;
        for col in orphan_start..orphan_end {
            let cell = buf.get(col, text_y).expect("orphan image cell");
            assert_eq!(
                cell.fg,
                theme.fg(),
                "orphan image label must stay plain text"
            );
            assert_eq!(
                cell.bg, panel_bg,
                "orphan image label must not get a pill band"
            );
        }
    }

    /// Selecting a chip keeps its identity color (so the user can still tell
    /// which pasted block is selected) but the selection wins on background —
    /// the highlighted slice stays a uniform `selected_bg`.
    #[test]
    fn draw_composer_chip_keeps_identity_color_under_selection() {
        let theme = Theme::default();
        let paste_chip = crate::composer_attachments::paste_chip(1, 3, 2048);
        let input = format!("see {paste_chip} end");
        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 5);
        // Select exactly the chip bytes (absolute offsets into `input`).
        let sel_lo = "see ".len();
        let sel_hi = sel_lo + paste_chip.len();
        use crate::model::layout::SemanticCursor;
        let selection = SelectionState::Range {
            anchor: SemanticCursor::new(crate::composer::INPUT_MSG_IDX, 0, sel_lo),
            head: SemanticCursor::new(crate::composer::INPUT_MSG_IDX, 0, sel_hi),
        };
        terminal.draw(|f| {
            draw_composer(
                f,
                Rect::new(0, 0, 80, 3),
                &input,
                input.len(),
                true,
                false,
                &theme,
                &mut LayoutMap::new(),
                false,
                &mut 0,
                &selection,
                0,
                1,
            );
        });
        let buf = terminal.buffer();
        let text_y = crate::design::COMPOSER_TEXT_ROW_OFFSET;
        let text_x = COMPOSER_PROMPT_PREFIX_COLS as u16;
        let chip_start = text_x + sel_lo as u16;
        let chip_end = chip_start + neenee_tui_engine::text::str_len(&paste_chip) as u16;
        for col in chip_start..chip_end {
            let cell = buf.get(col, text_y).expect("selected chip cell");
            assert_eq!(
                cell.fg,
                theme.chip_paste_fg(),
                "selected chip must keep its identity color"
            );
            assert_eq!(
                cell.bg,
                theme.selected(),
                "selection must win the background"
            );
        }
    }

    /// A chip split across a wrap boundary paints both fragments with the
    /// same pill, so a pasted block stays visually contiguous as it wraps
    /// inside the input box.
    #[test]
    fn draw_composer_chip_pill_continues_across_wrap() {
        let theme = Theme::default();
        let image_chip = crate::composer_attachments::image_chip(1, 1536);
        // Narrow text area (16 - 2 prefix - 2 pad = 12 cols) forces the
        // `[Image #1 · 1.5 KB]` label onto its own wrapped fragment.
        let input = format!("xx {image_chip} yy");
        let mut terminal = neenee_tui_engine::TestTerminal::new(16, 6);
        terminal.draw(|f| {
            draw_composer(
                f,
                Rect::new(0, 0, 16, 5),
                &input,
                input.len(),
                true,
                true,
                &theme,
                &mut LayoutMap::new(),
                false,
                &mut 0,
                &SelectionState::None,
                1,
                0,
            );
        });
        let buf = terminal.buffer();
        let panel_bg = theme.input_surface();
        // Scan every rendered row: every glyph that belongs to the chip label
        // (ignoring spaces, which also appear in the prompt indent and the
        // panel padding) must carry the chip band, proving the pill survives
        // the wrap instead of reverting to plain text on the continuation row.
        let chip_glyphs: Vec<char> = image_chip.chars().filter(|c| *c != ' ').collect();
        for row in 0..5u16 {
            for col in 0..16u16 {
                let cell = buf.get(col, row).expect("row cell");
                if chip_glyphs.contains(&cell.symbol().chars().next().unwrap_or('\0')) {
                    assert_eq!(
                        cell.bg,
                        theme.chip_image_bg(panel_bg),
                        "wrapped chip fragment at ({col},{row}) lost its band"
                    );
                    assert_eq!(
                        cell.fg,
                        theme.chip_image_fg(),
                        "wrapped chip fragment at ({col},{row}) lost its amber"
                    );
                }
            }
        }
    }

    /// Regression for the IME cursor-lag fix: the input-driven immediate flush
    /// places the terminal cursor via [`composer::cursor_screen_pos`], and the
    /// draw path places it via [`draw_composer`]'s `set_cursor_position`. The
    /// two **must agree** for every (input, caret offset) pair — if they ever
    /// diverge, the IME composition window (which samples the cursor on its own
    /// schedule) anchors to a different coordinate than the rendered caret, the
    /// exact "IME 捕获位置错乱" symptom. This test locks the invariant by
    /// rendering each case and asserting the rendered cursor equals the pure
    /// function's output.
    #[test]
    fn cursor_screen_pos_matches_drawn_caret() {
        use super::composer::cursor_screen_pos;

        let theme = Theme::default();
        // Composer rect must fit inside the test terminal (24×8): a 4-row box
        // at y=0..4, x=0..20.
        let rect = Rect::new(0, 0, 20, 4);

        // (label, input, byte cursor) spanning ASCII, CJK (wide), mid-string,
        // empty, and a cursor that rests past the last wrapped line.
        let cases: &[(&str, &str, usize)] = &[
            ("ascii end", "hello", 5),
            ("ascii mid", "hello", 2),
            ("empty", "", 0),
            ("cjk end", "中文测试", 12),
            ("cjk mid", "中文测试", 6),
            ("mixed", "a中b文", 5),
            ("past wrap", "aaaa bbbb cccc dd", 16),
        ];

        for (label, input, byte_cursor) in cases {
            let byte_cursor = *byte_cursor;
            // What the draw path places.
            let mut terminal = neenee_tui_engine::TestTerminal::new(24, 8);
            terminal.draw(|f| {
                draw_composer(
                    f,
                    rect,
                    input,
                    byte_cursor,
                    true,
                    true,
                    &theme,
                    &mut LayoutMap::new(),
                    false,
                    &mut 0,
                    &SelectionState::None,
                    0,
                    0,
                );
            });
            let drawn = match terminal.cursor() {
                neenee_tui_engine::CursorState::Visible(x, y) => (x, y),
                other => panic!("{label}: caret should be visible, got {other:?}"),
            };

            // What the immediate-flush pure function places.
            let mut scroll = 0usize;
            let flushed = cursor_screen_pos(rect, input, byte_cursor, &mut scroll)
                .unwrap_or_else(|| panic!("{label}: cursor_screen_pos returned None"));

            assert_eq!(
                drawn, flushed,
                "{label} (input={input:?}, byte={byte_cursor}): \
                 draw path and immediate-flush path disagree — \
                 this is what re-introduces the IME anchor drift"
            );
        }
    }

    /// The immediate flush must update `input_scroll` to keep the caret in view
    /// exactly as the draw path does — otherwise a caret moved below the
    /// visible window would render at the right place but be anchored
    /// off-screen by the flush, desyncing scroll state across frames.
    #[test]
    fn cursor_screen_pos_clamps_scroll_like_draw() {
        use super::composer::cursor_screen_pos;

        // A 20-wide box (text width ~16) with a long input; the box shows only
        // a couple of rows, so a caret near the end forces a scroll.
        let rect = Rect::new(0, 0, 20, 4);
        let input = "word ".repeat(20); // ~100 chars, wraps many times
        let byte_cursor = input.len();

        let mut scroll = 0usize;
        let flushed = cursor_screen_pos(rect, &input, byte_cursor, &mut scroll)
            .expect("caret position resolves");

        // The flushed caret must sit on a visible row (within the box's text
        // rows), proving scroll advanced to track it.
        let visible_rows = (rect.height as usize)
            .saturating_sub(crate::design::COMPOSER_VERTICAL_CHROME_ROWS as usize)
            .max(1);
        let caret_row = (flushed.1 - rect.y - crate::design::COMPOSER_TEXT_ROW_OFFSET) as usize;
        assert!(
            caret_row < visible_rows,
            "flushed caret row {caret_row} outside the {visible_rows} visible rows"
        );
        assert!(scroll > 0, "scroll should have advanced to track the caret");
    }

    /// (head + continuation), cover exactly the selected glyphs, and leave the
    /// trailing pad on the panel background — no extra glyph, no half-highlighted
    /// wide char. Exercises the full-3-CJK selection the live bug report used.
    #[test]
    fn composer_cjk_selection_covers_full_width_glyphs() {
        use crate::model::layout::SemanticCursor;
        let theme = Theme::default();
        let panel_bg = theme.input_surface();
        let sel_bg = theme.selected();
        let input = "中文测"; // 3 wide glyphs = 6 cols (cols 2..8)
        // Select all three. Head points AT 测 (byte 6); the inclusive-head model
        // includes the glyph under the head, so the range is [0, 9) = "中文测".
        let sel = SelectionState::Range {
            anchor: SemanticCursor::new(INPUT_MSG_IDX, 0, 0),
            head: SemanticCursor::new(INPUT_MSG_IDX, 0, 6),
        };
        let mut terminal = neenee_tui_engine::TestTerminal::new(20, 5);
        terminal.draw(|f| {
            draw_composer(
                f,
                Rect::new(0, 0, 20, 4),
                input,
                input.len(),
                true,
                false,
                &theme,
                &mut LayoutMap::new(),
                false,
                &mut 0,
                &sel,
                0,
                0,
            );
        });
        let g = terminal.buffer();
        let y = crate::design::COMPOSER_TEXT_ROW_OFFSET;
        // Cols: 0='›', 1=gap, 2-7='中文测'(sel), 8+=panel tail.
        for (col, label, expect_sel) in [
            (2usize, "中 head", true),
            (3, "中 cont", true),
            (4, "文 head", true),
            (5, "文 cont", true),
            (6, "测 head", true),
            (7, "测 cont", true),
            (8, "tail 0", false),
            (9, "tail 1", false),
        ] {
            let cell = g.get(col as u16, y).unwrap();
            let want = if expect_sel { sel_bg } else { panel_bg };
            assert_eq!(
                cell.bg, want,
                "{label} at col {col}: bg {:?} expected {:?}",
                cell.bg, want
            );
        }
        // While a selection is active the caller passes `show_caret = false`
        // (see the event loop), so no terminal caret is placed on top of the
        // highlighted glyphs — the "appended flickering character" symptom.
        assert!(
            matches!(terminal.cursor(), neenee_tui_engine::CursorState::Hidden),
            "caret must be hidden while a selection is active"
        );
    }

    #[test]
    fn composer_two_cjk_select_all_has_no_extra_glyph_or_tail_highlight() {
        use crate::model::layout::SemanticCursor;

        let theme = Theme::default();
        let panel_bg = theme.input_surface();
        let sel_bg = theme.selected();
        let input = "你好";
        let sel = SelectionState::Range {
            anchor: SemanticCursor::new(INPUT_MSG_IDX, 0, 0),
            head: SemanticCursor::new(INPUT_MSG_IDX, 0, input.len()),
        };
        let mut terminal = neenee_tui_engine::TestTerminal::new(16, 5);

        terminal.draw(|f| {
            draw_composer(
                f,
                Rect::new(0, 0, 16, 4),
                input,
                input.len(),
                true,
                false,
                &theme,
                &mut LayoutMap::new(),
                false,
                &mut 0,
                &sel,
                0,
                0,
            );
        });

        let y = crate::design::COMPOSER_TEXT_ROW_OFFSET;
        let buffer = terminal.buffer();

        assert_eq!(buffer.get(2, y).unwrap().symbol(), "你");
        assert_eq!(buffer.get(2, y).unwrap().width, 2);
        assert_eq!(buffer.get(3, y).unwrap().symbol(), " ");
        assert_eq!(buffer.get(3, y).unwrap().width, 0);
        assert_eq!(buffer.get(4, y).unwrap().symbol(), "好");
        assert_eq!(buffer.get(4, y).unwrap().width, 2);
        assert_eq!(buffer.get(5, y).unwrap().symbol(), " ");
        assert_eq!(buffer.get(5, y).unwrap().width, 0);
        assert_eq!(
            buffer.get(6, y).unwrap().symbol(),
            " ",
            "tail cell must not contain a duplicate glyph"
        );

        for col in 2..=5 {
            assert_eq!(
                buffer.get(col, y).unwrap().bg,
                sel_bg,
                "col {col} should be selected"
            );
        }
        assert_eq!(
            buffer.get(6, y).unwrap().bg,
            panel_bg,
            "tail cell must remain on input panel background"
        );
        assert!(
            matches!(terminal.cursor(), neenee_tui_engine::CursorState::Hidden),
            "caret must be hidden while a selection is active"
        );
    }

    /// Regression for the input-select bug: a click that starts a selection
    /// (anchor == head, a collapsed range) must highlight NOTHING, and a drag
    /// through the real click pipeline (layout_map → cursor_at) must highlight
    /// exactly the dragged glyphs with the correct background. The prior
    /// `inclusive_grapheme_end`-on-a-point logic lit up one glyph on every
    /// click and flickered as the drag moved — "an extra changing character
    /// appears and the selection background misbehaves".
    #[test]
    fn composer_collapsed_click_highlights_nothing_drag_highlights_cleanly() {
        let theme = Theme::default();
        let panel_bg = theme.input_surface();
        let sel_bg = theme.selected();
        let input = "中文测";
        let rect = Rect::new(0, 0, 20, 4);
        let text_row = crate::design::COMPOSER_TEXT_ROW_OFFSET;

        // Record input regions so cursor_at can resolve real drag positions.
        let mut layout_map = LayoutMap::new();
        let mut rec = neenee_tui_engine::TestTerminal::new(20, 5);
        rec.draw(|f| {
            draw_composer(
                f,
                rect,
                input,
                input.len(),
                true,
                false,
                &theme,
                &mut layout_map,
                true,
                &mut 0,
                &SelectionState::None,
                0,
                0,
            );
        });
        let anchor = layout_map.cursor_at(rect.x + 2, rect.y + text_row).unwrap();
        assert_eq!(anchor.byte_offset, 0);

        fn row_bgs(
            input: &str,
            rect: Rect,
            text_row: u16,
            theme: &Theme,
            sel: &SelectionState,
        ) -> Vec<neenee_tui_engine::Color> {
            let mut t = neenee_tui_engine::TestTerminal::new(20, 5);
            t.draw(|f| {
                draw_composer(
                    f,
                    rect,
                    input,
                    input.len(),
                    true,
                    false,
                    theme,
                    &mut LayoutMap::new(),
                    false,
                    &mut 0,
                    sel,
                    0,
                    0,
                );
            });
            (0..10u16)
                .map(|c| t.buffer().get(c, text_row).unwrap().bg)
                .collect()
        }

        // 1) Collapsed click (anchor == head): no glyph may carry the selection bg.
        let collapsed = SelectionState::Range {
            anchor,
            head: anchor,
        };
        for (col, bg) in row_bgs(input, rect, text_row, &theme, &collapsed)
            .into_iter()
            .enumerate()
        {
            assert_ne!(bg, sel_bg, "collapsed click lit up col {col}");
            let _ = panel_bg;
        }

        // 2) Drag onto 测's first column (byte 6): inclusive head selects all
        //    three glyphs; the trailing pad stays on the panel bg.
        let head = layout_map.cursor_at(rect.x + 6, rect.y + text_row).unwrap();
        assert_eq!(head.byte_offset, 6);
        let drag = SelectionState::Range { anchor, head };
        let bgs = row_bgs(input, rect, text_row, &theme, &drag);
        // cols 0,1 = prefix; 2..8 = "中文测" (selected); 8,9 = tail (panel).
        for (col, &bg) in bgs[2..8].iter().enumerate() {
            assert_eq!(bg, sel_bg, "col {} should be selected", col + 2);
        }
        for (col, &bg) in bgs[8..10].iter().enumerate() {
            assert_eq!(bg, panel_bg, "col {} should be panel tail", col + 8);
        }

        // 3) Drag to the second visual column of 中. The hit-test cursor maps
        // both columns of a wide glyph to that glyph's byte start; with an
        // inclusive head this selects 中 only, not the next glyph.
        let head = layout_map.cursor_at(rect.x + 3, rect.y + text_row).unwrap();
        assert_eq!(head.byte_offset, 1);
        let drag = SelectionState::Range { anchor, head };
        let bgs = row_bgs(input, rect, text_row, &theme, &drag);
        for (col, &bg) in bgs[2..4].iter().enumerate() {
            assert_eq!(bg, sel_bg, "col {} should select 中", col + 2);
        }
        for (col, &bg) in bgs[4..8].iter().enumerate() {
            assert_eq!(bg, panel_bg, "col {} should remain unselected", col + 4);
        }
    }

    #[test]
    fn user_message_and_composer_keep_symmetric_panel_padding() {
        let theme = Theme::default();
        let user_bg = theme.user_surface();
        let input_bg = theme.input_surface();
        let app_bg = theme.surface();
        let width = 60u16;
        let mut terminal = neenee_tui_engine::TestTerminal::new(width, 24);

        // A long user message fills the first wrapped line edge to edge, so the
        // right-side padding is only present if the wrap width reserves it.
        let messages = vec![TranscriptMessage::new(
            neenee_contracts::Role::User,
            "x".repeat(200),
        )];
        let long_input = "y".repeat(200);

        terminal.draw(|f| {
            let mut layout_map = LayoutMap::new();
            // draw_transcript only computes the input box geometry; the composer
            // itself is drawn separately (as the live app does), using the
            // returned input_rect.
            let render = draw_transcript(
                f,
                &mut layout_map,
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: &long_input,
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    envoy_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
            let mut input_scroll = 0;
            draw_composer(
                f,
                render.input_rect,
                &long_input,
                0,
                true,
                true,
                &theme,
                &mut layout_map,
                false,
                &mut input_scroll,
                &SelectionState::None,
                0,
                0,
            );
        });

        let buffer = terminal.buffer();

        // Find the first user-message text row. Layout (60-col terminal):
        //   cols 0,1  = global app_bg (viewport margin)
        //   cols 2,3  = user_panel_bg inner pad (USER_MESSAGE_TEXT_GAP_COLS)
        //   col  4+   = text
        let user_row = (0..buffer.area().height)
            .find(|&y| {
                let c4 = &buffer[(4, y)];
                c4.symbol() == "x" && c4.bg == user_bg
            })
            .expect("user message row exists");

        // Left: 2-col app_bg outer gutter (viewport margin + entry inset),
        // then 2-col user_panel_bg inner pad.
        assert_eq!(buffer[(0, user_row)].bg, app_bg, "left outer gutter");
        assert_eq!(buffer[(1, user_row)].bg, app_bg, "left outer gutter");
        assert_eq!(
            buffer[(2, user_row)].bg,
            user_bg,
            "left inner padding must be user_panel_bg"
        );
        assert_eq!(
            buffer[(3, user_row)].bg,
            user_bg,
            "left inner padding is 2 cols, not 1"
        );
        assert_eq!(buffer[(4, user_row)].symbol(), "x", "text starts at col 4");

        // Right: 2-col user_panel_bg inner pad, then 2-col app_bg outer gutter.
        // user_text_width = (band_w) - (TEXT_GAP + RIGHT_PAD) = (60-4) - 4 = 52
        // -> text fills cols 4..56.
        assert_eq!(
            buffer[(56, user_row)].symbol(),
            " ",
            "right inner padding must stay clear of wrapped text"
        );
        assert_eq!(buffer[(56, user_row)].bg, user_bg, "right inner padding");
        assert_eq!(buffer[(57, user_row)].bg, user_bg, "right inner padding");
        assert_eq!(buffer[(58, user_row)].bg, app_bg, "right outer gutter");
        assert_eq!(buffer[(59, user_row)].bg, app_bg, "right outer gutter");

        // Composer: the input panel starts at x = FOOTER_H_INSET (2). `›` at
        // x=2, text from x=4, and a 2-col right pad in the input box's active
        // background before the app_bg gutter at the far right.
        let composer_row = (0..buffer.area().height)
            .find(|&y| {
                let c4 = &buffer[(4, y)];
                c4.symbol() == "y" && c4.bg == input_bg
            })
            .expect("composer row exists");
        assert_eq!(buffer[(2, composer_row)].symbol(), "›", "composer prompt");
        assert_eq!(
            buffer[(4, composer_row)].symbol(),
            "y",
            "composer text starts at col 4"
        );
        // full_w (composer panel) = 60 - 2*FOOTER_H_INSET = 56, panel spans
        // x=2..58. Right pad at x=56,57 (input_bg), gutter x=58,59 (app_bg).
        assert_eq!(
            buffer[(56, composer_row)].bg,
            input_bg,
            "composer right inner padding"
        );
        assert_eq!(
            buffer[(57, composer_row)].bg,
            input_bg,
            "composer right inner padding"
        );
        assert_eq!(
            buffer[(58, composer_row)].bg,
            app_bg,
            "composer right outer gutter"
        );
        assert_eq!(
            buffer[(59, composer_row)].bg,
            app_bg,
            "composer right outer gutter"
        );
    }

    /// The input box owns two dedicated background tokens — active (the box
    /// owns the keyboard) and inactive (a transcript step owns it). Both must
    /// render as full panels and the two states must be visibly different
    /// colors, so "where does typing land" is legible from luminance alone
    /// and neither state melts into the app background. Regression guard for
    /// the activated/deactivated input being indistinguishable.
    #[test]
    fn composer_focused_and_unfocused_panels_render_distinct_backgrounds() {
        let theme = Theme::default();
        let active_bg = theme.input_surface();
        let inactive_bg = theme.input_surface_inactive();
        let app_bg = theme.surface();
        assert_ne!(active_bg, inactive_bg, "pair must be distinct colors");

        let panel_bg_at = |focused: bool| -> neenee_tui_engine::Color {
            let mut terminal = neenee_tui_engine::TestTerminal::new(30, 5);
            terminal.draw(|f| {
                draw_composer(
                    f,
                    Rect::new(0, 0, 30, 3),
                    "hello",
                    5,
                    focused,
                    false,
                    &theme,
                    &mut LayoutMap::new(),
                    false,
                    &mut 0,
                    &SelectionState::None,
                    0,
                    0,
                );
            });
            let buffer = terminal.buffer();
            // A point inside the panel: the top padding row is painted
            // unconditionally, so it carries the panel background.
            let cell = &buffer[(0, 0)];
            assert_eq!(cell.symbol(), " ", "top padding row must be blank");
            cell.bg
        };

        let rendered_active = panel_bg_at(true);
        let rendered_inactive = panel_bg_at(false);
        assert_eq!(
            rendered_active, active_bg,
            "focused box must paint the input-active background"
        );
        assert_eq!(
            rendered_inactive, inactive_bg,
            "unfocused box must paint the input-inactive background"
        );
        assert_ne!(
            rendered_active, app_bg,
            "focused box must not melt into the app background"
        );
        assert_ne!(
            rendered_inactive, app_bg,
            "unfocused box must not melt into the app background"
        );
        assert_ne!(
            rendered_inactive,
            theme.user_surface(),
            "the inactive input is its own token, not the sent-user-message panel"
        );
    }

    /// A queued user message (one staged in the send queue waiting for the
    /// in-flight turn to finish) must render with the dimmer
    /// `user_panel_bg_queued` band and a visible "⏸ Queued" badge so the user
    /// can tell their message is pending, not delivered.
    #[test]
    fn queued_user_message_renders_badge_and_dimmer_bg() {
        let theme = Theme::default();
        let _queued_bg = theme.user_surface_queued();
        let delivered_bg = theme.user_surface();
        let width = 40u16;
        let mut terminal = neenee_tui_engine::TestTerminal::new(width, 20);

        let messages = vec![
            TranscriptMessage::new(neenee_contracts::Role::User, "first queued").queued(),
            TranscriptMessage::new(neenee_contracts::Role::User, "second queued").queued(),
        ];

        terminal.draw(|f| {
            let mut layout_map = LayoutMap::new();
            let _ = draw_transcript(
                f,
                &mut layout_map,
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    envoy_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
        });

        let buffer = terminal.buffer();

        // Both queued panels must carry the queued bg, never the delivered bg.
        // Scan the inner-pad columns (2,3) of every row for any cell painted
        // with the delivered bg — that would mean a queued message leaked the
        // wrong surface.
        for y in 0..buffer.area().height {
            for x in 2..4 {
                let bg = buffer[(x, y)].bg;
                assert_ne!(
                    bg, delivered_bg,
                    "queued panels must never carry the delivered bg, found at ({},{})",
                    x, y
                );
            }
        }

        // Each queued user message renders one "⏸ Queued" badge row OUTSIDE
        // the panel (on plain `surface`, above the panel's top transition).
        // The badge is the paused glyph at the text column, on a surface row.
        let badge_count = (0..buffer.area().height)
            .filter(|&y| buffer[(4, y)].symbol() == "⏸")
            .count();
        assert_eq!(
            badge_count, 2,
            "each queued user message must render one badge row, got {}",
            badge_count
        );
    }

    /// The transcript content rect must be recorded after rendering so that
    /// clicks on gap rows (which carry no region) still switch keyboard focus
    /// to Browse. It must span the horizontal band inside the outer gutters
    /// (clicks in the gutters are not transcript clicks) and the vertical
    /// extent of drawn content, including the inter-message gap row.
    #[test]
    fn transcript_content_rect_spans_band_and_gap_rows() {
        let theme = Theme::default();
        let width = 40u16;
        let mut terminal = neenee_tui_engine::TestTerminal::new(width, 24);
        // Two assistant text messages so a `MESSAGE_GAP_ROWS` blank row is
        // emitted between them — that row is rendered but never registered.
        let messages = vec![
            TranscriptMessage::new(neenee_contracts::Role::Assistant, "first".to_string()),
            TranscriptMessage::new(neenee_contracts::Role::Assistant, "second".to_string()),
        ];
        let mut layout_map = LayoutMap::new();
        terminal.draw(|f| {
            draw_transcript(
                f,
                &mut layout_map,
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    envoy_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
        });

        let rect = layout_map
            .transcript_content_rect()
            .expect("content rect must be recorded when messages are drawn");
        // Horizontal band excludes the outer `TRANSCRIPT_H_INSET` gutters.
        assert_eq!(rect.x, TRANSCRIPT_H_INSET);
        assert_eq!(rect.width, width - 2 * TRANSCRIPT_H_INSET);

        // The whole point of the rect: a gap row between the two messages is
        // rendered but carries no region (clicking it does not resolve to a
        // cursor). It must still fall inside the content rect so the click
        // handler can switch focus to Browse.
        let gap_y = (rect.y..rect.y + rect.height)
            .find(|&y| layout_map.region_at(rect.x, y).is_none())
            .expect("there must be at least one unregistered gap row between the two messages");
        assert!(rect.y <= gap_y && gap_y < rect.y + rect.height);
    }

    /// Wide tables (including CJK content) must keep borders intact and never
    /// overflow the viewport: columns shrink to fit, cell text wraps, and
    /// every rendered line stays within the available width.
    #[test]
    fn wide_table_shrinks_columns_and_keeps_borders_intact() {
        use crate::model::document::TableAlignment;

        let headers = vec![
            "Tool".to_string(),
            "Type".to_string(),
            "Implementation".to_string(),
            "Key Feature".to_string(),
        ];
        let rows = vec![
            vec![
                "bash".to_string(),
                "Write".to_string(),
                "std::process::Command (sh -c / cmd /C)".to_string(),
                "execute shell command, supports timeout, truncates output".to_string(),
            ],
            vec![
                "read_text".to_string(),
                "Read".to_string(),
                "std::fs::read_to_string".to_string(),
                "supports offset/limit".to_string(),
            ],
        ];
        let aligns = vec![
            TableAlignment::None,
            TableAlignment::None,
            TableAlignment::None,
            TableAlignment::None,
        ];

        // ── Narrow terminal (34 cols): table is far wider, must shrink ──
        let lines = build_table_render(&headers, &rows, &aligns, 34).lines;
        assert!(!lines.is_empty(), "table must produce output");

        for (i, line) in lines.iter().enumerate() {
            assert!(
                line.width() <= 34,
                "line {i} overflows: {} cols: {}",
                line.width(),
                line
            );
        }
        assert!(lines.first().unwrap().starts_with('┌'));
        assert!(lines.last().unwrap().starts_with('└'));
        assert!(
            lines.iter().any(|l| l.starts_with('├')),
            "missing header/body separator"
        );
        // Two body rows → one separator between them (plus one after header).
        let sep_count = lines.iter().filter(|l| l.starts_with('├')).count();
        assert_eq!(
            sep_count, 2,
            "expected 2 separators (header→body + row→row), got {sep_count}"
        );
        let pipe_counts: Vec<usize> = lines
            .iter()
            .filter(|l| l.starts_with('│'))
            .map(|l| l.matches('│').count())
            .collect();
        assert!(!pipe_counts.is_empty(), "must have data lines");
        assert!(
            pipe_counts.iter().all(|&c| c == pipe_counts[0]),
            "all data lines must have the same number of column separators"
        );

        // ── Wide terminal (80 cols): table fits without shrinking ──
        let wide_lines = build_table_render(&headers, &rows, &aligns, 76).lines;
        for (i, line) in wide_lines.iter().enumerate() {
            assert!(
                line.width() <= 76,
                "wide line {i} overflows: {} cols",
                line.width()
            );
        }
        // When it fits, the table should be shorter (no wrapping needed).
        assert!(
            wide_lines.len() <= lines.len(),
            "wide table should have fewer lines than shrunk table"
        );
    }

    /// Ragged body rows (fewer cells than the header, and more) must not panic
    /// the adaptive renderer and must still produce a rectangular grid: every
    /// data line carries the same number of `│` column separators. Regression
    /// test for the `index out of bounds: the len is 1 but the index is 1`
    /// panic at `markdown_table.rs` (`cell_styles[i]`) caused by a body row
    /// with a single cell in a two-column table.
    #[test]
    fn table_render_handles_ragged_rows_without_panicking() {
        use crate::model::document::TableAlignment;

        let headers = vec!["A".to_string(), "B".to_string()];
        // 0, 1, 2, and 3 cells — exercises both the under- and over-wide paths.
        let rows = vec![
            vec![],
            vec!["only".to_string()],
            vec!["x".to_string(), "y".to_string()],
            vec!["p".to_string(), "q".to_string(), "r".to_string()],
        ];
        let aligns = vec![TableAlignment::None, TableAlignment::None];

        let table = build_table_render(&headers, &rows, &aligns, 40);
        assert!(!table.lines.is_empty(), "ragged table must still render");

        // Every data line must have the same number of column separators, i.e.
        // the grid stays rectangular regardless of input raggedness.
        let pipe_counts: Vec<usize> = table
            .lines
            .iter()
            .filter(|l| l.starts_with('│'))
            .map(|l| l.matches('│').count())
            .collect();
        assert!(!pipe_counts.is_empty(), "must have data lines");
        assert!(
            pipe_counts.iter().all(|&c| c == pipe_counts[0]),
            "ragged rows produced uneven column counts: {pipe_counts:?}"
        );

        // Every data line carries per-cell geometry for exactly `ncols` cells,
        // so hit-testing / selection never indexes out of bounds.
        for info in table.line_info.iter().flatten() {
            assert_eq!(
                info.col_spans.len(),
                2,
                "each data line must describe exactly 2 cells"
            );
        }
    }

    /// Inline-code / bold markup delimiters (`` ` ``, `**`) are rendered at zero
    /// width, so a column holding markup must be sized and wrapped by its
    /// *visible* width — otherwise the column is inflated, the wrapped text can
    /// split a `` `…` ``/`**…**` pair across lines, and data-row `│` separators
    /// drift out of line with the border grid. A plain table and a markup table
    /// carrying the same visible content must therefore share identical borders
    /// and the same line count (no spurious wrap).
    #[test]
    fn table_markup_columns_size_to_visible_width() {
        use crate::model::document::TableAlignment;

        let plain = build_table_render(
            &["a".to_string(), "b".to_string()],
            &[vec!["bold".to_string(), "code".to_string()]],
            &[TableAlignment::None, TableAlignment::None],
            80,
        );
        let markup = build_table_render(
            &["a".to_string(), "b".to_string()],
            &[vec!["**bold**".to_string(), "`code`".to_string()]],
            &[TableAlignment::None, TableAlignment::None],
            80,
        );

        // Borders are markup-free, so plain and markup grids must match exactly
        // once columns are sized to visible width.
        let plain_borders: Vec<&String> =
            plain.lines.iter().filter(|l| !l.starts_with('│')).collect();
        let markup_borders: Vec<&String> = markup
            .lines
            .iter()
            .filter(|l| !l.starts_with('│'))
            .collect();
        assert_eq!(
            plain_borders, markup_borders,
            "markup must not inflate column width"
        );

        // The markup cell fits its column on a single line (no delimiter split):
        // same number of data lines as the plain version.
        let plain_data = plain.lines.iter().filter(|l| l.starts_with('│')).count();
        let markup_data = markup.lines.iter().filter(|l| l.starts_with('│')).count();
        assert_eq!(
            plain_data, markup_data,
            "markup must not introduce extra wrapped lines"
        );
    }

    #[test]
    fn shrink_columns_preserves_minimum_and_proportions() {
        // Intrinsic [10, 5, 20], target 24, min 3.
        // total_min = 9, shrinkable = 26, available = 15.
        // col0: 3 + 7*15/26 = 3 + 4 = 7
        // col1: 3 + 2*15/26 = 3 + 1 = 4
        // col2: 3 + 17*15/26 = 3 + 9 = 12
        let result = shrink_column_widths(&[10, 5, 20], 24, 3);
        assert_eq!(result.len(), 3);
        assert!(result.iter().all(|&w| w >= 3), "must respect minimum");
        assert!(
            result.iter().sum::<usize>() <= 24,
            "must fit within target, got {}",
            result.iter().sum::<usize>()
        );
        // Largest intrinsic column stays largest after shrinking.
        let max_val = *result.iter().max().unwrap();
        let max_idx = result.iter().position(|&v| v == max_val).unwrap();
        assert_eq!(max_idx, 2);
    }

    #[test]
    fn shrink_columns_with_tiny_target_returns_all_minimum() {
        let result = shrink_column_widths(&[10, 20, 30], 5, 3);
        assert_eq!(result, vec![3, 3, 3]);
    }

    /// Drive `draw_history_panel` against a real buffer across every input
    /// state the Ctrl+R picker can land in. The assertions are deliberately
    /// structural ("does not panic, produces a non-empty frame") because the
    /// fuzzy highlight math is already covered by `fuzzy::tests`; here we
    /// only need to prove the renderer consumes each state without exploding.
    #[test]
    fn history_panel_renders_every_query_state() {
        let theme = Theme::default();
        let history: Vec<neenee_contracts::HistoryEntry> = [
            "git status",
            "git commit -am 'ship it'",
            "cargo test",
            "review the diff before sending",
        ]
        .into_iter()
        .enumerate()
        .map(|(i, text)| {
            neenee_contracts::HistoryEntry::new(
                text.to_string(),
                Some(format!("s{i}")),
                Some("~/p".to_string()),
                (i as u64) * 1_000,
            )
        })
        .collect();
        let texts: Vec<&str> = history.iter().map(|e| e.text.as_str()).collect();

        let cases: &[(&str, usize)] = &[
            ("", history.len()), // empty query → everything surfaces
            ("git", 2),          // partial match → subset with highlights
            ("zzz", 0),          // no subsequence → empty placeholder
        ];

        let input_rect = neenee_tui_engine::Rect::new(0, 22, 80, 2);
        for (query, expected_matches) in cases {
            let mut terminal = neenee_tui_engine::TestTerminal::new(80, 24);
            let mut ranked = crate::fuzzy::rank(&texts, query);
            crate::fuzzy::sort_by_score(&mut ranked);
            assert_eq!(
                ranked.len(),
                *expected_matches,
                "query {:?} should surface {} entries",
                query,
                expected_matches
            );
            terminal.draw(|f| {
                let _ = draw_history_panel(
                    f, &history, &ranked, 0, &mut 0, true, false, false, input_rect, 0, &theme,
                );
            });
        }

        // Empty history must render the "(no history yet)" placeholder rather
        // than indexing into an empty slice.
        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 24);
        let empty: Vec<neenee_contracts::HistoryEntry> = Vec::new();
        let ranked: Vec<(usize, crate::fuzzy::FuzzyMatch)> = crate::fuzzy::rank::<&str>(&[], "");
        terminal.draw(|f| {
            let _ = draw_history_panel(
                f, &empty, &ranked, 0, &mut 0, true, false, false, input_rect, 0, &theme,
            );
        });
    }

    /// A multi-line history entry collapses to its first line in the fuzzy
    /// list (so a long prompt never breaks the single-row grid), and the
    /// preview mode renders the full text verbatim. Both modes must consume a
    /// real buffer without panicking.
    #[test]
    fn history_panel_folds_multiline_and_previews_full_text() {
        let theme = Theme::default();
        let history: Vec<neenee_contracts::HistoryEntry> =
            ["first line\nsecond line\nthird line", "single line"]
                .into_iter()
                .enumerate()
                .map(|(i, text)| {
                    neenee_contracts::HistoryEntry::new(
                        text.to_string(),
                        Some(format!("s{i}")),
                        None,
                        0,
                    )
                })
                .collect();
        let texts: Vec<&str> = history.iter().map(|e| e.text.as_str()).collect();

        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 24);
        let ranked = crate::fuzzy::rank(&texts, "");
        let input_rect = neenee_tui_engine::Rect::new(0, 22, 80, 2);

        // List mode: the multi-line entry must render as one row.
        terminal.draw(|f| {
            let _ = draw_history_panel(
                f, &history, &ranked, 0, &mut 0, true, false, false, input_rect, 0, &theme,
            );
        });
        let buf = terminal.buffer();
        let has_marker = buf.content.iter().any(|c| c.symbol() == "↵");
        assert!(has_marker, "multi-line entry should show the ↵ fold marker");

        // Preview mode: the full multi-line text renders without panic.
        terminal.draw(|f| {
            let _ = draw_history_panel(
                f, &history, &ranked, 0, &mut 0, true, true, false, input_rect, 0, &theme,
            );
        });
    }

    /// The dropdown is an extension of the composer, not a fixed-size window:
    /// it collapses to the actual row count rather than reserving a fixed
    /// minimum. Two entries must produce a 4-row panel (2 rows + header +
    /// footer), not the old 6-row floor.
    #[test]
    fn history_panel_collapses_to_actual_row_count() {
        let theme = Theme::default();
        let history: Vec<neenee_contracts::HistoryEntry> = ["one", "two"]
            .into_iter()
            .enumerate()
            .map(|(i, text)| {
                neenee_contracts::HistoryEntry::new(
                    text.to_string(),
                    Some(format!("s{i}")),
                    None,
                    i as u64,
                )
            })
            .collect();
        let texts: Vec<&str> = history.iter().map(|e| e.text.as_str()).collect();
        let ranked = crate::fuzzy::rank(&texts, "");
        // Composer near the bottom of a tall terminal so room-above is not the
        // binding constraint — the row count is.
        let input_rect = neenee_tui_engine::Rect::new(0, 40, 80, 2);
        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 42);
        let mut panel: Option<neenee_tui_engine::Rect> = None;
        terminal.draw(|f| {
            panel = draw_history_panel(
                f, &history, &ranked, 0, &mut 0, true, false, false, input_rect, 0, &theme,
            )
        });
        let panel = panel.expect("panel should render with ample room above");
        // 2 entries + 4 chrome rows (top padding, header, footer, bottom
        // padding) = 6 rows. The panel still collapses to the actual row
        // count — a fixed minimum would have forced 8+ regardless of entries.
        assert_eq!(
            panel.height, 6,
            "panel must collapse to actual row count + chrome (6), not a fixed minimum"
        );
    }

    /// The dropdown shares the composer's surface language, not the permission
    /// sheet's: it opens and closes with full panel-bg padding rows and never
    /// paints a full-height brand-colored left column (which would read as
    /// selection/severity). The top and bottom rows must be solid panel
    /// background (no half-block `▄`/`▀` glyphs), and the left column must NOT
    /// be brand-colored.
    #[test]
    fn history_panel_uses_composer_padding_not_brand_column() {
        let theme = Theme::default();
        let history: Vec<neenee_contracts::HistoryEntry> = ["one", "two", "three"]
            .into_iter()
            .enumerate()
            .map(|(i, text)| {
                neenee_contracts::HistoryEntry::new(
                    text.to_string(),
                    Some(format!("s{i}")),
                    None,
                    i as u64,
                )
            })
            .collect();
        let texts: Vec<&str> = history.iter().map(|e| e.text.as_str()).collect();
        let ranked = crate::fuzzy::rank(&texts, "");
        let input_rect = neenee_tui_engine::Rect::new(0, 40, 30, 2);
        let mut terminal = neenee_tui_engine::TestTerminal::new(30, 42);
        let mut panel: Option<neenee_tui_engine::Rect> = None;
        terminal.draw(|f| {
            panel = draw_history_panel(
                f, &history, &ranked, 0, &mut 0, true, false, false, input_rect, 0, &theme,
            )
        });
        let panel = panel.expect("panel should render");
        let buf = terminal.buffer();

        // Top row is a full panel-bg padding row (no half-block glyph).
        let top_left = buf.get(panel.x, panel.y).expect("top-left cell");
        assert_eq!(
            top_left.bg,
            theme.panel(),
            "top edge must be a solid panel-bg row, matching the composer's padding"
        );
        assert_eq!(
            top_left.symbol(),
            " ",
            "top edge must be blank (no ▄ transition glyph)"
        );
        // Bottom row is likewise a solid panel-bg padding row.
        let bottom_left = buf
            .get(panel.x, panel.y + panel.height - 1)
            .expect("bottom-left cell");
        assert_eq!(
            bottom_left.bg,
            theme.panel(),
            "bottom edge must be a solid panel-bg row, matching the composer's padding"
        );
        assert_eq!(
            bottom_left.symbol(),
            " ",
            "bottom edge must be blank (no ▀ transition glyph)"
        );

        // No full-height brand column: the background of the left column on the
        // header row (which is never selection-tinted) must NOT be the brand
        // color. A brand column would paint every left-edge cell, including the
        // header's, with brand as its background. The header sits one row below
        // the top transition edge.
        let header_left = buf.get(panel.x, panel.y + 1).expect("header left cell");
        assert_ne!(
            header_left.bg,
            theme.brand(),
            "no full-height brand left column — the composer edge language has none"
        );
    }
    /// never grows into the activity bar's rows, so the live status surface
    /// above the composer always stays visible and always reads as above the
    /// history dropdown.
    #[test]
    fn history_panel_reserves_activity_bar_rows() {
        let theme = Theme::default();
        // Enough entries that, absent the reservation, the panel would want to
        // grow tall and run past the activity bar.
        let history: Vec<neenee_contracts::HistoryEntry> = (0..25)
            .map(|i| {
                neenee_contracts::HistoryEntry::new(
                    format!("entry {i}"),
                    Some(format!("s{i}")),
                    None,
                    i,
                )
            })
            .collect();
        let texts: Vec<&str> = history.iter().map(|e| e.text.as_str()).collect();
        let ranked = crate::fuzzy::rank(&texts, "");
        // Composer at row 15; the activity bar occupies the single row above it
        // (row 14), so `activity_height = 1`.
        let input_rect = neenee_tui_engine::Rect::new(0, 15, 80, 2);
        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 17);
        let mut panel: Option<neenee_tui_engine::Rect> = None;
        terminal.draw(|f| {
            panel = draw_history_panel(
                f, &history, &ranked, 0, &mut 0, true, false, false, input_rect, 1, &theme,
            )
        });
        let panel = panel.expect("panel should render");
        // The activity bar occupies the single row above the composer
        // (input_rect.y - 1 = 14). The panel must never cover it: its bottom
        // edge (panel.y + panel.height) must sit at or above row 14.
        assert!(
            panel.y + panel.height <= 14,
            "panel footprint [y={}, h={}] must not cover the activity bar row (14)",
            panel.y,
            panel.height
        );
    }

    /// With no messages, `draw_transcript` renders the empty-state hero in
    /// place of the stream: `content_lines` is non-zero (so the app loop does
    /// not treat it as a zero-height stream) and the call does not panic.
    #[test]
    fn empty_session_renders_empty_state_with_nonzero_height() {
        let theme = Theme::default();
        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 24);
        let messages: Vec<TranscriptMessage> = Vec::new();

        let mut render_opt: Option<TranscriptRender> = None;
        terminal.draw(|f| {
            let mut layout_map = LayoutMap::new();
            render_opt = Some(draw_transcript(
                f,
                &mut layout_map,
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "idle",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    envoy_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            ));
        });
        let render = render_opt.expect("draw_transcript must return a render");

        // The empty-state hero replaces the transcript; it occupies the logo
        // rows plus a gap, never zero, so scroll-follow logic stays honest.
        assert!(
            render.content_lines > 0,
            "empty state should report non-zero content_lines"
        );
        assert!(render.sticky.is_none(), "no sticky header on empty state");
        assert!(
            render.view_height > 0,
            "view_height should reflect the viewport, not be zero"
        );
    }

    /// A non-empty session skips the empty-state branch entirely — the hero
    /// never competes with real content.
    #[test]
    fn nonempty_session_does_not_render_empty_state() {
        let theme = Theme::default();
        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 24);
        let messages = vec![TranscriptMessage::new(
            neenee_contracts::Role::User,
            "hello",
        )];

        let mut render_opt: Option<TranscriptRender> = None;
        terminal.draw(|f| {
            let mut layout_map = LayoutMap::new();
            render_opt = Some(draw_transcript(
                f,
                &mut layout_map,
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "idle",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    envoy_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            ));
        });
        let render = render_opt.expect("draw_transcript must return a render");

        // With a real message the stream is rendered normally — content_lines
        // reflects at least one rendered message rather than the fixed
        // empty-state height.
        assert!(
            render.content_lines > 0,
            "non-empty session should render its messages"
        );
    }

    /// A user-supplied logo (from `logo.txt`) replaces the built-in wordmark
    /// on the empty state, and `content_lines` tracks its (clamped) height so
    /// scroll accounting stays honest. A four-line user logo yields seven
    /// reported lines (4 + blank gap + carousel page), distinct from the
    /// built-in wordmark's height.
    #[test]
    fn empty_session_uses_user_logo_and_reports_its_height() {
        let theme = Theme::default();
        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 24);
        let messages: Vec<TranscriptMessage> = Vec::new();
        // Four lines → reported content is 4 + 2 (gap + carousel page) = 7.
        let logo: Vec<String> = vec![
            "  N N  ".to_string(),
            " N N N ".to_string(),
            "  N N  ".to_string(),
            "       ".to_string(),
        ]
        .into_iter()
        .chain(std::iter::repeat_n("xxxxx".to_string(), 0))
        .collect();

        let mut render_opt: Option<TranscriptRender> = None;
        terminal.draw(|f| {
            let mut layout_map = LayoutMap::new();
            render_opt = Some(draw_transcript(
                f,
                &mut layout_map,
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "idle",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    envoy_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: Some(&logo),
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            ));
        });
        let render = render_opt.expect("draw_transcript must return a render");

        // 4 logo lines + 2 blank gap + 1 carousel page = 7.
        assert_eq!(
            render.content_lines, 7,
            "user-logo content_lines must be logo rows + gap + guidance rows"
        );
    }

    /// A shared harness for full-transcript renders that need to inspect the
    /// painted grid. Returns the terminal so callers can read its buffer.
    fn render_full_view(
        width: u16,
        height: u16,
        messages: &[TranscriptMessage],
        page_hints: Option<PageHints<'_>>,
    ) -> neenee_tui_engine::TestTerminal {
        let theme = Theme::default();
        let mut terminal = neenee_tui_engine::TestTerminal::new(width, height);
        let hints = page_hints;
        terminal.draw(|f| {
            let _ = draw_transcript(
                f,
                &mut LayoutMap::new(),
                TranscriptView {
                    messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    envoy_bar: None,
                    side_banner: None,
                    page_hints: hints,
                    session_head: Some(SessionHead {
                        session_id: "sess-01a2b3c4",
                        workspace: "~/projects/xx",
                        autopilot: false,
                    }),
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
        });
        terminal
    }

    fn grid_row(terminal: &neenee_tui_engine::TestTerminal, y: u16) -> String {
        let buffer = terminal.buffer();
        let width = buffer.area().width;
        (0..width).map(|x| buffer[(x, y)].symbol()).collect()
    }

    /// ADR-0104: the head band's row 2 is demand-driven. On the main view
    /// with no live asides (the common idle case) the band is a single row —
    /// the legend line stays blank and the empty-state hero moves up one row.
    #[test]
    fn main_view_without_asides_renders_a_single_row_head_band() {
        let terminal = render_full_view(
            80,
            24,
            &[],
            Some(PageHints {
                kind: PageKind::Main,
                asides: None,
                interruptible: true,
                parent_note: "",
            }),
        );
        assert!(grid_row(&terminal, 0).contains("SESSION"));
        let row1 = grid_row(&terminal, 1);
        assert!(
            row1.trim().is_empty(),
            "row 2 must stay blank without asides: {row1:?}"
        );
    }

    /// ADR-0104: with live asides the row-2 legend appears (chip + `F5
    /// asides`), and it never carries an interrupt pair — the activity bar's
    /// `Esc Esc interrupt` is the authoritative copy.
    #[test]
    fn main_view_with_asides_shows_the_legend_row() {
        let terminal = render_full_view(
            80,
            24,
            &[],
            Some(PageHints {
                kind: PageKind::Main,
                asides: Some(AsidesChip {
                    total: 2,
                    running: 1,
                }),
                interruptible: true,
                parent_note: "",
            }),
        );
        let row1 = grid_row(&terminal, 1);
        assert!(row1.contains("btw 2 · 1 running"), "chip: {row1:?}");
        assert!(row1.contains("F5"), "aside jump pair: {row1:?}");
        assert!(!row1.contains("Esc"), "no interrupt pair: {row1:?}");
        assert!(!row1.contains("F1"), "no global help pair: {row1:?}");
    }

    /// The Envoy page's row 2 never renders — its permanent footer already
    /// carries the same legend (ADR-0104), so a second copy one screen apart
    /// would be pure duplication.
    #[test]
    fn envoy_view_omits_row2_entirely() {
        let hints = PageHints {
            kind: PageKind::Envoy,
            asides: None,
            interruptible: true,
            parent_note: "",
        };
        assert!(!hints.has_content());
        let terminal = render_full_view(80, 24, &[], Some(hints));
        let row1 = grid_row(&terminal, 1);
        assert!(row1.trim().is_empty(), "row 2 blank on envoy: {row1:?}");
    }

    /// The empty-state tour renders the current carousel page beneath the
    /// logo (ADR-0104) — no static tagline, no dot indicator.
    #[test]
    fn empty_state_tour_renders_the_current_carousel_page() {
        let theme = Theme::default();
        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 24);
        let messages: Vec<TranscriptMessage> = Vec::new();
        terminal.draw(|f| {
            let _ = draw_transcript(
                f,
                &mut LayoutMap::new(),
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    envoy_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 2,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
        });
        let buffer = terminal.buffer();
        let width = buffer.area().width as usize;
        let all: Vec<String> = (0..buffer.area().height)
            .map(|y| (0..width).map(|x| buffer[(x as u16, y)].symbol()).collect())
            .collect();
        let joined = all.join("\n");
        // The static tagline is retired (ADR-0104): the carousel's first
        // page already answers "how do I start", so no duplicate line.
        assert!(
            !joined.contains("Type a message below to begin."),
            "no static tagline: {joined}"
        );
        // Page 2 of the tour is the /btw page.
        assert!(joined.contains("/btw"), "page 2 visible: {joined}");
        // No dot indicator row (ADR-0104): the carousel is a single line and
        // the rotation is self-explaining.
        assert!(!joined.contains('●'), "no dot indicator anywhere: {joined}");
    }

    /// An H1 heading renders with an UNDERLINED modifier. The underline must
    /// cover only the prefix + text cells and must not bleed into the trailing
    /// whitespace of the heading row. Inspects the rendered grid cells
    /// directly to pin the clamp in `draw_message_body`.
    #[test]
    fn h1_underline_clamps_to_text_extent() {
        let theme = Theme::default();
        let mut terminal = neenee_tui_engine::TestTerminal::new(60, 12);
        let messages = vec![TranscriptMessage::new(
            neenee_contracts::Role::Assistant,
            "# QQ_H1_TEST\n\nbody text here\n",
        )];
        terminal.draw(|f| {
            let _ = draw_transcript(
                f,
                &mut LayoutMap::new(),
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    envoy_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
        });
        let buffer = terminal.buffer();
        let width = buffer.area().width;
        let underline = neenee_tui_engine::Modifier::UNDERLINE;

        let mut head = None;
        'outer: for y in 0..buffer.area().height {
            for x in 0..width {
                if buffer[(x, y)].symbol() == "Q" {
                    head = Some((x, y));
                    break 'outer;
                }
            }
        }
        let (hx, hy) = head.expect("heading 'Q' cell exists");

        // "QQ_H1_TEST" is 10 cells; prefix is 3 cells. All 13 are underlined.
        for x in hx..hx + 10 {
            assert!(
                buffer[(x, hy)].style.add.contains(underline),
                "heading text cell at x={x} must be UNDERLINED"
            );
        }
        let trailing = hx + 10;
        assert!(trailing < width, "trailing cell within grid");
        assert!(
            !buffer[(trailing, hy)].style.add.contains(underline),
            "underline must not bleed into trailing whitespace at x={trailing}"
        );
        assert!(
            !buffer[(width - 1, hy)].style.add.contains(underline),
            "underline must not reach the right edge"
        );
    }

    /// Same clamp check with a multi-codepoint emoji grapheme (ZWJ family) in
    /// the heading: `wrap_text` measures per-char (overcounting the sequence)
    /// while the grid renders per-grapheme, so this guards the underline width
    /// against the char-vs-grapheme measurement split.
    #[test]
    fn h1_underline_clamps_with_emoji_grapheme() {
        let theme = Theme::default();
        let mut terminal = neenee_tui_engine::TestTerminal::new(60, 12);
        let messages = vec![TranscriptMessage::new(
            neenee_contracts::Role::Assistant,
            "# 👨‍👩‍👧 OKX\n\nbody\n",
        )];
        terminal.draw(|f| {
            let _ = draw_transcript(
                f,
                &mut LayoutMap::new(),
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    envoy_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
        });
        let buffer = terminal.buffer();
        let width = buffer.area().width;
        let underline = neenee_tui_engine::Modifier::UNDERLINE;

        let mut x_pos = None;
        'outer: for y in 0..buffer.area().height {
            for x in 0..width {
                if buffer[(x, y)].symbol() == "X" {
                    x_pos = Some((x, y));
                    break 'outer;
                }
            }
        }
        let (xx, xy) = x_pos.expect("heading 'X' cell exists");

        assert!(
            buffer[(xx, xy)].style.add.contains(underline),
            "heading 'X' text cell must be UNDERLINED"
        );
        let trailing = xx + 1;
        assert!(trailing < width, "trailing cell within grid");
        assert!(
            !buffer[(trailing, xy)].style.add.contains(underline),
            "underline must not bleed past emoji heading at x={trailing}"
        );
    }

    /// A wide (emoji) glyph in an H1 heading occupies a head cell plus a
    /// wide-continuation cell. The grid stores the continuation without the
    /// `add` modifiers (it is a non-emitted placeholder), but the diff skips
    /// continuations and emits the head's run style — so the backend prints
    /// the wide glyph while the UNDERLINED SGR is active, underlining both
    /// columns. This pins that emitted behavior at the `Draw`-command layer.
    #[test]
    fn h1_underline_emits_wide_glyph_in_underlined_run() {
        let theme = Theme::default();
        let width = 60u16;
        let mut terminal = neenee_tui_engine::TestTerminal::new(width, 12);
        let messages = vec![TranscriptMessage::new(
            neenee_contracts::Role::Assistant,
            "# Hello😀\n\nbody\n",
        )];
        terminal.draw(|f| {
            let _ = draw_transcript(
                f,
                &mut LayoutMap::new(),
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    envoy_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
        });
        let back = terminal.buffer();
        let front = neenee_tui_engine::Grid::new(width, 12);
        let cmd = neenee_tui_engine::diff::diff(back, &front);
        let underline = neenee_tui_engine::Modifier::UNDERLINE;

        let wide_run_style = cmd.draws.iter().find_map(|d| match d {
            neenee_tui_engine::Draw::Cells { style, cells, .. } => cells
                .iter()
                .any(|(sym, w)| sym == "😀" && *w == 2)
                .then_some(*style),
            _ => None,
        });
        let style =
            wide_run_style.expect("a Draw::Cells run containing wide glyph '😀' must be emitted");
        assert!(
            style.add.contains(underline),
            "wide glyph '😀' must be emitted in an UNDERLINED run so the terminal \
             underlines both columns, got add={:?}",
            style.add,
        );
    }

    /// Regression: a long H1 heading that wraps to multiple lines. The heading
    /// *prefix* (the leading indent on row 0 and the continuation indent
    /// on rows 1+) is decoration, not heading text, so it must NOT carry the
    /// UNDERLINED modifier. Previously the prefix shared the UNDERLINED style,
    /// which underlined the leading whitespace of every wrapped row — the
    /// underline appeared to "cross the line head" and cover the blank indent.
    ///
    /// We render a heading that wraps to ≥2 rows and assert that, on every
    /// row, the underline begins exactly at the text column (prefix width) and
    /// that the indent columns themselves are never underlined. The trailing
    /// blank columns must also stay un-underlined (the existing clamp).
    #[test]
    fn h1_underline_excludes_prefix_indent_on_wrapped_rows() {
        let theme = Theme::default();
        // Use a terminal at/above the render minimum so `draw_transcript` does
        // not trip its too-small guard. A 76-column transcript band still
        // wraps this ~95-char heading to ≥2 rows.
        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 24);
        let messages = vec![TranscriptMessage::new(
            neenee_contracts::Role::Assistant,
            "# This is a very long heading that intentionally wraps to multiple rows for the underline-prefix test\n\nbody\n",
        )];
        terminal.draw(|f| {
            let _ = draw_transcript(
                f,
                &mut LayoutMap::new(),
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    envoy_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
        });
        let buffer = terminal.buffer();
        let width = buffer.area().width;
        let underline = neenee_tui_engine::Modifier::UNDERLINE;

        // The heading prefix is "   " (3 columns); locate the heading's rows
        // as the contiguous non-blank rows at the top (before the blank gap +
        // body). The heading "This is a very long heading that wraps to
        // multiple lines" wraps to several rows here.
        let mut heading_rows: Vec<u16> = Vec::new();
        let mut found_body = false;
        for y in 0..buffer.area().height {
            let row_has_text = (0..width).any(|x| buffer[(x, y)].symbol() != " ");
            if !row_has_text {
                if !heading_rows.is_empty() {
                    found_body = true;
                }
                continue;
            }
            if found_body {
                break;
            }
            heading_rows.push(y);
        }
        assert!(
            heading_rows.len() >= 2,
            "heading must wrap to at least 2 rows, got {}",
            heading_rows.len()
        );

        for &y in &heading_rows {
            // Indent columns [0, text_start) must never be underlined.
            // The heading prefix is `TRANSCRIPT_BODY_LEADING_INDENT` cols
            // (matching body prose — see the `Block::Heading` arm), applied
            // inside the already-inset band: entry inset (TRANSCRIPT_H_INSET)
            // + heading prefix (TRANSCRIPT_BODY_LEADING_INDENT). Text starts
            // at col `TRANSCRIPT_H_INSET + TRANSCRIPT_BODY_LEADING_INDENT`.
            let text_start = super::TRANSCRIPT_H_INSET + super::TRANSCRIPT_BODY_LEADING_INDENT;
            for x in 0..text_start {
                let cell = &buffer[(x, y)];
                assert!(
                    !cell.style.add.contains(underline),
                    "indent cell at (x={x}, y={y}) must NOT be underlined \
                     (it is heading decoration, not text), symbol={:?}",
                    cell.symbol(),
                );
            }
            // The trailing blank tail (rightmost column) must not be underlined.
            let last = width - 1;
            assert!(
                !buffer[(last, y)].style.add.contains(underline),
                "trailing cell at (x={last}, y={y}) must NOT be underlined"
            );
            // And at least the first text column must be underlined (the
            // heading text itself is still underlined).
            let first_text_cell = &buffer[(text_start, y)];
            assert!(
                first_text_cell.style.add.contains(underline),
                "first heading-text cell at (x={text_start}, y={y}) must be UNDERLINED, \
                 symbol={:?}",
                first_text_cell.symbol(),
            );
        }
    }
}
