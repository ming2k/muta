//! Transient chrome around the input box: the activity bar with an animated
//! breathing-dot indicator, the one-row todo bar that surfaces the live task
//! list, the completion menu anchored above the input, and the persistent
//! model bar pinned below the input (context usage, stream rate, model
//! identity). Composer-owned meta rows (`as:` target and Enter keys) live inside
//! [`crate::composer`] and its `components::composer_hints` helpers; this bar
//! carries only ambient gauges. The activity bar (transient liveness) and the
//! todo bar (the agent's live task list) are the click targets that open the
//! Activity modal.

use mutx_engine::{
    Block as RtBlock, Clear, Color, Frame, Line, Modifier, Paragraph, Rect, Span, Style,
};
use std::time::{Duration, Instant};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::model::layout::LayoutMap;

use super::Theme;
use super::components::keycap::keycap_span;
use super::design::{
    BAR_LEGEND_GAP_MIN, JOIN_ENUMERATE_COLS, MODEL_BAR_GAP_MIN, MODEL_BAR_INNER_PADDING,
    MODEL_BAR_MODEL_GAP, MODEL_BAR_SEGMENT_GAP,
};
use super::keymap::Key;
use super::primitives::{contrast_fg, viewport_rect};

/// Number of distinct luminance steps in one breathing cycle. At the 100ms
/// spinner tick this is ~1.2s per cycle — calm, not frantic.
pub const SPINNER_PHASES: usize = 12;

/// The activity indicator glyph: a single dot whose luminance breathes (see
/// [`breathing_color`]) rather than a cycling braille frame. Replaces the old
/// braille spinner for a quieter, less busy feel. The only indicator glyph on
/// the row — the former two-cell block-density micro-meter (ADR-0154) is
/// retired: one breathing dot is the whole liveness story.
pub fn spinner_glyph() -> &'static str {
    "●"
}

/// Cosine luminance sweep between `bg` (dim, at phase 0) and `base` (bright,
/// at mid-cycle). Used with [`spinner_glyph`] so the running indicator is a
/// dot that gently breathes instead of a spinning braille glyph.
pub fn breathing_color(phase: usize, base: Color, bg: Color) -> Color {
    let (br, bgc, bb) = rgb_of(bg);
    let (fr, fgc, fb) = rgb_of(base);
    let n = SPINNER_PHASES as f32;
    let t = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * (phase % SPINNER_PHASES) as f32 / n).cos();
    Color::Rgb(lerp_u8(br, fr, t), lerp_u8(bgc, fgc, t), lerp_u8(bb, fb, t))
}

/// Which mechanism drives the activity dot this frame. Exactly one wins —
/// classified once, below, so the channels can never fight over the same
/// cells.
///
/// * [`Liveness::Breathing`] — the default while a round runs: the slow
///   clock-driven breath. With ADR-0154 the byte-luminance channel is
///   retired (one dot, one story); a held-connection stall is the annotation
///   clause's job, not the glyph's.
/// * [`Liveness::Gated`] — paused for a human decision: fully static amber.
///   Honest stillness; motion would lie about who is working.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Liveness {
    Breathing,
    Gated,
}

/// Classify the frame's dot mechanism. Gate wins over everything.
pub fn classify_liveness(awaiting_permission: bool) -> Liveness {
    if awaiting_permission {
        Liveness::Gated
    } else {
        Liveness::Breathing
    }
}

pub fn dot_color(liveness: Liveness, spinner_phase: usize, theme: &Theme) -> Color {
    match liveness {
        Liveness::Gated => theme.warning,
        Liveness::Breathing => breathing_color(spinner_phase, theme.brand(), theme.surface()),
    }
}

fn rgb_of(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (119, 125, 117), // text_muted fallback for non-truecolor
    }
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t)
        .round()
        .clamp(0.0, 255.0) as u8
}

/// Draw the transient activity bar that sits directly above the input box
/// (below the ambient todo/queue meta bars). Replaces the old inline
/// `┃ muta ⟳ <status>` indicator: the brand prefix is dropped (the header
/// already shows it) and the static `⟳` glyph is replaced by a breathing-dot
/// indicator so the harness never looks frozen.
///
/// Layout:
/// ```text
/// <spinner> <status> [<elapsed>] [· retry N/M next in Xs]   Esc Esc interrupt
/// ```
/// The transport clause is an annotation channel, not a status: it appears
/// only while a provider retry backs off, counts down live, and is the first
/// segment dropped when width runs out.
/// The whole bar is transient (turn-scoped): it shows only while a round is
/// active and is hidden while idle, so the row returns to the transcript.
/// Session-state flags such as `DELEGATED` deliberately do not live here:
/// they live on the head row at the top of the view
/// ([`crate::page_header::draw_page_header`]) so this row stays a pure activity surface. The
/// persistent task-list summary lives on its own [`draw_todo_bar`], floated
/// above this row as ambient meta-info.
///
/// `awaiting_permission` colors the status text with the warning hue so a
/// pending tool-permission decision reads as a distinct attention state rather
/// than ordinary activity. (The permission sheet replaces the input box and
/// the bars beneath it; this bar is the one live status surface that survives,
/// so marking the state here is what tells the user *why* the round is
/// paused.)
///
/// The bar surfaces what the user most wants to know mid-round — the live
/// status and how long the round has
/// run — and is the click target that opens the Activity modal for the full
/// detail. The structural counters (`round N · turn M · <model>`) live in the
/// modal: they change rarely and take space, while the bar is a glance
/// surface. Segments are omitted when there is nothing to report:
/// - elapsed only while the round timer is running;
/// - the whole bar only while a round is active.
///
/// When the status string already carries a reason (e.g.
/// `retry 1/4 in 3s · <message>`), it flows through unchanged as the lead.
///
/// Returns `Some(rect)` when the bar is drawn so the event loop can hit-test
/// clicks and open the Activity modal; `None` when the bar is hidden (idle).
pub struct ActivityBarView<'a> {
    /// Master-slot label (the typed phase's text).
    pub status: &'a str,
    /// Transport-setback clause rendered beside the label, muted. `None`
    /// when transport is healthy (silence is healthy).
    pub backoff_clause: Option<&'a str>,
    /// Stream-silence clause (`· silent 9s`, `· no tokens 52s`), armed only
    /// while a model request is open and no token has arrived within the
    /// applicable tolerance (generous for the first byte, tighter once a
    /// stream has flowed). `None` while tokens flow or the request is closed.
    pub silent_clause: Option<String>,
    /// Warning-tinted gate state (permission / ask_user pending).
    pub awaiting_permission: bool,
}

pub fn draw_activity_bar(
    frame: &mut Frame,
    rect: Rect,
    round_started_at: Option<Instant>,
    view: ActivityBarView<'_>,
    spinner_phase: usize,
    theme: &Theme,
) -> Option<Rect> {
    // The bar is a single transient LEFT segment (spinner + shimmering status
    // + elapsed/interrupt hint + pursuit) shown only while a
    // turn is active. With nothing to report it is hidden entirely.
    let ActivityBarView {
        status,
        backoff_clause,
        silent_clause,
        awaiting_permission,
    } = view;
    let status_active = !status.is_empty() && status != "idle";
    let dim = Style::default().fg(theme.muted());

    // If there is no transient activity, hide the bar — no point painting a
    // blank row. (The persistent todo summary has its own bar below.)
    if !status_active {
        return None;
    }

    let row_width = rect.width as usize;
    let available_width = row_width;
    let elapsed =
        round_started_at.map(|started| format!(" [{}]", format_elapsed(started.elapsed())));
    let full_interrupt_width = UnicodeWidthStr::width("Esc Esc interrupt");
    let key_interrupt_width = UnicodeWidthStr::width("Esc Esc");
    let prefix_width = UnicodeWidthStr::width(" ● ");
    const MIN_STATUS_WIDTH: usize = 4;
    const MIN_TINY_STATUS_WIDTH: usize = 1;
    const SEGMENT_GAP: usize = 2;
    let show_interrupt_words =
        available_width >= prefix_width + SEGMENT_GAP + full_interrupt_width + MIN_STATUS_WIDTH;
    let show_interrupt_keys = show_interrupt_words
        || available_width
            >= prefix_width + SEGMENT_GAP + key_interrupt_width + MIN_TINY_STATUS_WIDTH;
    let interrupt_width = if show_interrupt_words {
        full_interrupt_width
    } else if show_interrupt_keys {
        key_interrupt_width
    } else {
        0
    };
    let interrupt_gap = if show_interrupt_keys { SEGMENT_GAP } else { 0 };
    // Transport clause sits between the master label and the elapsed timer.
    // Degradation order under width pressure (most to least expendable):
    //   full clause → compact clause → clause gone → elapsed → status
    //   truncation → interrupt words.
    // The clause never steals so much as a column from an intact master
    // label, and the interrupt affordance is the last thing to die.
    // Clause slot: at most one annotation ever occupies it — by
    // construction they are phase-exclusive (transport clause lives in
    // AwaitingModel, silence clause only once a stream has flowed).
    let effective_full = backoff_clause.or_else(|| silent_clause.as_deref());
    let full_clause = effective_full.unwrap_or("");
    let full_clause_w = UnicodeWidthStr::width(full_clause);
    // Compact form exists only for the retry countdown (`· retry 2/8 …` →
    // `· 2/8`): the attempt counter earns its width even when tight. A
    // silence note is more expendable and simply leaves the row first.
    let compact_clause = if backoff_clause.is_some() {
        backoff_clause.map(|clause| {
            let attempt = clause
                .split("retry ")
                .nth(1)
                .and_then(|rest| rest.split_whitespace().next())
                .unwrap_or("");
            format!(" · {attempt}")
        })
    } else {
        None
    };
    let compact_clause_w = compact_clause
        .as_deref()
        .map(UnicodeWidthStr::width)
        .unwrap_or(0);
    let status_natural_w = UnicodeWidthStr::width(status);
    let elapsed_text = elapsed.clone().unwrap_or_default();
    let elapsed_w = UnicodeWidthStr::width(elapsed_text.as_str());
    let fixed_tail = interrupt_gap + interrupt_width;
    // Does the row fit with this exact segment recipe?
    let fits = |status_w: usize, clause_w: usize, elapsed_on: bool| -> bool {
        let el = if elapsed_on { elapsed_w } else { 0 };
        prefix_width + status_w + clause_w + el + fixed_tail <= available_width
    };
    enum Clause {
        Full,
        Compact,
        None,
    }
    let (clause_choice, visible_elapsed_width) = if fits(status_natural_w, full_clause_w, true) {
        (Clause::Full, elapsed_w)
    } else if fits(status_natural_w, full_clause_w, false) {
        (Clause::Full, 0)
    } else if fits(status_natural_w, compact_clause_w, false) {
        (Clause::Compact, 0)
    } else if fits(status_natural_w, 0, true) {
        (Clause::None, elapsed_w)
    } else {
        // Last resort: give up elapsed, then give up status length itself.
        (Clause::None, 0)
    };
    let visible_clause_width = match clause_choice {
        Clause::Full => full_clause_w,
        Clause::Compact => compact_clause_w,
        Clause::None => 0,
    };
    let clause_for_display = match clause_choice {
        Clause::Full => Some(full_clause),
        Clause::Compact => compact_clause.as_deref(),
        Clause::None => None,
    };
    let status_width = available_width
        .saturating_sub(prefix_width + visible_clause_width + visible_elapsed_width + fixed_tail)
        .max(MIN_STATUS_WIDTH.min(available_width));
    let status = truncate_for_bar(status, status_width);

    let mut spans: Vec<Span> = Vec::new();
    let spinner = spinner_glyph();
    // Single arbiter for the dot's appearance this frame: clock-driven
    // breath or gated stillness — never a blend of two.
    let liveness = classify_liveness(awaiting_permission);
    let spinner_color = dot_color(liveness, spinner_phase, theme);

    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        spinner,
        Style::default()
            .fg(spinner_color)
            .add_modifier(Modifier::BOLD),
    ));

    // Byte-pressure micro-meter retired (ADR-0154): the breathing dot is the
    // row's only indicator glyph, and stalls speak through the silent
    // clause, not through density glyphs.

    // Lead segment: the live status — the thing that changes frame to frame,
    // so it receives the left-to-right shimmer. The structural counters
    // (round/turn/model) are deliberately absent; they live in the Activity
    // modal that this bar opens on click. Truncate this segment first so the
    // interrupt affordance survives narrow widths.
    //
    // A pending permission request is a distinct attention state, not ordinary
    // activity: render the label in a steady warning hue (bold, no shimmer) so
    // the user reads "the round is paused waiting on your decision" rather
    // than "something is running". The permission sheet below carries the
    // decision affordances; this bar just signals the state.
    spans.push(Span::raw(" "));
    if awaiting_permission {
        spans.push(Span::styled(
            status,
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        // Steady-state master label: brand, upright, no resident animation.
        // Every previous oscillation here was paid for by the frame clock,
        // not by work; the typed phase's own word changes are the honest
        // freshness signal. Italic was retired with ADR-0154 — oblique type
        // reads as content emphasis (quotes, math), wrong register for chrome.
        spans.push(Span::styled(status, Style::default().fg(theme.brand())));
    }

    if visible_elapsed_width > 0 {
        spans.push(Span::styled(elapsed_text, dim));
    }

    // Transport clause: muted by design — it must read as an annotation on
    // the live status, never the headline. Sits at the right edge it ticks
    // toward, ahead of the fixed interrupt keys.
    if visible_clause_width > 0 {
        spans.push(Span::styled(clause_for_display.unwrap_or_default(), dim));
    }

    // The interrupt affordance is a separate, right-pinned segment. This
    // keeps operational keys out of the activity sentence and leaves a stable
    // scan target as the status and elapsed value change.
    if show_interrupt_keys {
        let used: usize = spans.iter().map(|span| span.content.width()).sum();
        let gap = available_width.saturating_sub(used + interrupt_width);
        spans.push(Span::styled(" ".repeat(gap), dim));
        spans.push(keycap_span(theme, Key::ESC.display()));
        spans.push(Span::styled(" ", dim));
        spans.push(keycap_span(theme, Key::ESC.display()));
        if show_interrupt_words {
            spans.push(Span::styled(" interrupt", dim));
        }
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
    Some(rect)
}

/// The one-row todo summary that leads the footer stack (above the queue bar,
/// and above the transient activity bar). It is the permanent home for
/// task-list affordances: a brand-colored `TODOS` tag, the done/total
/// progress, and a one-line preview of the current item — the `InProgress`
/// one, or the first `Pending` when nothing is mid-flight (so the bar always
/// points at "what is happening / what is next").
///
/// The bar is deliberately quiet: it sits on the plain surface (no raised
/// tint, no pin glyph), so it reads as metadata rather than another pinned
/// panel.
///
/// Layout:
/// ```text
/// TODOS d/t · {current item preview…}                Ctrl+T expand
/// ```
/// The right-pinned `Ctrl+T expand` legend is the keyboard affordance that
/// opens the Activity modal on the Todos section; it keeps a guaranteed
/// [`BAR_LEGEND_GAP_MIN`] columns of breathing room from the preview so a
/// truncated item never butts against the keycap. It drops under width
/// pressure (the `expand` label first, then the whole legend) so the identity
/// and preview on the left always survive. The whole bar is the click target
/// for the same destination.
///
/// Always one row tall. Parallels [`draw_queue_bar`] but lighter — one row
/// instead of two — since a todo item is informational (the agent's plan)
/// rather than directly actionable the way a queued dispatch is.
///
/// Returns the full bar rect so the event loop can hit-test clicks.
pub fn draw_todo_bar(
    frame: &mut Frame,
    rect: Rect,
    todos: &muta_contracts::TodoList,
    theme: &Theme,
) -> Rect {
    use muta_contracts::{TodoItem, TodoStatus};

    // Plain surface: every span drops the background entirely, so the row
    // blends with the frame instead of reading as a raised band.
    let dim = Style::default().fg(theme.muted());
    let fg = Style::default().fg(theme.fg());
    let bold = Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD);
    // The `TODOS` tag wears the brand color so the left edge still reads as a
    // deliberate section marker without needing a pin glyph or a tinted band.
    let tag_style = Style::default()
        .fg(theme.brand())
        .add_modifier(Modifier::BOLD);
    // Full row width in display columns, for the right-pinned legend math.
    let full_w = rect.width as usize;

    let done = todos.count(TodoStatus::Completed);
    let total = todos.items.len();
    let progress = format!("{done}/{total}");

    // Current item: the InProgress one, else the first Pending (next up).
    let current: Option<&TodoItem> = todos
        .items
        .iter()
        .find(|i| i.status == TodoStatus::InProgress)
        .or_else(|| todos.items.iter().find(|i| i.status == TodoStatus::Pending));

    // ── Left identity: `TODOS d/t` ──
    // Uppercase reads as a section tag rather than a lowercase token; the
    // count sits one space off the label.
    let left: Vec<Span<'static>> = vec![
        Span::styled("TODOS", tag_style),
        Span::styled(" ", dim),
        Span::styled(progress, bold),
    ];
    let left_w: usize = left.iter().map(|s| s.content.width()).sum();

    // ── Right legend: `Ctrl+T expand`, dropping under width pressure ──
    let mk_legend = |density: TodoLegendDensity| -> Vec<Span<'static>> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        spans.push(keycap_span(theme, Key::CTRL_T.display()));
        if matches!(density, TodoLegendDensity::Full) {
            spans.push(Span::styled(" expand", dim));
        }
        spans
    };
    let legend_width =
        |spans: &Vec<Span<'static>>| -> usize { spans.iter().map(|s| s.content.width()).sum() };

    // Columns reserved between the identity and the legend: the gap that
    // leads the preview (only when there is one) plus the legend's breathing
    // room — deliberately wider than the hint/status bar gap so the keycap
    // never reads as glued to the content.
    let content_sep = 2;
    let gap_for = |legend_w: usize| if legend_w > 0 { BAR_LEGEND_GAP_MIN } else { 0 };
    const MIN_PREVIEW_WIDTH: usize = 4;
    let preview_budget = |legend_w: usize| {
        let sep = if current.is_some() { content_sep } else { 0 };
        full_w.saturating_sub(left_w + sep + legend_w + gap_for(legend_w))
    };

    let mut legend = mk_legend(TodoLegendDensity::Full);
    if current.is_some() && preview_budget(legend_width(&legend)) < MIN_PREVIEW_WIDTH {
        legend = mk_legend(TodoLegendDensity::Compact);
    }
    if current.is_some() && preview_budget(legend_width(&legend)) < MIN_PREVIEW_WIDTH {
        legend.clear();
    }
    let legend_w = legend_width(&legend);
    let gap = gap_for(legend_w);

    let mut row: Vec<Span<'static>> = Vec::with_capacity(left.len() + 4 + legend.len());
    row.extend(left);

    if let Some(item) = current {
        let budget = preview_budget(legend_w);
        let one_line = crate::overlays::common::one_line(item.content.trim());
        let preview = if one_line.width() > budget {
            crate::overlays::common::truncate_ellipsis(&one_line, budget)
        } else {
            one_line
        };
        let preview_w = preview.width();
        row.push(Span::styled("  ", dim));
        row.push(Span::styled(preview, fg));
        // Space before the legend: at least `gap` so the keycap keeps real
        // distance from the content even when the preview truncates to fill;
        // any leftover width flows into the same gap, pinning the legend flush
        // right. The budget check above guarantees `gap` always fits here.
        let pad = full_w
            .saturating_sub(left_w + content_sep + preview_w + legend_w)
            .max(gap);
        row.push(Span::styled(" ".repeat(pad), dim));
    } else {
        // No current item (e.g. everything terminal just before auto-clear):
        // right-pin the legend directly, keeping the same minimum gap.
        let pad = full_w.saturating_sub(left_w + legend_w).max(gap);
        row.push(Span::styled(" ".repeat(pad), dim));
    }

    row.extend(legend);

    frame.render_widget(Paragraph::new(Line::from(row)), rect);
    rect
}

/// How much of the todo bar's `Ctrl+T expand` legend survives under width
/// pressure.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TodoLegendDensity {
    /// Key + label: `Ctrl+T expand`.
    Full,
    /// Key only: `Ctrl+T`.
    Compact,
}

/// Truncate `s` to at most `max` display cells, appending `…` when cut, so a
/// long status or pursuit objective does not overflow the single-line bar.
fn truncate_for_bar(s: &str, max: usize) -> String {
    if UnicodeWidthStr::width(s) <= max {
        s.to_string()
    } else if max == 0 {
        String::new()
    } else if max == 1 {
        "…".to_string()
    } else {
        let mut used = 0usize;
        let mut head = String::new();
        for ch in s.chars() {
            let width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + width > max - 1 {
                break;
            }
            head.push(ch);
            used += width;
        }
        format!("{head}…")
    }
}

/// Draw a completion menu anchored above the input box.
///
/// The popup's leading edge aligns with the typed trigger token (`anchor_x`,
/// passed in by the caller as the token's on-screen column — e.g. column 0 of
/// the composer text area for a `/command`, or the `@`'s column for a path
/// mention), so the menu visually hangs off the text it completes.
///
/// Each row is `command  description` laid out in two columns separated by
/// plain padding — no `·` ornament; the primary/secondary relationship is
/// carried by weight and brightness alone (command bright + bold,
/// description dim). Alias rows render as `alias ↝ /canonical` with the
/// mapping in the dim tier: the difference is visible in the candidate
/// list, and accepting commits the canonical spelling. The selected row is
/// highlighted as one solid band across the **full popup width**, label
/// column, padding, and trailing fill included, so the highlight never
/// fragments into per-segment blocks.
pub fn draw_completion_menu(
    frame: &mut Frame,
    _layout_map: &mut LayoutMap,
    completions: &[crate::completion::Completion],
    selected_idx: Option<usize>,
    anchor: Rect,
    anchor_x: u16,
    theme: &Theme,
) {
    if completions.is_empty() {
        return;
    }

    const MAX_VISIBLE: usize = 6;

    let total = completions.len();
    let scroll_offset = match selected_idx {
        Some(sel) if sel >= MAX_VISIBLE && total > MAX_VISIBLE => {
            (sel - (MAX_VISIBLE - 1)).min(total - MAX_VISIBLE)
        }
        _ => 0,
    };
    let window_end = (scroll_offset + MAX_VISIBLE).min(total);
    let visible_rows = &completions[scroll_offset..window_end];
    let menu_height = visible_rows.len() as u16;

    let viewport = viewport_rect(frame);

    // Active item & Detail Inspector: only show hover documentation when an entry is actively selected (selected_idx is Some)
    let active_doc = selected_idx
        .and_then(|idx| completions.get(idx))
        .and_then(|c| c.doc.as_ref());

    let max_cmd = completions
        .iter()
        .map(|c| match &c.kind {
            crate::completion::CompletionItemKind::IntentSuggestion { .. } => {
                c.label.width() + 2 // "➜ " prefix
            }
            // Alias rows append " [*]" to mark they are aliases.
            crate::completion::CompletionItemKind::SlashAlias => c.label.width() + 4,
            _ if c.alias_of.is_some() => c.label.width() + 4,
            _ => c.label.width(),
        })
        .max()
        .unwrap_or(0);

    // Left menu is a pure, compact command list without inline descriptions
    let max_menu_width = ((viewport.width as usize) * 3 / 5).max(24);
    let content_width = (max_cmd + 2).max(18);
    let menu_width = (content_width
        .min(max_menu_width)
        .min(viewport.width as usize)) as u16;

    // Position of the primary completion menu (above the input box)
    let mut y = anchor.y.saturating_sub(menu_height);
    if y == 0 && anchor.y < menu_height {
        y = 0;
    }
    let x = anchor_x
        .min(viewport.right().saturating_sub(menu_width))
        .max(viewport.x);

    let menu_area = Rect::new(x, y, menu_width, menu_height);
    frame.render_widget(Clear, menu_area);

    let block = RtBlock::default().style(Style::default().bg(theme.body()));
    let menu_w = menu_area.width as usize;

    let lines: Vec<Line> = visible_rows
        .iter()
        .enumerate()
        .map(|(row, c)| {
            let global_idx = row + scroll_offset;
            let is_selected = Some(global_idx) == selected_idx;
            let body_bg = theme.body();
            let row_bg = if is_selected { theme.brand() } else { body_bg };
            let is_alias = matches!(c.kind, crate::completion::CompletionItemKind::SlashAlias)
                || c.alias_of.is_some();
            let cmd_style = if is_selected {
                Style::default()
                    .bg(row_bg)
                    .fg(contrast_fg(theme.brand()))
                    .add_modifier(Modifier::BOLD)
            } else if matches!(
                c.kind,
                crate::completion::CompletionItemKind::IntentSuggestion { .. }
            ) {
                Style::default()
                    .bg(row_bg)
                    .fg(theme.info())
                    .add_modifier(Modifier::BOLD)
            } else if is_alias {
                Style::default().bg(row_bg).fg(theme.fg())
            } else {
                Style::default()
                    .bg(row_bg)
                    .fg(theme.fg())
                    .add_modifier(Modifier::BOLD)
            };

            let (primary, secondary) = match &c.kind {
                crate::completion::CompletionItemKind::IntentSuggestion { .. } => {
                    (format!("➜ {}", c.label), String::new())
                }
                crate::completion::CompletionItemKind::SlashAlias => {
                    (format!("{} [*]", c.label), String::new())
                }
                _ if is_alias => (
                    format!("{} [*]", c.label),
                    String::new(),
                ),
                _ => (c.label.clone(), String::new()),
            };
            let secondary_style = if is_selected {
                Style::default().bg(row_bg).fg(contrast_fg(theme.brand()))
            } else {
                Style::default().bg(row_bg).fg(theme.muted())
            };

            // Pad to the full popup width so the selected-row highlight
            // stays one solid band across label, mapping, and trailing fill.
            let used = primary.width() + secondary.width();
            let pad = menu_w.saturating_sub(used);
            let mut spans = vec![Span::styled(primary, cmd_style)];
            if !secondary.is_empty() {
                spans.push(Span::styled(secondary, secondary_style));
            }
            if pad > 0 {
                spans.push(Span::styled(" ".repeat(pad), Style::default().bg(row_bg)));
            }
            Line::from(spans)
        })
        .collect();

    frame.render_widget(Paragraph::new(lines).block(block), menu_area);

    // Right-side Hover Documentation Flyout Window with line wrapping and distinct background
    if let Some(doc) = active_doc {
        let space_on_right =
            (viewport.right() as usize).saturating_sub(menu_area.right() as usize + 1);
        let space_on_left = (menu_area.x as usize).saturating_sub(viewport.x as usize + 1);

        let (doc_x, doc_width) = if space_on_right >= 26 {
            let w = space_on_right.min(56) as u16;
            (menu_area.right() + 1, w)
        } else if space_on_left >= 26 {
            let w = space_on_left.min(56) as u16;
            (menu_area.x.saturating_sub(w + 1), w)
        } else {
            (0, 0)
        };

        if doc_width >= 20 {
            let doc_bg = theme.panel();
            let max_text_w = (doc_width as usize).saturating_sub(2).max(10);
            let mut insp_lines = Vec::new();

            // Line 1: Command name + [Category] (or /alias -> /canonical + [Category])
            let cat_label = doc.category.as_deref().unwrap_or("Command");
            let alias_row = selected_idx.and_then(|idx| completions.get(idx));
            let is_alias = alias_row
                .map(|c| {
                    matches!(c.kind, crate::completion::CompletionItemKind::SlashAlias)
                        || c.alias_of.is_some()
                })
                .unwrap_or(false);

            let header_title = if is_alias {
                let alias_label = alias_row
                    .map(|c| c.label.as_str())
                    .unwrap_or_default();
                let target = alias_row
                    .and_then(|c| c.alias_of.as_deref())
                    .unwrap_or(&doc.name);
                format!("{alias_label} -> {target}")
            } else {
                doc.name.clone()
            };

            let header_spans = vec![
                Span::styled(" ", Style::default().bg(doc_bg)),
                Span::styled(
                    header_title,
                    Style::default()
                        .bg(doc_bg)
                        .fg(theme.info())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  [{cat_label}]"),
                    Style::default()
                        .bg(doc_bg)
                        .fg(theme.muted())
                        .add_modifier(Modifier::DIM),
                ),
            ];
            insp_lines.push(Line::from(header_spans));

            // Line 2..: Summary (wrapped)
            if !doc.summary.is_empty() {
                for wl in crate::text_layout::wrap_text(&doc.summary, max_text_w) {
                    insp_lines.push(Line::from(vec![
                        Span::styled(" ", Style::default().bg(doc_bg)),
                        Span::styled(
                            wl.text.to_string(),
                            Style::default()
                                .bg(doc_bg)
                                .fg(theme.fg())
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                }
            }

            // Line 3..: Usage (wrapped)
            if !doc.usage.is_empty() {
                let usage_str = format!("Usage: {}", doc.usage.join("  |  "));
                for wl in crate::text_layout::wrap_text(&usage_str, max_text_w) {
                    insp_lines.push(Line::from(vec![
                        Span::styled(" ", Style::default().bg(doc_bg)),
                        Span::styled(
                            wl.text.to_string(),
                            Style::default()
                                .bg(doc_bg)
                                .fg(theme.brand())
                                .add_modifier(Modifier::DIM),
                        ),
                    ]));
                }
            }

            // Line 4..: Subcommands, each with its own introduction — the
            // detail tier of the progressive disclosure (the menu row only
            // shows the parent; the flyout explains the verbs).
            for (sub_name, sub_summary) in &doc.subcommands {
                let sub_str = format!("  {sub_name} — {sub_summary}");
                for wl in crate::text_layout::wrap_text(&sub_str, max_text_w) {
                    insp_lines.push(Line::from(vec![
                        Span::styled(" ", Style::default().bg(doc_bg)),
                        Span::styled(
                            wl.text.to_string(),
                            Style::default().bg(doc_bg).fg(theme.muted()),
                        ),
                    ]));
                }
            }

            let max_flyout_h = (viewport.height as usize)
                .saturating_sub(anchor.y as usize)
                .clamp(12, 16) as u16;
            let doc_height = (insp_lines.len() as u16).max(menu_height).min(max_flyout_h);
            let mut doc_y = anchor.y.saturating_sub(doc_height);
            if doc_y == 0 && anchor.y < doc_height {
                doc_y = 0;
            }

            let doc_area = Rect::new(doc_x, doc_y, doc_width, doc_height);
            frame.render_widget(Clear, doc_area);

            let doc_w = doc_width as usize;
            let padded_doc_lines: Vec<Line> = (0..doc_height as usize)
                .map(|idx| {
                    if let Some(mut line) = insp_lines.get(idx).cloned() {
                        let cur_w = line.width();
                        if cur_w < doc_w {
                            line.spans.push(Span::styled(
                                " ".repeat(doc_w - cur_w),
                                Style::default().bg(doc_bg),
                            ));
                        }
                        line
                    } else {
                        Line::from(vec![Span::styled(
                            " ".repeat(doc_w),
                            Style::default().bg(doc_bg),
                        )])
                    }
                })
                .collect();

            let doc_block = RtBlock::default().style(Style::default().bg(doc_bg));
            frame.render_widget(Paragraph::new(padded_doc_lines).block(doc_block), doc_area);
        }
    }
}

/// Inputs for [`draw_model_bar`]. The split row's halves — context usage
/// and stream rate on the left, model identity on the right. The
/// Enter-action keys and the `as:` target row moved inside the composer
/// ([`crate::components::composer_hints`]); session state (workspace path,
/// `DELEGATED`) lives on the status bar below.
pub struct ModelBarView<'a> {
    pub current_model: &'a str,
    /// Whether the model is available / offered by the active provider.
    /// When `false`, the model is displayed as unavailable/missing (e.g. delisted).
    pub model_available: bool,
    pub provider_name: Option<&'a str>,
    pub reasoning_effort: Option<&'a str>,
    pub context_tokens: Option<usize>,
    /// Client-observed stream TPS of the latest completed principal ReAct
    /// turn. `None` hides the gauge entirely until the first turn lands a
    /// defensible sample; each completed turn refreshes it.
    pub last_turn_tps: Option<f64>,
    pub ignition_elapsed_ms: Option<u128>,
}

impl<'a> Default for ModelBarView<'a> {
    fn default() -> Self {
        Self {
            current_model: "",
            model_available: true,
            provider_name: None,
            reasoning_effort: None,
            context_tokens: None,
            last_turn_tps: None,
            ignition_elapsed_ms: None,
        }
    }
}

/// Fine-grained click targets painted inside the model bar.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModelBarRects {
    pub performance: Option<Rect>,
    pub context: Option<Rect>,
}

/// Draw the single-line model bar pinned below the input box. The row splits
/// in two, justified against opposite edges: the ambient gauges — context
/// usage and the last turn's stream rate, each trailed by the keycap hint of
/// its drill-down modal (`Ctrl+O` / `Ctrl+S`) — anchor the left half, and
/// the model identity group (`model effort @instance`) pins right. The
/// Enter-action keys moved inside the composer's bottom padding row; the
/// `as:` target row lives in its top padding row
/// ([`crate::components::composer_hints`]).
pub fn draw_model_bar(
    frame: &mut Frame,
    rect: Rect,
    view: ModelBarView<'_>,
    theme: &Theme,
) -> ModelBarRects {
    let ModelBarView {
        current_model,
        model_available,
        provider_name,
        reasoning_effort,
        context_tokens,
        last_turn_tps,
        ignition_elapsed_ms,
    } = view;

    let bg = theme.surface();
    let full_w = rect.width as usize;

    // --- Cluster build inputs: the context + rate gauges anchor the row's
    // left edge (padded by `inner`); the model identity group pins right,
    // mirrored with the same indent. `MODEL_BAR_GAP_MIN` keeps the two
    // halves from ever colliding.
    let inner = MODEL_BAR_INNER_PADDING;

    // --- Gauge cluster: context usage, last-turn performance.
    // Build each segment separately so we can drop optional ones when the
    // terminal is too narrow.
    let context_max = crate::providers::model_context_window(current_model);

    // Build the segments independently. Under width pressure the keycap
    // hints drop first, then the instance suffix (pure provenance), then
    // reasoning, then the stream rate, then context; the model name is the
    // last item standing.
    let (model_label, model_style) = if current_model.is_empty() {
        (
            "(no model)".to_string(),
            Style::default().fg(theme.muted()).bg(bg),
        )
    } else if !model_available {
        let name = current_model;
        (
            format!("{name} [unavailable]"),
            Style::default()
                .fg(theme.err())
                .add_modifier(Modifier::BOLD)
                .bg(bg),
        )
    } else {
        (
            current_model.to_string(),
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD)
                .bg(bg),
        )
    };
    let model_width = model_label.width();
    let model_spans = vec![Span::styled(model_label, model_style)];

    // Instance suffix: ` @kimi-code` — muted so it reads as provenance, not
    // identity. Empty names render as nothing.
    let instance_label = provider_name
        .filter(|name| !name.is_empty())
        .map(|name| format!("@{name}"));
    let mut instance_spans: Vec<Span<'static>> = Vec::new();
    if let Some(label) = &instance_label {
        instance_spans.push(Span::styled(
            label.clone(),
            Style::default().fg(theme.muted()).bg(bg),
        ));
    }
    let instance_width = instance_spans
        .iter()
        .map(|span| span.content.width())
        .sum::<usize>();

    // Reasoning-effort tag: `{effort}` (e.g. `max`). Optional — only present
    // when the active model is actually reasoning (caller-resolved and
    // protocol-gated). Sits right after the model name so it reads as an
    // attribute of the model — "Kimi K3 max @kimi-code".
    let mut reasoning_spans: Vec<Span<'static>> = Vec::new();
    if let Some(effort) = reasoning_effort {
        reasoning_spans.push(Span::styled(
            effort.to_string(),
            Style::default()
                .fg(theme.info())
                .add_modifier(Modifier::BOLD)
                .bg(bg),
        ));
    }
    let reasoning_width = reasoning_spans
        .iter()
        .map(|span| span.content.width())
        .sum::<usize>();

    // Context-usage segment: `89.2k (8%)`. It intentionally reflects only
    // committed AI-visible context; live composer drafts belong in the
    // on-demand Context Usage report, not this persistent glance surface.
    // Always shown when the model
    // reports a context window; the percentage takes the threshold color so
    // a nearly full window is unmissable.
    //
    // The harness owns projection semantics. Never infer AI context from the
    // rendered transcript: it contains durable command echoes, archived rounds,
    // and UI-only children while omitting system/tool-schema input.
    let mut context_spans: Vec<Span<'static>> = Vec::new();
    if context_max > 0 {
        let used = context_tokens.unwrap_or(0);
        context_spans = context_usage_spans(used, context_max, theme, bg);
    }
    let context_seg_width = context_spans
        .iter()
        .map(|span| span.content.width())
        .sum::<usize>();

    // Performance segment: the latest completed principal turn's
    // client-observed stream rate. Hidden entirely until a defensible sample
    // exists (a `– tok/s` placeholder is noise before the first turn
    // completes) — `Ctrl+S` and the report modal remain the discovery paths
    // regardless, and each completed turn refreshes this value.
    let performance_spans: Vec<Span<'static>> = last_turn_tps
        .filter(|rate| rate.is_finite() && *rate > 0.0)
        .map(|rate| {
            vec![Span::styled(
                format!("{rate:.1} tok/s"),
                Style::default().fg(theme.info()).bg(bg),
            )]
        })
        .unwrap_or_default();
    let performance_width = performance_spans
        .iter()
        .map(|span| span.content.width())
        .sum::<usize>();

    // Progressive disclosure: both right-cluster gauges open a drill-down
    // modal on click, so each carries its keyboard twin as a muted keycap
    // hint — `Ctrl+O` for the context report, `Ctrl+S` for the performance
    // report. The hints are the first thing to go under width pressure
    // (below), which keeps the gauges themselves available longest.
    let keycap_dim =
        |text: &str| Span::styled(text.to_string(), Style::default().fg(theme.muted()).bg(bg));
    let context_keycap_width = Key::CTRL_O.display().width() + 1; // + leading space
    let performance_keycap_width = Key::CTRL_S.display().width() + 1; // + leading space

    let mut show_model = model_width > 0;
    let mut show_reasoning = reasoning_width > 0;
    let mut show_instance = instance_width > 0;
    let mut show_performance = performance_width > 0;
    let mut show_context = context_seg_width > 0;
    let mut show_context_keycap = show_context;
    let mut show_performance_keycap = show_performance;
    // The model name, its reasoning effort, and the provider instance form
    // one identity group (`Kimi K3 high @111xianyu`) joined by the tighter
    // model gap; the context and rate gauges each optionally trail their
    // keycap hint and join across the wider segment gap.
    let identity_width_for = |model: bool, reasoning: bool, instance: bool| {
        let identity_count = usize::from(model) + usize::from(reasoning) + usize::from(instance);
        usize::from(model) * model_width
            + usize::from(reasoning) * reasoning_width
            + usize::from(instance) * instance_width
            + identity_count.saturating_sub(1) * MODEL_BAR_MODEL_GAP
    };
    let gauges_width_for =
        |performance: bool, performance_keycap: bool, context: bool, context_keycap: bool| {
            usize::from(performance)
                * (performance_width + usize::from(performance_keycap) * performance_keycap_width)
                + usize::from(context)
                    * (context_seg_width + usize::from(context_keycap) * context_keycap_width)
                + usize::from(performance && context) * MODEL_BAR_SEGMENT_GAP
        };
    // The row is justified: the gauge cluster leads from the left edge and
    // the identity cluster pins to the right, each `inner` in from its
    // edge. `MODEL_BAR_GAP_MIN` keeps the middle fill from ever collapsing
    // into the two halves.
    let fits = |gauges_width: usize, identity_width: usize| {
        let middle = usize::from(gauges_width > 0 && identity_width > 0) * MODEL_BAR_GAP_MIN;
        inner + gauges_width + middle + identity_width + inner <= full_w
    };

    let mut gauges_width = gauges_width_for(
        show_performance,
        show_performance_keycap,
        show_context,
        show_context_keycap,
    );
    let mut identity_width = identity_width_for(show_model, show_reasoning, show_instance);
    // Drop order under width pressure: the keycap hints first (progressive
    // disclosure dies first — the gauges are the payload), then the instance
    // suffix (pure provenance), then reasoning, then the stream rate, then
    // context, then the model name. (Session-state flags such as
    // `DELEGATED` are not on this row — they live on the status bar.) Each
    // step recomputes only the half it shrank.
    if !fits(gauges_width, identity_width) && (show_context_keycap || show_performance_keycap) {
        show_context_keycap = false;
        show_performance_keycap = false;
        gauges_width = gauges_width_for(
            show_performance,
            show_performance_keycap,
            show_context,
            show_context_keycap,
        );
    }
    if !fits(gauges_width, identity_width) && show_instance {
        show_instance = false;
        identity_width = identity_width_for(show_model, show_reasoning, show_instance);
    }
    if !fits(gauges_width, identity_width) && show_reasoning {
        show_reasoning = false;
        identity_width = identity_width_for(show_model, show_reasoning, show_instance);
    }
    if !fits(gauges_width, identity_width) && show_performance {
        show_performance = false;
        show_performance_keycap = false;
        gauges_width = gauges_width_for(
            show_performance,
            show_performance_keycap,
            show_context,
            show_context_keycap,
        );
    }
    if !fits(gauges_width, identity_width) && show_context {
        show_context = false;
        show_context_keycap = false;
        gauges_width = gauges_width_for(
            show_performance,
            show_performance_keycap,
            show_context,
            show_context_keycap,
        );
    }
    // The model name is the last item standing — no `fits` check follows,
    // so its drop needs no width recompute.
    if !fits(gauges_width, identity_width) && show_model {
        show_model = false;
    }

    // Ignition label takeover: during the `M A X` label phase the identity
    // cluster renders as the converging label instead; the caller's band
    // tint paints the glow over it. The label owns the identity cluster's
    // width budget on the row's right half.
    let label_budget = identity_width_for(show_model, show_reasoning, show_instance);
    let label_spans = ignition_elapsed_ms
        .and_then(|ms| super::effort_ignition::label_cluster(label_budget, ms, bg, theme));
    let ignition_label_active = label_spans.is_some();

    // Left cluster: the gauges, flush against the row's left indent. The
    // context meter leads — it reads as the bridge between "what is
    // answering" and "how fast" — and the live speed signal follows. Each
    // gauge that has a drill-down modal carries its keycap hint right after
    // its value, as part of the same segment (they open the same modal).
    let mut left_spans: Vec<Span<'static>> = Vec::new();
    if show_context {
        left_spans.extend(context_spans);
        if show_context_keycap {
            left_spans.push(Span::styled(" ", Style::default().bg(bg)));
            left_spans.push(keycap_dim(Key::CTRL_O.display()));
        }
    }
    // Stream rate follows, across the wider gap; its keycap hint trails it
    // the same way.
    if show_performance {
        if !left_spans.is_empty() {
            left_spans.push(Span::styled(
                " ".repeat(MODEL_BAR_SEGMENT_GAP),
                Style::default().bg(bg),
            ));
        }
        left_spans.extend(performance_spans);
        if show_performance_keycap {
            left_spans.push(Span::styled(" ", Style::default().bg(bg)));
            left_spans.push(keycap_dim(Key::CTRL_S.display()));
        }
    }

    // Right cluster: the identity group (`model effort @instance`), or the
    // ignition takeover label while its phase is live.
    let mut right_spans: Vec<Span<'static>> = Vec::new();
    if let Some(label) = label_spans {
        right_spans = label;
    } else {
        let identity_separator =
            || Span::styled(" ".repeat(MODEL_BAR_MODEL_GAP), Style::default().bg(bg));
        let mut identity_started = false;
        for segment in [
            show_model.then_some(model_spans),
            show_reasoning.then_some(reasoning_spans),
            show_instance.then_some(instance_spans),
        ]
        .into_iter()
        .flatten()
        {
            if identity_started {
                right_spans.push(identity_separator());
            }
            identity_started = true;
            right_spans.extend(segment);
        }
    }

    // Split layout: the gauge cluster anchors the row's left edge; the
    // identity group pins right. The middle fill absorbs the remaining
    // width, so the two halves evenly own the row from their edges — the
    // `inner` indent mirrored on both sides, and `MODEL_BAR_GAP_MIN`
    // guaranteeing they never collide.
    let left_rendered_width: usize = left_spans.iter().map(|s| s.content.width()).sum();
    let right_rendered_width: usize = right_spans.iter().map(|s| s.content.width()).sum();
    let min_gap =
        usize::from(left_rendered_width > 0 && right_rendered_width > 0) * MODEL_BAR_GAP_MIN;
    let gap = full_w
        .saturating_sub(inner + left_rendered_width + right_rendered_width + inner)
        .max(min_gap);

    let mut spans: Vec<Span<'static>> =
        Vec::with_capacity(8 + left_spans.len() + right_spans.len());
    spans.push(Span::styled(" ".repeat(inner), Style::default().bg(bg)));
    spans.extend(left_spans);
    spans.push(Span::styled(" ".repeat(gap), Style::default().bg(bg)));
    spans.extend(right_spans);
    // Trailing edge indent + fill so the row owns every cell on this line.
    let used: usize = inner + left_rendered_width + gap + right_rendered_width;
    spans.push(Span::styled(
        " ".repeat(full_w.saturating_sub(used)),
        Style::default().bg(bg),
    ));

    frame.render_widget(Paragraph::new(Line::from(spans)), rect);

    // Compute the context-meter and stream-rate segments' screen rects so
    // the caller can make them clickable. The gauges anchor the row's left
    // half; the segments are located by walking the row in render order
    // from the left indent — nothing is assumed about which segment is
    // first.
    let mut performance_rect: Option<Rect> = None;
    let mut context_rect: Option<Rect> = None;
    if !ignition_label_active {
        let mut x = inner as u16;
        let mut any_rendered = false;
        let mut advance = |width: usize, seg: &mut Option<Rect>, leading: bool| {
            if leading {
                x += MODEL_BAR_SEGMENT_GAP as u16;
            }
            *seg = Some(Rect::new(rect.x + x, rect.y, width as u16, rect.height));
            x += width as u16;
        };
        if show_context {
            advance(
                context_seg_width + usize::from(show_context_keycap) * context_keycap_width,
                &mut context_rect,
                any_rendered,
            );
            any_rendered = true;
        }
        if show_performance {
            advance(
                performance_width + usize::from(show_performance_keycap) * performance_keycap_width,
                &mut performance_rect,
                any_rendered,
            );
        }
    }
    ModelBarRects {
        performance: performance_rect,
        context: context_rect,
    }
}

/// Abbreviate an absolute path to its native `~`-relative form so the workspace reads as a
/// short, glanceable label. Falls back to the literal path when no home
/// directory is known or the path is outside it. Mirrors the home-resolution
/// pattern the `~/` mention-completion query parser uses, in reverse.
pub(crate) fn tilde_home(path: &std::path::Path) -> String {
    let home = dirs::home_dir().or_else(|| std::env::var_os("HOME").map(std::path::PathBuf::from));
    if let Some(home) = home {
        // `strip_prefix` is infallible once `starts_with` has confirmed the
        // prefix; the `.ok()` swallows the statically-unreachable `Err`.
        if path.starts_with(&home) {
            let rest = path.strip_prefix(&home).ok();
            return match rest {
                Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
                Some(rest) => std::path::PathBuf::from("~")
                    .join(rest)
                    .display()
                    .to_string(),
                // Unreachable: starts_with was just checked.
                None => path.display().to_string(),
            };
        }
    }
    path.display().to_string()
}

/// One queued outbox item projected for the [`QueueBarView`] / queue modal. It
/// is a small owned snapshot of the relevant fields of a
/// [`crate::app::QueuedDispatch`], so the renderers stay decoupled from
/// the full dispatch state machine (images, pastes, lifecycle states) and
/// A snapshot of one staged dispatch, lent to the render layer so drawing
/// the queue bar/modal never entangles the view layer with the app's
/// mutable state.
#[derive(Clone)]
pub struct QueueItemView {
    /// When the item was queued (epoch ms); rendered as a local `HH:MM` in
    /// the queue modal (the bar itself no longer shows a time).
    pub queued_at_ms: u64,
    /// The user's literal prompt text (the first run is previewed in the bar).
    pub text: String,
    /// `true` while the item is an in-flight mid-round steer. A live busy-Enter
    /// steer is transcript-owned and never enters the outbox, but this field is
    /// retained so the view struct keeps its shape;
    /// producers always pass `false`.
    #[allow(dead_code)]
    pub steering: bool,
}

/// Inputs for [`draw_queue_bar`]: the persistent one-row outbox summary pinned
/// below the transcript gap. This is the permanent home for queue affordances.
pub struct QueueBarView<'a> {
    /// Outbox items for the viewed session, in dispatch order (front pops
    /// first). The layout hides the bar while this is empty, so an empty
    /// slice is only a defensive case (a bare identity row).
    pub items: &'a [QueueItemView],
    /// `true` while next-round items are held back because the running round
    /// has not yet naturally completed. Recolors the count so the user can see
    /// the queue is paused, not forgotten.
    pub paused: bool,
    /// `true` while the user has hard-blocked the outbox (`Ctrl+P`, or the
    /// queue modal being open). While blocked, no message auto-drains even
    /// after the round completes. Surfaced as a distinct `blocked` tag + the
    /// legend key flipping to `Ctrl+P resume`, so it never reads as an
    /// ordinary pause.
    pub blocked: bool,
}

/// The persistent one-row outbox summary pinned below the transcript gap.
///
/// A brand-colored `QUEUE` label on the plain surface, quietly matching the
/// todo bar above it. The single row carries, left → right: the identity
/// (`QUEUE` + count, plus a `· blocked` state tag while the user holds the
/// outbox), a one-line preview of the next item to pop, and the right-pinned
/// keycap legend (`Ctrl+P` block/resume, `Ctrl+Q` expand).
///
/// Width pressure degrades the middle and the legend before the identity:
/// the preview truncates with `…`, then the legend sheds its labels and the
/// `Ctrl+Q` cluster (keeping `Ctrl+P`, the state toggle), then the
/// legend drops entirely. An empty queue is never rendered (the layout hides
/// the row), so there is no empty-hint state.
///
/// `paused` recolors the count (warn) so the user can see the queue is held
/// back because the running round has not yet completed, not forgotten. A user
/// `blocked` state (error color + `blocked` tag + the legend's `Ctrl+P
/// resume`) is the explicit "send nothing even after the round ends" override.
///
/// Returns the full bar rect so the event loop can make the region clickable
/// (click → expand the Queue modal).
pub fn draw_queue_bar(
    frame: &mut Frame,
    rect: Rect,
    view: QueueBarView<'_>,
    theme: &Theme,
) -> Rect {
    let QueueBarView {
        items,
        paused,
        blocked,
    } = view;

    // Plain surface: every span drops the background entirely, so the row
    // blends with the frame instead of reading as a raised band.
    let full_w = rect.width as usize;

    // ── Resolve the next item to pop ────────────────────────────────────────
    // Dispatch is FIFO: the front-most item pops first. Every bar item is a
    // next-round entry now (ADR-0126).
    let next = items.first();

    let count = items.len();
    let dim = Style::default().fg(theme.muted());
    let fg = Style::default().fg(theme.fg());
    // Blocked outranks paused outranks normal: a user block is the strongest
    // "nothing sends" signal, so it wears the error color; a natural pause
    // (round not done) stays the gentler warning.
    let count_color = if blocked {
        theme.err()
    } else if paused {
        theme.warn()
    } else {
        theme.fg()
    };
    let count_style = Style::default()
        .fg(count_color)
        .add_modifier(Modifier::BOLD);
    // The `QUEUE` label wears the brand color so the left edge still reads as
    // a deliberate section marker without needing a glyph or a tinted band.
    let tag_style = Style::default()
        .fg(theme.brand())
        .add_modifier(Modifier::BOLD);

    // ── Left: `QUEUE N [blocked]` ───────────────────────────────────────────
    let mut left: Vec<Span<'static>> =
        vec![Span::styled("QUEUE", tag_style), Span::styled(" ", dim)];
    let count_label = if count > 99 {
        "99+".to_string()
    } else {
        count.to_string()
    };
    left.push(Span::styled(count_label, count_style));
    // When blocked, append an explicit `blocked` tag in the error color so
    // the held-back state never reads as an ordinary pause — the count is
    // already error-colored, and this label spells out why. R1: `blocked` is
    // a state of the queue.
    if blocked {
        left.push(Span::styled("  ", dim));
        left.push(Span::styled("blocked", count_style));
    }

    // ── Right-side keycap legend ────────────────────────────────────────────
    // The keys explain the outbox affordances (the Ctrl row, ADR-0126):
    //   Ctrl+P — block / resume the outbox (toggles; label flips with state)
    //   Ctrl+Q — expand the full queue list
    // The keycap units are same-rank peers (R2), so they are separated by
    // plain whitespace — no dot. Under width pressure the labels drop first,
    // then the Ctrl+Q cluster, keeping `Ctrl+P` (the state toggle) last.
    let mk_right = |density: LegendDensity| -> Vec<Span<'static>> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let sep = |spans: &mut Vec<Span<'static>>| {
            spans.push(Span::styled(" ".repeat(JOIN_ENUMERATE_COLS), dim));
        };
        spans.push(keycap_span(theme, Key::CTRL_P.display()));
        if matches!(density, LegendDensity::Full) {
            spans.push(Span::styled(
                if blocked { " resume" } else { " block" },
                dim,
            ));
        }
        if !matches!(density, LegendDensity::Tiny) {
            sep(&mut spans);
            spans.push(keycap_span(theme, Key::CTRL_Q.display()));
            if matches!(density, LegendDensity::Full) {
                spans.push(Span::styled(" expand", dim));
            }
        }
        spans
    };

    // ── Middle: next-item preview ───────────────────────────────────────────
    // One-line, control-chars-collapsed. The preview is the most elastic
    // segment: it truncates to whatever the identity and the legend leave
    // behind, and disappears entirely on very narrow rows.
    let preview_text = next.map(|item| crate::overlays::common::one_line(item.text.trim()));

    let left_w: usize = left.iter().map(|s| s.content.width()).sum();
    let right_w =
        |right: &[Span<'static>]| -> usize { right.iter().map(|s| s.content.width()).sum() };
    // Minimum columns a preview must get to stay meaningful; below this the
    // space goes to the legend instead.
    const PREVIEW_MIN_COLS: usize = 8;

    // Pick the densest legend that still fits identity + legend (+ preview).
    let mut legend_density = LegendDensity::Full;
    let mut right = mk_right(legend_density);
    loop {
        let rw = right_w(&right);
        let reserved = left_w
            + if rw > 0 { BAR_LEGEND_GAP_MIN + rw } else { 0 }
            + if preview_text.is_some() {
                JOIN_ENUMERATE_COLS + PREVIEW_MIN_COLS
            } else {
                0
            };
        if reserved <= full_w {
            break;
        }
        match legend_density {
            LegendDensity::Full => {
                legend_density = LegendDensity::Compact;
                right = mk_right(legend_density);
            }
            LegendDensity::Compact => {
                legend_density = LegendDensity::Tiny;
                right = mk_right(legend_density);
            }
            LegendDensity::Tiny => {
                // Still too tight: drop the legend entirely and keep the
                // identity (+ preview).
                right.clear();
                break;
            }
        }
    }

    let rw = right_w(&right);
    let preview_budget = full_w
        .saturating_sub(left_w)
        .saturating_sub(if rw > 0 { BAR_LEGEND_GAP_MIN + rw } else { 0 })
        .saturating_sub(if preview_text.is_some() {
            JOIN_ENUMERATE_COLS
        } else {
            0
        });
    let preview = preview_text
        .filter(|_| preview_budget >= PREVIEW_MIN_COLS)
        .map(|text| {
            if text.width() > preview_budget {
                crate::overlays::common::truncate_ellipsis(&text, preview_budget)
            } else {
                text
            }
        });
    let preview_w = preview.as_ref().map_or(0, |p| p.width());

    // ── Compose the single row: left · preview … legend ─────────────────────
    let mut row: Vec<Span<'static>> = Vec::with_capacity(left.len() + right.len() + 4);
    row.extend(left);
    if let Some(preview) = preview {
        row.push(Span::styled(" ".repeat(JOIN_ENUMERATE_COLS), dim));
        row.push(Span::styled(preview, fg));
    }
    let used = left_w
        + if preview_w > 0 {
            JOIN_ENUMERATE_COLS + preview_w
        } else {
            0
        };
    let gap = full_w
        .saturating_sub(used + rw)
        .max(if rw > 0 { BAR_LEGEND_GAP_MIN } else { 0 });
    row.push(Span::styled(" ".repeat(gap), dim));
    row.extend(right);
    let used = used + gap + rw;
    row.push(Span::styled(" ".repeat(full_w.saturating_sub(used)), dim));

    frame.render_widget(Paragraph::new(Line::from(row)), rect);

    rect
}

/// How much of the queue bar's keycap legend survives under width pressure.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LegendDensity {
    /// Keys + labels: `Ctrl+P block  Ctrl+Q expand`.
    Full,
    /// Bare keycaps: `Ctrl+P  Ctrl+Q`.
    Compact,
    /// Only the block/resume toggle: `Ctrl+P`.
    Tiny,
}

/// Context-usage ratio at which the usage bar turns from green to yellow.
const CONTEXT_USAGE_WARN_THRESHOLD: f64 = 0.7;
/// Context-usage ratio at which the usage bar turns from yellow to red.
const CONTEXT_USAGE_CRIT_THRESHOLD: f64 = 0.9;

/// Compact wall-clock elapsed for the activity bar: `12s`, `1m 23s`,
/// `1h 02m`. Stays short so it fits the single-line activity bar even with a
/// long model name + status. Sub-second durations render as `0s` rather than
/// `0ms` because the bar ticks at most a few times per second and showing
/// millisecond precision would flicker without adding signal. Shared with the
/// Activity modal so the bar and the modal report the same elapsed format.
pub fn format_elapsed(d: Duration) -> String {
    let total_secs = d.as_secs();
    if total_secs < 60 {
        format!("{}s", total_secs)
    } else if total_secs < 3600 {
        let m = total_secs / 60;
        let s = total_secs % 60;
        format!("{}m {:02}s", m, s)
    } else {
        let h = total_secs / 3600;
        let m = (total_secs % 3600) / 60;
        format!("{}h {:02}m", h, m)
    }
}

/// Format a token count with a single-letter SI suffix: `999`, `1.0k`, `20.2k`,
/// `1.5M`, `3.2B`.
fn format_token_count(n: usize) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        format!("{}", n)
    }
}

/// Context-window usage indicator: `89.2k (8%)`.
/// The percentage takes the green → yellow → red threshold color so a nearly
/// full window is unmissable; the token count stays muted. `bg` is applied to
/// every span so the indicator reads on a solid background.
fn context_usage_spans(used: usize, max: usize, theme: &Theme, bg: Color) -> Vec<Span<'static>> {
    let ratio = if max == 0 {
        0.0
    } else {
        ((used as f64) / (max as f64)).clamp(0.0, 1.0)
    };
    let color = if ratio < CONTEXT_USAGE_WARN_THRESHOLD {
        theme.ok()
    } else if ratio < CONTEXT_USAGE_CRIT_THRESHOLD {
        theme.warn()
    } else {
        theme.err()
    };
    let pct = (ratio * 100.0).round() as u32;

    let mut spans = Vec::with_capacity(2);
    spans.push(Span::styled(
        format_token_count(used),
        Style::default().fg(theme.muted()).bg(bg),
    ));
    spans.push(Span::styled(
        format!(" ({}%)", pct),
        Style::default().fg(color).bg(bg),
    ));
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn activity_row_text(width: u16, status: &str, phase: usize) -> String {
        activity_row_text_with_clause(width, status, None, false, phase)
    }

    fn activity_row_text_with_clause(
        width: u16,
        status: &str,
        backoff_clause: Option<&str>,
        awaiting: bool,
        phase: usize,
    ) -> String {
        let mut terminal = mutx_engine::TestTerminal::new(width, 1);
        terminal.draw(|frame| {
            draw_activity_bar(
                frame,
                Rect::new(0, 0, width, 1),
                None,
                crate::chrome::ActivityBarView {
                    status,
                    backoff_clause,
                    silent_clause: None,
                    awaiting_permission: awaiting,
                },
                phase,
                &Theme::default(),
            );
        });
        terminal
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    }

    /// Render the activity bar and collect the foreground color of each cell,
    /// so a test can assert e.g. the permission state paints in the warning
    /// hue rather than the shimmer palette.
    fn activity_row_colors(width: u16, status: &str, awaiting: bool, phase: usize) -> Vec<Color> {
        activity_row_colors_with_clause(width, status, None, awaiting, phase)
    }

    fn activity_row_colors_with_clause(
        width: u16,
        status: &str,
        backoff_clause: Option<&str>,
        awaiting: bool,
        phase: usize,
    ) -> Vec<Color> {
        let mut terminal = mutx_engine::TestTerminal::new(width, 1);
        terminal.draw(|frame| {
            draw_activity_bar(
                frame,
                Rect::new(0, 0, width, 1),
                None,
                crate::chrome::ActivityBarView {
                    status,
                    backoff_clause,
                    silent_clause: None,
                    awaiting_permission: awaiting,
                },
                phase,
                &Theme::default(),
            );
        });
        terminal
            .buffer()
            .content
            .iter()
            .map(|cell| cell.fg)
            .collect()
    }

    #[test]
    fn activity_bar_preserves_interrupt_hint_at_minimum_width() {
        let row = activity_row_text(
            36,
            "retrying a provider request after a very detailed transient failure",
            8,
        );
        assert!(row.contains("Esc Esc interrupt"), "row was {row:?}");
        assert!(row.contains('…'), "long status was not truncated: {row:?}");
    }

    #[test]
    fn tilde_home_shortens_a_home_rooted_path() {
        let home = dirs::home_dir()
            .or_else(|| std::env::var_os("HOME").map(std::path::PathBuf::from))
            .expect("test requires a discoverable home directory");
        let under = home.join("projects").join("xx");
        let rendered = tilde_home(&under);
        assert_eq!(
            rendered,
            std::path::PathBuf::from("~")
                .join("projects")
                .join("xx")
                .display()
                .to_string()
        );

        // The home directory itself collapses to a bare `~`.
        assert_eq!(tilde_home(&home), "~");
    }

    fn todo_list_with(item: &str, status: muta_contracts::TodoStatus) -> muta_contracts::TodoList {
        let mut todos = muta_contracts::TodoList::new();
        todos.items.push(muta_contracts::TodoItem {
            id: muta_contracts::TodoId(1),
            content: item.to_string(),
            status,
            created_at: 0,
            updated_at: 0,
        });
        todos
    }

    fn todo_row_text(todos: &muta_contracts::TodoList, width: u16) -> String {
        let mut terminal = mutx_engine::TestTerminal::new(width, 1);
        terminal.draw(|frame| {
            draw_todo_bar(frame, Rect::new(0, 0, width, 1), todos, &Theme::default());
        });
        terminal
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    }

    #[test]
    fn backoff_clause_renders_beside_status_and_degrades_narrow() {
        // Master label keeps the workflow story; the transport countdown is a
        // separate, muted clause — never replacing the label.
        let wide = activity_row_text_with_clause(
            100,
            "waiting for model",
            Some(" · retry 2/8 next in 4s"),
            false,
            0,
        );
        assert!(wide.contains("waiting for model"), "{wide:?}");
        assert!(wide.contains("retry 2/8 next in 4s"), "{wide:?}");

        // Under width pressure the compact attempt counter survives and the
        // master label is still intact.
        let narrow = activity_row_text_with_clause(
            44,
            "waiting for model",
            Some(" · retry 2/8 next in 4s"),
            false,
            0,
        );
        assert!(narrow.contains("waiting for model"), "{narrow:?}");
        assert!(
            narrow.contains("2/8") || !narrow.contains("next in"),
            "compact form should drop the countdown tail: {narrow:?}"
        );

        // No clause configured → no stray separators.
        let plain = activity_row_text_with_clause(80, "answering", None, false, 0);
        assert!(plain.contains("answering"), "{plain:?}");
    }

    #[test]
    fn todo_bar_leads_with_brand_tag_on_a_plain_surface() {
        // The tag treatment: `TODOS` leads at the gutter in the brand accent
        // on the plain frame surface — no pin glyph, no raised tint — so the
        // row reads as quiet metadata rather than another pinned panel. We
        // assert all of this against the real buffer cells (the substring-only
        // tests can't see color or background).
        let theme = Theme::default();
        let todos = todo_list_with("write the docs", muta_contracts::TodoStatus::InProgress);
        let mut terminal = mutx_engine::TestTerminal::new(80, 1);
        terminal.draw(|frame| {
            draw_todo_bar(frame, Rect::new(0, 0, 80, 1), &todos, &theme);
        });
        let cells = terminal.buffer().content.clone();

        // (1) The tag leads at the gutter, brand-colored.
        assert_eq!(cells[0].symbol(), "T", "expected 'TODOS' tag at col 0");
        assert_eq!(cells[0].fg(), theme.brand(), "TODOS tag not brand-colored");

        // (2) The bar sits on the plain surface: no raised tint anywhere on
        // the row (sample the trailing cell too).
        assert_eq!(cells[0].bg(), Color::Reset, "tag must not sit on a tint");
        assert_eq!(cells[79].bg(), Color::Reset, "row must stay plain");
    }

    #[test]
    fn todo_bar_shows_tag_progress_current_item_and_legend() {
        // InProgress item is the surfaced "current" content.
        let todos = todo_list_with("write the docs", muta_contracts::TodoStatus::InProgress);
        let text = todo_row_text(&todos, 80);
        assert!(text.contains("TODOS 0/1"), "row was {text:?}");
        assert!(text.contains("write the docs"), "row was {text:?}");
        assert!(text.contains("Ctrl+T expand"), "row was {text:?}");
    }

    #[test]
    fn todo_bar_falls_back_to_first_pending_when_nothing_is_in_progress() {
        let todos = todo_list_with("write the docs", muta_contracts::TodoStatus::Pending);
        let text = todo_row_text(&todos, 80);
        assert!(text.contains("TODOS 0/1"), "row was {text:?}");
        // The first Pending item reads as "next up" when nothing is mid-flight.
        assert!(text.contains("write the docs"), "row was {text:?}");
    }

    #[test]
    fn todo_bar_drops_legend_under_width_pressure() {
        let todos = todo_list_with("write the docs", muta_contracts::TodoStatus::InProgress);
        // At 20 cols the `expand` label cannot fit alongside the preview.
        let text = todo_row_text(&todos, 20);
        assert!(text.contains("TODOS 0/1"), "row was {text:?}");
        assert!(!text.contains("expand"), "legend leaked: {text:?}");
    }

    #[test]
    fn todo_bar_keeps_real_gap_before_the_legend() {
        // Long content truncates to the preview budget; the `Ctrl+T` keycap
        // must still keep a real gap from the text instead of butting against
        // the `…`. At 40 cols the preview is truncated *and* the full legend
        // still fits, so this exercises exactly the cramped layout the gap is
        // there to prevent.
        let todos = todo_list_with(
            "a very long todo item that must be truncated to leave the legend room",
            muta_contracts::TodoStatus::InProgress,
        );
        let text = todo_row_text(&todos, 40);
        let ctrl = text.find("Ctrl").expect("legend should fit at 40 cols");
        let dots = text[..ctrl]
            .rfind('…')
            .expect("preview should be truncated");
        let between = &text[dots + '…'.len_utf8()..ctrl];
        assert!(
            between.chars().all(|c| c == ' '),
            "legend must be separated from the preview by spaces: {text:?}"
        );
        assert!(
            between.chars().count() >= BAR_LEGEND_GAP_MIN,
            "legend too close to content ({} cols): {text:?}",
            between.chars().count()
        );
    }

    #[test]
    fn activity_bar_carries_no_todos_badge() {
        // Decoupled: the activity bar is a pure liveness surface now and never
        // embeds the `todos d/t` summary (that lives on its own bar below).
        let mut terminal = mutx_engine::TestTerminal::new(80, 1);
        terminal.draw(|frame| {
            draw_activity_bar(
                frame,
                Rect::new(0, 0, 80, 1),
                None,
                ActivityBarView {
                    status: "Working",
                    backoff_clause: None,
                    silent_clause: None,
                    awaiting_permission: false,
                },
                0,
                &Theme::default(),
            );
        });
        let text = terminal
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(!text.contains("todos"), "badge leaked onto bar: {text:?}");
        assert!(!text.contains("Ctrl+T"), "hint leaked onto bar: {text:?}");
    }

    #[test]
    fn narrow_runtime_row_keeps_interrupt_keys_without_todos_badge() {
        let mut terminal = mutx_engine::TestTerminal::new(36, 1);
        terminal.draw(|frame| {
            draw_activity_bar(
                frame,
                Rect::new(0, 0, 36, 1),
                None,
                ActivityBarView {
                    status: "retrying a provider request after a detailed transient failure",
                    backoff_clause: None,
                    silent_clause: None,
                    awaiting_permission: false,
                },
                8,
                &Theme::default(),
            );
        });
        let text = terminal
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("Esc Esc"), "row was {text:?}");
        // The todos summary lives on the dedicated todo bar, not here.
        assert!(!text.contains("todos"), "badge leaked: {text:?}");
        // Session-state flags live on the hint bar; the activity row never
        // carries them, even when they would fit.
        assert!(!text.contains("autopilot"), "row was {text:?}");
    }

    /// A pending permission request paints the status label in a steady warning
    /// hue rather than the ordinary shimmer palette, so the bar reads as a
    /// distinct attention state ("the round is paused on your decision") above
    /// the permission sheet. The warning hue must actually appear on the label
    /// cells, distinguishing it from the brand-colored shimmer.
    #[test]
    fn activity_bar_paints_awaiting_permission_in_warning_hue() {
        let theme = Theme::default();
        let awaiting = activity_row_colors(80, "awaiting permission", true, 4);
        let normal = activity_row_colors(80, "working", false, 4);

        // The warning color must be present somewhere in the awaiting row.
        assert!(
            awaiting.contains(&theme.warning),
            "awaiting-permission row must use the warning hue"
        );
        // A permission state must not shimmer (the shimmer sweeps the brand hue
        // across phases). The normal row, by contrast, carries brand-derived
        // colors at this phase.
        assert!(
            !awaiting.contains(&theme.warning) || awaiting != normal,
            "awaiting row must differ from the ordinary shimmer row"
        );
        // Sanity: the normal row does carry some non-warning color from the
        // shimmer (so the comparison above is meaningful).
        assert!(
            normal
                .iter()
                .any(|&c| c != theme.muted() && c != Color::Reset),
            "normal row should carry shimmer colors"
        );
    }

    #[test]
    fn format_token_count_uses_si_suffixes() {
        assert_eq!(format_token_count(0), "0");
        assert_eq!(format_token_count(999), "999");
        assert_eq!(format_token_count(1000), "1.0k");
        assert_eq!(format_token_count(20_200), "20.2k");
        assert_eq!(format_token_count(1_000_000), "1.0M");
        assert_eq!(format_token_count(3_200_000_000), "3.2B");
    }

    #[test]
    fn context_usage_spans_render_used_and_percentage() {
        let theme = Theme::default();
        let spans = context_usage_spans(20_200, 256_000, &theme, theme.panel());
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "20.2k (8%)");
    }

    /// Split-row contract: the gauges (`context  Ctrl+O`, `rate Ctrl+S`)
    /// anchor the left half, and the identity group (`model effort
    /// @instance`) pins right — reading left → right as **context → speed →
    /// identity**. Under width pressure the keycap hints drop first, then
    /// the instance suffix (provenance is nice-to-have) while the model
    /// name, effort tag, context meter, and stream rate all still fit.
    #[test]
    fn model_bar_orders_context_speed_then_model() {
        let row_text = |width: u16, tps: Option<f64>| -> String {
            let mut terminal = mutx_engine::TestTerminal::new(width, 1);
            terminal.draw(|f| {
                draw_model_bar(
                    f,
                    Rect::new(0, 0, width, 1),
                    ModelBarView {
                        current_model: "kimi-k2.7-code",
                        model_available: true,
                        provider_name: Some("kimi-code"),
                        reasoning_effort: Some("max"),
                        last_turn_tps: tps,
                        ..Default::default()
                    },
                    &Theme::default(),
                );
            });
            let buf = terminal.buffer();
            (0..buf.area().width as usize)
                .map(|x| buf.content[x].symbol().to_string())
                .collect::<String>()
        };

        // Wide enough for everything: `ctx Ctrl+O  rate Ctrl+S` left,
        // `model effort @instance` right, in that left-to-right order.
        let wide = row_text(80, Some(47.8));
        let ctx_pos = wide.find("(0%)").expect("context meter shown");
        let rate_pos = wide.find("47.8 tok/s").expect("stream rate shown");
        let model_pos = wide.find("kimi-k2.7-code").expect("model shown");
        assert!(
            ctx_pos < rate_pos && rate_pos < model_pos,
            "row must read context → speed → identity: {wide:?}"
        );
        let inst_pos = wide.find("@kimi-code").expect("instance suffix shown");
        assert!(model_pos < inst_pos, "instance follows the model: {wide:?}");
        // Progressive disclosure: each gauge trails its drill-down keycap.
        let ctx_key = wide.find("Ctrl+O").expect("context keycap hint shown");
        let rate_key = wide.find("Ctrl+S").expect("performance keycap hint shown");
        assert!(
            ctx_pos < ctx_key && rate_pos < rate_key,
            "keycaps trail their gauges: {wide:?}"
        );
        // Justified split: the identity cluster pins flush to the row's
        // right edge (mirrored `inner` indent).
        assert!(
            wide.trim_end().ends_with("kimi-k2.7-code max @kimi-code"),
            "identity must end at the right edge: {wide:?}"
        );

        // No TPS sample yet: the rate gauge (and its keycap) hide entirely —
        // no `– tok/s` placeholder noise before the first turn completes.
        let cold = row_text(80, None);
        assert!(
            !cold.contains("tok/s") && !cold.contains("Ctrl+S"),
            "rate gauge must hide without a sample: {cold:?}"
        );
        assert!(
            cold.contains("(0%)") && cold.contains("Ctrl+O"),
            "context gauge and its keycap survive: {cold:?}"
        );

        // Narrower row: the keycap hints drop first (52 still keeps the
        // provenance suffix), then the instance suffix (46), while the
        // gauges, model name, and effort tag survive in order.
        let narrow = row_text(52, Some(47.8));
        assert!(
            !narrow.contains("Ctrl+O") && !narrow.contains("Ctrl+S"),
            "keycap hints hide first: {narrow:?}"
        );
        assert!(
            narrow.contains("@kimi-code"),
            "provenance suffix survives at 52: {narrow:?}"
        );
        let tighter = row_text(46, Some(47.8));
        assert!(
            !tighter.contains('@'),
            "instance should hide next: {tighter:?}"
        );
        let ctx_pos = tighter.find("(0%)").expect("context survives");
        let rate_pos = tighter.find("tok/s").expect("rate survives");
        let model_pos = tighter.find("kimi-k2.7-code").expect("model survives");
        assert!(
            ctx_pos < rate_pos && rate_pos < model_pos,
            "order must hold after dropping the hints: {tighter:?}"
        );
    }

    #[test]
    fn model_bar_renders_model_and_context() {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(80, 3);
        terminal.draw(|f| {
            draw_model_bar(
                f,
                Rect::new(0, 2, 80, 1),
                ModelBarView {
                    current_model: "mock-model",
                    model_available: true,
                    provider_name: Some("mock-instance"),
                    ..Default::default()
                },
                &theme,
            );
        });
    }

    #[test]
    fn model_bar_renders_unavailable_model_indicator() {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(80, 1);
        terminal.draw(|f| {
            draw_model_bar(
                f,
                Rect::new(0, 0, 80, 1),
                ModelBarView {
                    current_model: "old-delisted-model",
                    model_available: false,
                    provider_name: Some("zai-code"),
                    ..Default::default()
                },
                &theme,
            );
        });
        let buf = terminal.buffer();
        let text = (0..buf.area().width as usize)
            .map(|x| buf.content[x].symbol().to_string())
            .collect::<String>();
        assert!(
            text.contains("old-delisted-model [unavailable]"),
            "row was {text:?}"
        );
    }

    #[test]
    fn model_bar_click_rects_follow_context_speed_order() {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(80, 1);

        let mut captured = ModelBarRects::default();
        terminal.draw(|f| {
            captured = draw_model_bar(
                f,
                Rect::new(0, 0, 80, 1),
                ModelBarView {
                    current_model: "kimi-k2.7-code",
                    provider_name: None,
                    last_turn_tps: Some(47.8),
                    ..Default::default()
                },
                &theme,
            );
        });
        let ctx = captured.context.expect("context rect present");
        let perf = captured.performance.expect("performance rect present");
        assert!(
            ctx.x + ctx.width <= perf.x,
            "context meter must sit left of the stream-rate segment"
        );
        // The gauges anchor the row's left edge: the context rect starts at
        // the inner indent, one cell in.
        assert_eq!(ctx.x, 1, "gauges must lead the row from the left indent");
        // Both rects carry exactly their own segment's text — gauge value
        // plus its trailing keycap hint (one click target per drill-down).
        let buf = terminal.buffer();
        let slice = |r: Rect| -> String {
            (r.x..r.x + r.width)
                .map(|x| buf[(x, r.y)].symbol().to_string())
                .collect::<String>()
        };
        assert_eq!(slice(ctx), "0 (0%) Ctrl+O", "context rect mismatch");
        assert_eq!(slice(perf), "47.8 tok/s Ctrl+S", "rate rect mismatch");
        // The identity cluster sits right of the gauges, pinned to the row's
        // right edge (one trailing indent cell).
        let row: String = (0..80).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        let model_pos = row.find("kimi-k2.7-code").expect("model on the row");
        assert!(
            perf.x + perf.width <= model_pos as u16,
            "identity must sit right of the gauges: {row:?}"
        );
        assert_eq!(
            &row[80 - 1 - "kimi-k2.7-code".len()..80 - 1],
            "kimi-k2.7-code",
            "model must end at the right indent: {row:?}"
        );
    }

    #[test]
    fn model_bar_reasoning_tag_shows_effort_when_set() {
        // Render the full model row for three effort states and read back the
        // whole line: the bare `{effort}` tag must appear right after the
        // model name when reasoning is in use and be absent entirely
        // otherwise.
        fn row_text(effort: Option<&str>) -> String {
            let mut terminal = mutx_engine::TestTerminal::new(80, 1);
            terminal.draw(|f| {
                draw_model_bar(
                    f,
                    Rect::new(0, 0, 80, 1),
                    ModelBarView {
                        current_model: "mock",
                        reasoning_effort: effort,
                        ..Default::default()
                    },
                    &Theme::default(),
                );
            });
            let buf = terminal.buffer();
            (0..buf.area().width as usize)
                .map(|x| buf.content[x].symbol().to_string())
                .collect::<String>()
                .trim()
                .to_string()
        }

        // No reasoning → no effort word anywhere on the row.
        let off = row_text(None);
        assert!(!off.contains("high"), "effort leaked in: {off:?}");
        assert!(!off.contains('◆'), "no diamond glyph in: {off:?}");
        // Reasoning on → bare `high` appears after the model name.
        let on = row_text(Some("high"));
        assert!(on.contains("high"), "missing effort tag in: {on:?}");
        let model_pos = on.find("mock").expect("model name on the row");
        let effort_pos = on.find("high").expect("effort tag");
        assert!(model_pos < effort_pos, "effort must follow the model name");
        // A different effort level renders its own value, not a hardcoded one.
        assert!(row_text(Some("max")).contains("max"));
    }

    #[test]
    fn model_bar_shows_the_instance_suffix_after_the_model_name() {
        // The `@<instance>` suffix must trail the model name so identical
        // models served by different instances stay attributable — and must
        // vanish entirely when no instance is known.
        fn row_text(provider_name: Option<&str>) -> String {
            let mut terminal = mutx_engine::TestTerminal::new(80, 1);
            terminal.draw(|f| {
                draw_model_bar(
                    f,
                    Rect::new(0, 0, 80, 1),
                    ModelBarView {
                        current_model: "mock",
                        provider_name,
                        ..Default::default()
                    },
                    &Theme::default(),
                );
            });
            let buf = terminal.buffer();
            (0..buf.area().width as usize)
                .map(|x| buf.content[x].symbol().to_string())
                .collect::<String>()
                .trim()
                .to_string()
        }

        let named = row_text(Some("kimi-code"));
        assert!(
            named.contains("@kimi-code"),
            "missing @instance in: {named:?}"
        );
        // The suffix is the last segment of the identity group:
        // `model effort @instance`.
        let model_pos = named.find("mock").expect("model name on the row");
        let inst_pos = named.find("@kimi-code").expect("instance suffix");
        assert!(model_pos < inst_pos, "instance must follow the model name");
        // Unknown / empty instance → no `@` anywhere on the row.
        assert!(!row_text(None).contains('@'));
        assert!(!row_text(Some("")).contains('@'));
    }

    #[test]
    fn model_bar_full_cluster_orders_model_effort_instance() {
        // The right cluster reads `Kimi K3 max @kimi-code` — effort tight
        // after the model name, the @instance provenance last. The identity
        // group (`model effort @instance`) joins with single spaces; it sits
        // across the wider gap from the left-anchored gauges.
        let mut terminal = mutx_engine::TestTerminal::new(120, 1);
        terminal.draw(|f| {
            draw_model_bar(
                f,
                Rect::new(0, 0, 120, 1),
                ModelBarView {
                    current_model: "mock",
                    provider_name: Some("kimi-code"),
                    reasoning_effort: Some("max"),
                    ..Default::default()
                },
                &Theme::default(),
            );
        });
        let buf = terminal.buffer();
        let text = (0..buf.area().width as usize)
            .map(|x| buf.content[x].symbol().to_string())
            .collect::<String>();
        let model_pos = text.find("mock").expect("model name");
        let effort_pos = text.find("max").expect("effort");
        let inst_pos = text.find("@kimi-code").expect("instance suffix");
        assert!(
            model_pos < effort_pos && effort_pos < inst_pos,
            "expected `model effort @instance` order in: {text:?}"
        );
        assert!(
            text.contains("mock max @kimi-code"),
            "identity group should join with single spaces in: {text:?}"
        );
    }

    #[test]
    fn model_bar_ignition_label_takes_over_the_identity_cluster() {
        // During the ignition's label phase the right cluster swaps the
        // whole `model effort @instance` identity for the converging `M A X`
        // label; once the phase ends the normal cluster returns.
        fn row_text(elapsed_ms: Option<u128>) -> String {
            let mut terminal = mutx_engine::TestTerminal::new(100, 1);
            terminal.draw(|f| {
                draw_model_bar(
                    f,
                    Rect::new(0, 0, 100, 1),
                    ModelBarView {
                        current_model: "k3",
                        provider_name: Some("kimi-code"),
                        reasoning_effort: Some("max"),
                        context_tokens: Some(12_400),
                        ignition_elapsed_ms: elapsed_ms,
                        ..Default::default()
                    },
                    &Theme::default(),
                );
            });
            let buf = terminal.buffer();
            (0..buf.area().width as usize)
                .map(|x| buf.content[x].symbol().to_string())
                .collect::<String>()
        }

        // Mid-label-phase: the `M A X` label replaces the identity cluster.
        let label = row_text(Some(900));
        assert!(
            label.contains('M') && label.contains('A') && label.contains('X'),
            "label phase must render M A X: {label:?}"
        );
        assert!(
            !label.contains("@kimi-code"),
            "instance cluster is hidden during the label takeover: {label:?}"
        );

        // After the label phase the identity cluster is back, effort included.
        let settled = row_text(Some(1250));
        assert!(settled.contains("max"), "effort returns: {settled:?}");
        assert!(
            settled.contains("@kimi-code"),
            "instance returns: {settled:?}"
        );

        // No ignition at all renders the ordinary cluster.
        let plain = row_text(None);
        assert!(plain.contains("k3"), "model id renders: {plain:?}");
    }

    /// Paint the completion menu into a test buffer and return the rect the
    /// popup actually occupied (found by scanning for the popup background),
    /// so assertions can check alignment and full-width highlighting without
    /// duplicating the layout math.
    fn paint_completion_menu(
        input_anchor_x: u16,
        selected: Option<usize>,
    ) -> (mutx_engine::TestTerminal, Rect) {
        let theme = Theme::default();
        let completions = vec![
            crate::completion::Completion {
                label: "/repeat".to_string(),
                description: "Schedule a prompt on a cron".to_string(),
                insert_text: "/repeat".to_string(),
                replace_start: 0,
                replace_end: 2,
                kind: crate::completion::CompletionItemKind::Slash,
                alias_of: None,
                doc: None,
            },
            crate::completion::Completion {
                label: "/permissions".to_string(),
                description: "Manage permissions".to_string(),
                insert_text: "/permissions".to_string(),
                replace_start: 0,
                replace_end: 2,
                kind: crate::completion::CompletionItemKind::Slash,
                alias_of: None,
                doc: None,
            },
        ];
        let mut terminal = mutx_engine::TestTerminal::new(80, 12);
        terminal.draw(|f| {
            let mut layout_map = LayoutMap::new();
            draw_completion_menu(
                f,
                &mut layout_map,
                &completions,
                selected,
                Rect::new(0, 10, 80, 2), // input box occupies rows 10..12
                input_anchor_x,
                &theme,
            );
        });
        // The two rows directly above the input box are the popup.
        (terminal, Rect::new(0, 8, 80, 2))
    }

    #[test]
    fn completion_menu_left_edge_aligns_with_anchor_column() {
        let (terminal, popup) = paint_completion_menu(2, None);
        let buf = terminal.buffer();
        let body = Theme::default().body();
        // Row start of the popup: cells left of the anchor column keep the
        // app background; the popup body starts exactly at the anchor column.
        let y = popup.y;
        let at_anchor = buf.get(2, y).expect("cell at anchor column");
        assert_eq!(at_anchor.bg, body, "popup body must start at the anchor");
        assert_eq!(at_anchor.symbol(), "/");
        let left_of_anchor = buf.get(1, y).expect("cell left of anchor");
        assert_ne!(
            left_of_anchor.bg, body,
            "popup must not start before the anchor"
        );
    }

    #[test]
    fn completion_menu_selected_row_is_one_solid_band_full_width() {
        let theme = Theme::default();
        let (terminal, popup) = paint_completion_menu(2, Some(0));
        let buf = terminal.buffer();
        let brand = theme.brand();
        let body = theme.body();
        let y = popup.y; // first popup row = selected row
        // Find the popup's horizontal extent on this row (cells whose bg is
        // the popup body/brand rather than the app background).
        let row_cells: Vec<u16> = (0..buf.area().width)
            .filter(|&x| {
                let bg = buf.get(x, y).map(|c| c.bg);
                bg == Some(brand) || bg == Some(body)
            })
            .collect();
        assert!(!row_cells.is_empty(), "popup row not found");
        let (first, last) = (*row_cells.first().unwrap(), *row_cells.last().unwrap());
        // Every cell of the selected row inside the popup extent carries the
        // selection background — label, the padding between label and
        // description, and the fill out to the popup's right edge — so the
        // highlight reads as one continuous band.
        for x in first..=last {
            assert_eq!(
                buf.get(x, y).map(|c| c.bg),
                Some(brand),
                "cell ({x}, {y}) broke the selection band"
            );
        }
        // The band spans across the menu width for the candidate.
        assert!(
            last - first >= 12,
            "popup band too narrow: {first}..={last}"
        );
        // The unselected row keeps the popup body background across its full
        // width (no brand cell leaks onto it).
        let second_row = popup.y + 1;
        for x in first..=last {
            assert_eq!(
                buf.get(x, second_row).map(|c| c.bg),
                Some(body),
                "cell ({x}, {second_row}) of the unselected row lost the body bg"
            );
        }
    }

    #[test]
    fn completion_menu_caps_width_and_stays_anchored() {
        let theme = Theme::default();
        let completions = [
            ("/models", "Switch the active model"),
            ("/tools", "Manage session tools (enable/disable)"),
            (
                "/delegate",
                "Toggle delegated autonomous mode — agent runs without human intervention (on/off)",
            ),
        ]
        .iter()
        .map(|(l, d)| crate::completion::Completion {
            label: l.to_string(),
            description: d.to_string(),
            insert_text: l.to_string(),
            replace_start: 0,
            replace_end: 1,
            kind: crate::completion::CompletionItemKind::Slash,
            alias_of: None,
            doc: None,
        })
        .collect::<Vec<_>>();
        let mut terminal = mutx_engine::TestTerminal::new(80, 12);
        terminal.draw(|f| {
            let mut layout_map = LayoutMap::new();
            draw_completion_menu(
                f,
                &mut layout_map,
                &completions,
                None,
                Rect::new(0, 10, 80, 2),
                2,
                &theme,
            );
        });
        let buf = terminal.buffer();
        let body = theme.body();
        // The popup keeps its anchor: body-colored cells start at column 2,
        // never stretch to the right edge of the 80-column viewport.
        let y = 9u16; // last popup row (3 candidates above rows 10..12)
        let cells: Vec<u16> = (0..80u16)
            .filter(|&x| buf.get(x, y).map(|c| c.bg) == Some(body))
            .collect();
        let (first, last) = (*cells.first().unwrap(), *cells.last().unwrap());
        assert_eq!(first, 2, "popup must stay anchored at the typed token");
        assert!(
            (last - first + 1) as usize <= 80 * 3 / 5,
            "popup must not fill the viewport: {first}..={last}"
        );
        let row_text: String = (first..=last)
            .filter_map(|x| buf.get(x, y).map(|c| c.symbol().to_string()))
            .collect();
        assert!(row_text.starts_with("/delegate"), "row was {row_text:?}");
    }

    #[test]
    fn completion_menu_renders_compact_entry_list_without_inline_descriptions() {
        let (terminal, popup) = paint_completion_menu(2, None);
        let buf = terminal.buffer();
        let row_text = |y: u16| -> String {
            (0..buf.area().width)
                .filter_map(|x| buf.get(x, y).map(|c| c.symbol().to_string()))
                .collect()
        };
        let first = row_text(popup.y);
        // Pure command entry in the left menu: no inline description text or separator
        assert!(first.contains("/repeat"), "row was {first:?}");
        assert!(
            !first.contains("Schedule a prompt on a cron"),
            "inline description should not appear in candidate list: {first:?}"
        );
        assert!(!first.contains('·'), "row was {first:?}");
    }

    #[test]
    fn completion_menu_marks_alias_rows_with_canonical_target() {
        // An alias candidate is marked with [*] in the menu list.
        let theme = Theme::default();
        let completions = vec![
            crate::completion::Completion {
                label: "/delegate".to_string(),
                description: "Toggle delegated mode".to_string(),
                insert_text: "/delegate".to_string(),
                replace_start: 0,
                replace_end: 2,
                kind: crate::completion::CompletionItemKind::Slash,
                alias_of: None,
                doc: None,
            },
            crate::completion::Completion {
                label: "/yolo".to_string(),
                description: "Toggle delegated mode".to_string(),
                insert_text: "/delegate".to_string(),
                replace_start: 0,
                replace_end: 2,
                kind: crate::completion::CompletionItemKind::SlashAlias,
                alias_of: Some("/delegate".to_string()),
                doc: None,
            },
        ];
        let mut terminal = mutx_engine::TestTerminal::new(80, 12);
        terminal.draw(|f| {
            let mut layout_map = LayoutMap::new();
            draw_completion_menu(
                f,
                &mut layout_map,
                &completions,
                None,
                Rect::new(0, 10, 80, 2),
                2,
                &theme,
            );
        });
        let buf = terminal.buffer();
        let row_text = |y: u16| -> String {
            (0..buf.area().width)
                .filter_map(|x| buf.get(x, y).map(|c| c.symbol().to_string()))
                .collect()
        };
        let alias_row = row_text(9); // popup bottom row = second candidate
        assert!(
            alias_row.trim_start().starts_with("/yolo [*]"),
            "alias shows [*] marker: {alias_row:?}"
        );
        let canonical_row = row_text(8);
        assert!(
            canonical_row.trim_start().starts_with("/delegate"),
            "canonical row is plain: {canonical_row:?}"
        );
        assert!(
            !canonical_row.contains("[*]"),
            "canonical rows carry no alias marker: {canonical_row:?}"
        );
    }

    #[test]
    fn completion_menu_hover_doc_flyout_only_appears_when_entry_is_selected() {
        let theme = Theme::default();
        let doc = crate::completion::CommandDoc {
            name: "/schedule".to_string(),
            summary: "Schedule a prompt on a cron or countdown".to_string(),
            usage: vec!["/schedule <when> <prompt>".to_string()],
            category: Some("Automation".to_string()),
            subcommands: vec![
                ("list".to_string(), "List scheduled prompts".to_string()),
                (
                    "cancel".to_string(),
                    "Cancel one schedule by id".to_string(),
                ),
            ],
        };
        let completions = vec![crate::completion::Completion {
            label: "/schedule".to_string(),
            description: "Schedule a prompt".to_string(),
            insert_text: "/schedule".to_string(),
            replace_start: 0,
            replace_end: 2,
            kind: crate::completion::CompletionItemKind::Slash,
            alias_of: None,
            doc: Some(doc),
        }];

        // 1. Unselected (selected = None): No right-side doc window is rendered
        let mut term_unselected = mutx_engine::TestTerminal::new(80, 12);
        term_unselected.draw(|f| {
            let mut layout_map = LayoutMap::new();
            draw_completion_menu(
                f,
                &mut layout_map,
                &completions,
                None,
                Rect::new(0, 10, 80, 2),
                2,
                &theme,
            );
        });
        let buf_unselected = term_unselected.buffer();
        let panel_bg = theme.panel();
        let has_panel_unselected =
            (0..80u16).any(|x| buf_unselected.get(x, 9).map(|c| c.bg) == Some(panel_bg));
        assert!(
            !has_panel_unselected,
            "unselected completion must not render hover doc flyout"
        );

        // 2. Selected (selected = Some(0)): Right-side doc window is rendered with panel_bg
        let mut term_selected = mutx_engine::TestTerminal::new(80, 12);
        term_selected.draw(|f| {
            let mut layout_map = LayoutMap::new();
            draw_completion_menu(
                f,
                &mut layout_map,
                &completions,
                Some(0),
                Rect::new(0, 10, 80, 2),
                2,
                &theme,
            );
        });
        let buf_selected = term_selected.buffer();
        let has_panel_selected =
            (0..80u16).any(|x| buf_selected.get(x, 9).map(|c| c.bg) == Some(panel_bg));
        assert!(
            has_panel_selected,
            "selected completion must render hover doc flyout with panel bg"
        );
    }

    #[test]
    fn completion_menu_hover_doc_flyout_shows_alias_to_target_header() {
        let theme = Theme::default();
        let doc = crate::completion::CommandDoc {
            name: "/delegate".to_string(),
            summary: "Toggle delegated mode".to_string(),
            usage: vec!["/delegate".to_string()],
            category: Some("Agent".to_string()),
            subcommands: vec![],
        };
        let completions = vec![crate::completion::Completion {
            label: "/yolo".to_string(),
            description: "Toggle delegated mode".to_string(),
            insert_text: "/delegate".to_string(),
            replace_start: 0,
            replace_end: 2,
            kind: crate::completion::CompletionItemKind::SlashAlias,
            alias_of: Some("/delegate".to_string()),
            doc: Some(doc),
        }];

        let mut term = mutx_engine::TestTerminal::new(80, 12);
        term.draw(|f| {
            let mut layout_map = LayoutMap::new();
            draw_completion_menu(
                f,
                &mut layout_map,
                &completions,
                Some(0),
                Rect::new(0, 10, 80, 2),
                2,
                &theme,
            );
        });
        let buf = term.buffer();
        let row_text = |y: u16| -> String {
            (0..buf.area().width)
                .filter_map(|x| buf.get(x, y).map(|c| c.symbol().to_string()))
                .collect()
        };
        // Check that the flyout header contains "/yolo -> /delegate"
        let full_text: Vec<String> = (0..12).map(row_text).collect();
        let found_header = full_text.iter().any(|r| r.contains("/yolo -> /delegate"));
        assert!(
            found_header,
            "flyout header should show `/yolo -> /delegate`, got buffer:\n{}",
            full_text.join("\n")
        );
    }

    /// Read back the one-row bar as joined text for assertion.
    fn queue_row_text(view: QueueBarView<'_>, width: u16, theme: &Theme) -> String {
        let mut terminal = mutx_engine::TestTerminal::new(width, 1);
        terminal.draw(|f| {
            draw_queue_bar(f, Rect::new(0, 0, width, 1), view, theme);
        });
        let buf = terminal.buffer();
        let mut out = String::new();
        for x in 0..width as usize {
            out.push_str(buf.content[x].symbol());
        }
        out.push('\n');
        out
    }

    #[test]
    fn queue_bar_leads_with_brand_tag_on_a_plain_surface() {
        // Matching the todo bar: the `QUEUE` tag leads at the gutter in the
        // brand accent on the plain frame surface — no tray glyph, no raised
        // tint — so the two bars read as one quiet family.
        let theme = Theme::default();
        let item = QueueItemView {
            queued_at_ms: 1_700_000_000_000,
            text: "fix the flaky test".to_string(),
            steering: false,
        };
        let mut terminal = mutx_engine::TestTerminal::new(70, 1);
        terminal.draw(|f| {
            draw_queue_bar(
                f,
                Rect::new(0, 0, 70, 1),
                QueueBarView {
                    items: &[item],
                    paused: false,
                    blocked: false,
                },
                &theme,
            );
        });
        let cells = terminal.buffer().content.clone();

        // (1) The tag leads at the gutter, brand-colored.
        assert_eq!(cells[0].symbol(), "Q", "expected 'QUEUE' tag at col 0");
        assert_eq!(cells[0].fg(), theme.brand(), "QUEUE tag not brand-colored");

        // (2) The bar sits on the plain surface: no raised tint anywhere
        // (sample the row's trailing cell too).
        assert_eq!(cells[0].bg(), Color::Reset, "tag must not sit on a tint");
        assert_eq!(cells[69].bg(), Color::Reset, "the row must stay plain");
    }

    #[test]
    fn queue_bar_empty_state_hints_how_to_stage() {
        let text = queue_row_text(
            QueueBarView {
                items: &[],
                paused: false,
                blocked: false,
            },
            70,
            &Theme::default(),
        );
        // Identity + zero count on the single row; no time label anymore.
        assert!(text.contains("QUEUE 0"), "row was {text:?}");
        assert!(!text.contains("--:--"), "time label leaked: {text:?}");
        // The layout hides an empty queue, so the bar renders no hint for it.
        assert!(!text.contains("queue empty"), "empty hint leaked: {text:?}");
    }

    #[test]
    fn queue_bar_previews_next_item_with_count_and_text() {
        let item = QueueItemView {
            queued_at_ms: 1_700_000_000_000,
            text: "fix the flaky test in parser".to_string(),
            steering: false,
        };
        let text = queue_row_text(
            QueueBarView {
                items: &[item],
                paused: true,
                blocked: false,
            },
            92,
            &Theme::default(),
        );
        // Identity + count reflects the one item; no time label anymore.
        assert!(text.contains("QUEUE 1"), "row was {text:?}");
        assert!(!text.contains(":"), "time label leaked: {text:?}");
        // Legend: the keycap units are same-rank peers (R2) — joined by
        // plain whitespace, never a `·` (which would imply one modifies the
        // other).
        assert!(
            text.contains("Ctrl+P block  Ctrl+Q expand"),
            "peer keycaps must use R2 whitespace: {text:?}"
        );
        assert!(!text.contains('·'), "no R1 dot between peers: {text:?}");
        // A live insert is transcript-owned (ADR-0126) and never rides the
        // bar, so every bar item previews plainly — no `steer›` badge.
        assert!(!text.contains("steer›"), "steer badge leaked: {text:?}");
        // The preview rides inline on the same row.
        assert!(
            text.contains("fix the flaky test"),
            "preview text missing: {text:?}"
        );
    }

    #[test]
    fn queue_bar_ignores_the_legacy_steer_flag() {
        // A busy-Enter steer is a transcript entry, not
        // an outbox item, so a `steering: true` view item can no longer be
        // produced by the projection. If one ever leaks through (a stale
        // snapshot), the bar must still render it as an ordinary next-round
        // entry — the badge vocabulary is retired with the state it marked.
        let item = QueueItemView {
            queued_at_ms: 1_700_000_000_000,
            text: "also cover the edge case".to_string(),
            steering: true,
        };
        let text = queue_row_text(
            QueueBarView {
                items: &[item],
                paused: false,
                blocked: false,
            },
            92,
            &Theme::default(),
        );
        assert!(
            !text.contains("steer›"),
            "steer badge must not render: {text:?}"
        );
        assert!(
            text.contains("also cover the edge case"),
            "preview text missing: {text:?}"
        );
    }

    #[test]
    fn queue_bar_never_renders_the_tab_affordance() {
        // The Tab toggle for the insert/next-round send target was removed —
        // a busy Enter always queues for the next round — so the queue bar's
        // legend must never mention Tab.
        let item = QueueItemView {
            queued_at_ms: 1_700_000_000_000,
            text: "add a comment".to_string(),
            steering: false,
        };
        let text = queue_row_text(
            QueueBarView {
                items: &[item],
                paused: false,
                blocked: false,
            },
            70,
            &Theme::default(),
        );
        assert!(!text.contains("Tab"), "tab legend leaked: {text:?}");
        // A non-steering item never wears the mid-round `steer›` badge.
        assert!(!text.contains("steer›"), "steer badge leaked: {text:?}");
    }
}
