//! Transient chrome around the input box: the activity bar with an animated
//! breathing-dot indicator, the one-row todo bar that surfaces the live task
//! list, the completion menu anchored above the input, and the persistent hint
//! bar pinned below the input. The activity bar (transient liveness) and the
//! todo bar (the agent's live task list) are the click targets that open the
//! Activity modal.

use neenee_tui_engine::{
    Block as RtBlock, Clear, Color, Frame, Line, Modifier, Paragraph, Rect, Span, Style,
};
use std::time::{Duration, Instant};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::tui::model::document::TranscriptMessage;
use crate::tui::model::layout::LayoutMap;

use super::Theme;
use super::components::keycap::{keycap_span, keycap_style};
use super::design::{
    BAR_LEGEND_GAP_MIN, HINT_BAR_GAP_MIN, HINT_BAR_INNER_PADDING, HINT_BAR_MODEL_GAP,
    HINT_BAR_SEGMENT_GAP, JOIN_ENUMERATE_COLS, JOIN_MODIFY,
};
use super::keymap::Key;
use super::primitives::{contrast_fg, viewport_rect};

/// Number of distinct luminance steps in one breathing cycle. At the 100ms
/// spinner tick this is ~1.2s per cycle — calm, not frantic.
pub const SPINNER_PHASES: usize = 12;

/// The shimmer crosses the full status label every two seconds. The app's
/// animation phase advances every 100ms, so 20 phases produce one sweep.
const SHIMMER_PHASES: usize = 20;
const SHIMMER_PADDING: usize = 6;
const SHIMMER_HALF_WIDTH: f32 = 4.0;

/// The activity indicator glyph: a single dot whose luminance breathes (see
/// [`breathing_color`]) rather than a cycling braille frame. Replaces the old
/// braille spinner for a quieter, less busy feel.
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

/// Render an animated luminance band across the live status label. The muted
/// base keeps the row quiet while the brand-colored highlight supplies a clear
/// left-to-right liveness cue.
fn shimmer_spans(text: &str, phase: usize, theme: &Theme) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }

    let period = chars.len() + SHIMMER_PADDING * 2;
    let sweep = (phase % SHIMMER_PHASES) as f32 / SHIMMER_PHASES as f32 * period as f32;
    let base = rgb_of(theme.muted());
    let highlight = rgb_of(theme.brand());

    chars
        .into_iter()
        .enumerate()
        .map(|(index, ch)| {
            let position = index as f32 + SHIMMER_PADDING as f32;
            let distance = (position - sweep).abs();
            let intensity = if distance <= SHIMMER_HALF_WIDTH {
                let x = std::f32::consts::PI * distance / SHIMMER_HALF_WIDTH;
                0.5 * (1.0 + x.cos())
            } else {
                0.0
            };
            let color = Color::Rgb(
                lerp_u8(base.0, highlight.0, intensity),
                lerp_u8(base.1, highlight.1, intensity),
                lerp_u8(base.2, highlight.2, intensity),
            );
            let mut style = Style::default().fg(color).add_modifier(Modifier::ITALIC);
            if intensity >= 0.65 {
                style = style.add_modifier(Modifier::BOLD);
            }
            Span::styled(ch.to_string(), style)
        })
        .collect()
}

/// Draw the transient activity bar that sits directly above the input box
/// (below the ambient todo/queue meta bars). Replaces the old inline
/// `┃ neenee ⟳ <status>` indicator: the brand prefix is dropped (the header
/// already shows it) and the static `⟳` glyph is replaced by a breathing-dot
/// indicator so the harness never looks frozen.
///
/// Layout:
/// ```text
/// <spinner> <status> (<elapsed> · Esc Esc interrupt) [· » <pursuit>] [⚠ <alert>]
/// ```
/// The whole bar is transient (turn-scoped): it shows only while a round is
/// active and is hidden while idle, so the row returns to the transcript.
/// Session-state flags such as `autopilot` deliberately do not live here:
/// they live on the head row at the top of the view
/// ([`crate::tui::page_header::draw_page_header`]) so this row stays a pure activity surface. The
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
/// status, whether a pursuit/plan is in flight, and how long the round has
/// run — and is the click target that opens the Activity modal for the full
/// detail. The structural counters (`round N · turn M · <model>`) live in the
/// modal: they change rarely and take space, while the bar is a glance
/// surface. Segments are omitted when there is nothing to report:
/// - pursuit badge only when a pursuit is armed (`⟴ <truncated objective>`);
/// - elapsed only while the round timer is running;
/// - the whole bar only while a round is active.
///
/// When the status string already carries a reason (e.g.
/// `retry 1/4 in 3s · <message>`), it flows through unchanged as the lead.
///
/// Returns `Some(rect)` when the bar is drawn so the event loop can hit-test
/// clicks and open the Activity modal; `None` when the bar is hidden (idle).
#[allow(clippy::too_many_arguments)]
pub fn draw_activity_bar(
    frame: &mut Frame,
    rect: Rect,
    review_alert: &str,
    round_started_at: Option<Instant>,
    status: &str,
    awaiting_permission: bool,
    spinner_phase: usize,
    theme: &Theme,
) -> Option<Rect> {
    // The bar is a single transient LEFT segment (spinner + shimmering status
    // + elapsed/interrupt hint + pursuit + review alert) shown only while a
    // turn is active. With nothing to report it is hidden entirely.
    let status_active = !status.is_empty() && status != "idle";
    let dim = Style::default().fg(theme.muted());

    // If there is no transient activity, hide the bar — no point painting a
    // blank row. (The persistent todo summary has its own bar below.)
    if !status_active {
        return None;
    }

    let row_width = rect.width as usize;
    let available_width = row_width;
    let elapsed = round_started_at.map(|started| format_elapsed(started.elapsed()));
    let full_hint_width = elapsed
        .as_ref()
        .map(|value| UnicodeWidthStr::width(format!(" ({value} · Esc Esc interrupt)").as_str()))
        .unwrap_or_else(|| UnicodeWidthStr::width(" (Esc Esc interrupt)"));
    let interrupt_hint_width = UnicodeWidthStr::width(" (Esc Esc interrupt)");
    let tiny_interrupt_hint_width = UnicodeWidthStr::width(" Esc Esc");
    let prefix_width = UnicodeWidthStr::width(" ● ");
    const MIN_STATUS_WIDTH: usize = 4;
    const MIN_TINY_STATUS_WIDTH: usize = 1;
    let show_elapsed =
        elapsed.is_some() && available_width >= prefix_width + full_hint_width + MIN_STATUS_WIDTH;
    let show_interrupt_words =
        show_elapsed || available_width >= prefix_width + interrupt_hint_width + MIN_STATUS_WIDTH;
    let show_interrupt_keys = show_interrupt_words
        || available_width >= prefix_width + tiny_interrupt_hint_width + MIN_TINY_STATUS_WIDTH;
    let hint_width = if show_elapsed {
        full_hint_width
    } else if show_interrupt_words {
        interrupt_hint_width
    } else if show_interrupt_keys {
        tiny_interrupt_hint_width
    } else {
        0
    };
    let status_width = available_width.saturating_sub(prefix_width + hint_width);
    let status = truncate_for_bar(status, status_width);

    let mut spans: Vec<Span> = Vec::new();
    let spinner = spinner_glyph();
    let spinner_color = breathing_color(spinner_phase, theme.brand(), theme.surface());

    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        spinner,
        Style::default()
            .fg(spinner_color)
            .add_modifier(Modifier::BOLD),
    ));

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
        spans.extend(shimmer_spans(&status, spinner_phase, theme));
    }

    // Keep the interrupt instruction immediately after the live status,
    // matching the place users look while waiting. Elapsed time is useful
    // context, but it drops before the key hint on narrow terminals.
    if show_interrupt_words {
        spans.push(Span::styled(" (", dim));
        if show_elapsed {
            spans.push(Span::styled(elapsed.unwrap_or_default(), dim));
            // R1: the elapsed time is a property of the running state.
            spans.push(Span::styled(JOIN_MODIFY, dim));
        }
        spans.push(keycap_span(theme, Key::ESC.display()));
        spans.push(Span::styled(" ", dim));
        spans.push(keycap_span(theme, Key::ESC.display()));
        spans.push(Span::styled(" interrupt)", dim));
    } else if show_interrupt_keys {
        // At the minimum supported terminal width, keep the actual keys and
        // drop only the explanatory words. The Activity help entry supplies
        // the long form if the user needs it.
        spans.push(Span::styled(" ", dim));
        spans.push(keycap_span(theme, Key::ESC.display()));
        spans.push(Span::styled(" ", dim));
        spans.push(keycap_span(theme, Key::ESC.display()));
    }

    // Session-review alert (ADR-0016): surfaced when a periodic diagnostic
    // judged the turn watch-worthy or stuck. Rendered with the same breathing
    // luminance sweep as the running-indicator dot so the alert pulses gently
    // rather than sitting as a flat warning chip — the motion draws the eye
    // without being frantic. The interrupt hint before it already tells the
    // user how to stop the turn. Empty alert = clear (nothing rendered).
    if !review_alert.is_empty() {
        let warn = breathing_color(spinner_phase, theme.warning, theme.surface());
        // U+FE0E (text presentation selector) forces the warning sign to
        // render as a 1-cell text glyph; without it some terminals pick
        // emoji presentation (2 cells) while `unicode-width` counts 1,
        // leaving a stray cell / misaligned right-pin.
        let alert = format!("⚠\u{FE0E} {review_alert}");
        let alert_width = UnicodeWidthStr::width("  ") + UnicodeWidthStr::width(alert.as_str());
        let used_width: usize = spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum();
        if used_width + alert_width <= available_width {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                alert,
                Style::default().fg(warn).add_modifier(Modifier::BOLD),
            ));
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
    todos: &neenee_core::TodoList,
    theme: &Theme,
) -> Rect {
    use neenee_core::{TodoItem, TodoStatus};

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

    // Columns reserved between the identity and the legend: the ` · ` that
    // leads the preview (only when there is one) plus the legend's breathing
    // room — deliberately wider than the hint/status bar gap so the keycap
    // never reads as glued to the content.
    let content_sep = UnicodeWidthStr::width(JOIN_MODIFY);
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
        let one_line = crate::tui::overlays::common::one_line(item.content.trim());
        let preview = if one_line.width() > budget {
            crate::tui::overlays::common::truncate_ellipsis(&one_line, budget)
        } else {
            one_line
        };
        let preview_w = preview.width();
        row.push(Span::styled(JOIN_MODIFY, dim));
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
/// description dim). The selected row is highlighted as one solid band
/// across the **full popup width**, label column, padding, and trailing fill
/// included, so the highlight never fragments into per-segment blocks.
pub fn draw_completion_menu(
    frame: &mut Frame,
    _layout_map: &mut LayoutMap,
    completions: &[crate::tui::completion::Completion],
    selected_idx: Option<usize>,
    anchor: Rect,
    anchor_x: u16,
    theme: &Theme,
) {
    if completions.is_empty() {
        return;
    }

    const MAX_VISIBLE: usize = 6;

    // Windowing: `suggestion_index` is the global index into the full list,
    // but only `MAX_VISIBLE` rows fit on screen. Without a scroll offset the
    // highlight would scroll off the bottom (and the up-arrow wrap path
    // would land on a row that is never rendered). The offset is recomputed
    // every frame from `selected_idx` so it tracks the cursor live:
    //   - when the cursor moves below the visible window, scroll down one
    //     row at a time so it stays on the last visible line;
    //   - when the cursor moves above (e.g. ↑ wraps from 0 to len-1), jump
    //     the window so the cursor sits on the last visible line;
    //   - otherwise leave it alone so short up/down moves inside the window
    //     don't jitter the list.
    let total = completions.len();
    let scroll_offset = match selected_idx {
        // Once the cursor passes the first page (sel >= MAX_VISIBLE), pin it
        // to the last visible row and slide the window up under it — that way
        // every ↓ just brings the next candidate into view at the bottom.
        // For the wrap path (↑ from 0 to len-1), `sel - (MAX_VISIBLE - 1)`
        // also yields the correct bottom-anchored window. Below MAX_VISIBLE,
        // the window stays at the top so short moves don't jitter the list.
        Some(sel) if sel >= MAX_VISIBLE && total > MAX_VISIBLE => {
            (sel - (MAX_VISIBLE - 1)).min(total - MAX_VISIBLE)
        }
        _ => 0,
    };
    let window_end = (scroll_offset + MAX_VISIBLE).min(total);
    let visible_rows = &completions[scroll_offset..window_end];
    let visible_count = visible_rows.len();
    let popup_height = visible_count as u16;

    let viewport = viewport_rect(frame);

    // Compute width from content. The description column is dropped entirely
    // (separator + padding) when no candidate carries a description — the
    // `@path` menu uses empty descriptions for a plain list of paths,
    // matching opencode's minimal aesthetic. Width is derived from the full
    // candidate list (not just the visible window) so the popup doesn't
    // resize as the user scrolls.
    let any_desc = completions.iter().any(|c| !c.description.is_empty());
    let max_cmd = completions
        .iter()
        .map(|c| c.label.width())
        .max()
        .unwrap_or(0);
    let max_desc = if any_desc {
        completions
            .iter()
            .map(|c| c.description.width())
            .max()
            .unwrap_or(0)
    } else {
        0
    };
    // The popup never grows past a compact share of the viewport: the slash
    // menu's longest description (e.g. /autopilot's) would otherwise fill
    // the whole row on a standard 80-column terminal, pushing the popup's
    // leading edge off the typed token and breaking the visual anchor.
    // Over-long descriptions truncate with an ellipsis (see the row builder
    // below) instead of stretching the menu.
    let max_popup_width = ((viewport.width as usize) * 3 / 5).max(24);
    // Text runs from edge to edge of the popup so the selection band can
    // paint the row solid; a single right-edge padding cell keeps the last
    // glyph off the frame boundary.
    let content_width = if any_desc {
        (max_cmd + 2 + max_desc).max(30)
    } else {
        (max_cmd + 1).max(20)
    }
    .min(max_popup_width);
    let popup_width = content_width as u16;

    // Position: try above the input box; if not enough room, clamp to top.
    // Horizontally hang the menu off the typed token (`anchor_x`); when the
    // token sits far right the popup shifts left just enough to stay on
    // screen (right-clamped), like an editor completion widget.
    let mut y = anchor.y.saturating_sub(popup_height);
    if y == 0 && anchor.y < popup_height {
        y = 0;
    }
    let x = anchor_x
        .min(viewport.right().saturating_sub(popup_width))
        .max(viewport.x);

    let area = Rect::new(x, y, popup_width.min(viewport.right() - x), popup_height);
    frame.render_widget(Clear, area);

    let block = RtBlock::default().style(Style::default().bg(theme.body()));

    let popup_w = area.width as usize;
    // The description column gets whatever the label column leaves inside
    // the capped popup width; longer descriptions truncate with an ellipsis.
    let desc_col = popup_w.saturating_sub(max_cmd + 2);
    let lines: Vec<Line> = visible_rows
        .iter()
        .enumerate()
        .map(|(row, c)| {
            // `row` is the on-screen position (0..MAX_VISIBLE); recover the
            // global index by adding the scroll offset so the highlight
            // check matches the value passed in `selected_idx`.
            let global_idx = row + scroll_offset;
            let is_selected = Some(global_idx) == selected_idx;
            let body_bg = theme.body();
            // Every span on the row shares the row background (`brand` when
            // selected, `body` otherwise) and the trailing fill spans out to
            // the popup's full width, so the highlight reads as one
            // continuous band instead of per-segment blocks.
            let row_bg = if is_selected { theme.brand() } else { body_bg };
            let cmd_style = if is_selected {
                Style::default()
                    .bg(row_bg)
                    .fg(contrast_fg(theme.brand()))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .bg(row_bg)
                    .fg(theme.fg())
                    .add_modifier(Modifier::BOLD)
            };
            let desc_style = if is_selected {
                Style::default()
                    .bg(row_bg)
                    .fg(contrast_fg(theme.brand()))
                    .add_modifier(Modifier::DIM)
            } else {
                Style::default().bg(row_bg).fg(theme.muted())
            };
            let pad_style = Style::default().bg(row_bg);

            // `command  description` in two padded columns separated by plain
            // spaces — no `·` separator; weight (bold) and brightness
            // (fg vs muted) carry the primary/secondary relationship.
            let mut used = max_cmd;
            let mut spans = vec![Span::styled(
                format!("{:<width$}", c.label, width = max_cmd),
                cmd_style,
            )];
            if any_desc {
                spans.push(Span::styled("  ", pad_style));
                let desc = if c.description.width() > desc_col {
                    truncate_for_bar(&c.description, desc_col)
                } else {
                    format!("{:<width$}", c.description, width = desc_col)
                };
                spans.push(Span::styled(desc, desc_style));
                used += 2 + desc_col;
            }
            // Solid fill to the popup edge so the selected row's highlight
            // spans the whole width, not just the text it contains.
            spans.push(Span::styled(
                " ".repeat(popup_w.saturating_sub(used)),
                pad_style,
            ));
            Line::from(spans)
        })
        .collect();

    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Inputs for [`draw_hint_bar`]. Carries the model + context-usage info that
/// the old top header showed, now collapsed onto one row. Session-level state
/// (workspace path, `autopilot`) deliberately lives on the status bar below,
/// not here.
pub struct HintBarView<'a> {
    pub current_model: &'a str,
    /// Display name of the provider instance serving the active model (the
    /// user-given instance name, e.g. `"kimi-code"`). Rendered as a muted
    /// `@<instance>` suffix right after the model name so identical models
    /// served by different instances stay distinguishable — mirroring the
    /// `· <provider>` suffix the flat Models picker shows on each row.
    /// `None` (or empty) when no instance is known (pre-snapshot startup).
    pub provider_name: Option<&'a str>,
    #[allow(dead_code)]
    pub messages: &'a [TranscriptMessage],
    /// Effective reasoning effort of the active model, shown as a `◆ {effort}`
    /// tag right after the model name — only when reasoning is actually in use
    /// for this model. The caller resolves the value and applies the
    /// per-protocol gating (Anthropic: shown only when thinking is opted in;
    /// OpenAI: shown whenever the model exposes an effort knob; Google:
    /// never), so this is `None` for models that are not reasoning. Mirrors
    /// the `◆ think on · {effort}` tag the `/models` picker shows on a row.
    pub reasoning_effort: Option<&'a str>,
    /// True while the prompt is a `!`-prefixed shell command and no transcript
    /// step is focused. The left side explains the resulting Enter action in
    /// plain language instead of exposing an implementation-mode badge.
    pub shell_active: bool,
    /// Busy-send disposition for the next Enter: the agent is mid-round, so
    /// Enter stages the message in the queue rather than sending immediately.
    pub busy: bool,
    /// Session-scoped size of the AI-visible request context. Produced by the
    /// harness from provider API usage when available, otherwise from the
    /// projected `model_window`; it is deliberately unrelated to durable or
    /// rendered transcript size. `None` is shown as zero until the first
    /// projection snapshot arrives.
    pub context_tokens: Option<usize>,
    /// Effort-ignition phase: `Some(elapsed_ms)` while the `max`-effort
    /// ignition celebration is running. During the label phase the whole
    /// right identity cluster (model / effort / instance) is replaced by the
    /// converging `M A X` label ([`super::effort_ignition::label_cluster`]);
    /// the caller's band tint paints over whatever renders here, so the
    /// takeover never fights the wave colors.
    pub ignition_elapsed_ms: Option<u128>,
}

#[derive(Clone, Copy)]
enum ActionDensity {
    Full,
    Compact,
    Tiny,
}

/// Build the left side of the bottom row as a short action sentence: send
/// when idle, queue when the agent is mid-round, or run a shell command. The
/// persistent queue bar carries the queue affordances (recall, expand), so
/// this stays a pure "what will Enter do" surface.
fn input_action_spans(
    shell_active: bool,
    busy: bool,
    density: ActionDensity,
    theme: &Theme,
    bg: Color,
) -> Vec<Span<'static>> {
    // Route the keycap through the unified keycap style (brand + bold) so the
    // "Enter" affordance matches every other keycap in the app — the activity
    // bar's Esc-to-interrupt hint, the queue bar's F2/F3 legend, the modal
    // footers — instead of hand-rolling a divergent fg+bold combination here.
    // The keycap style carries no background, so the surface tint is applied
    // here once for the whole row.
    let key_style = keycap_style(theme).bg(bg);
    let hint_style = Style::default().fg(theme.muted()).bg(bg);
    let compact = matches!(density, ActionDensity::Compact | ActionDensity::Tiny);
    let mut spans = vec![Span::styled(Key::ENTER.display(), key_style)];

    if shell_active {
        spans.push(Span::styled(
            if compact { " run" } else { " run command" },
            hint_style,
        ));
    } else if busy {
        // The agent is mid-round: Enter stages the message in the queue (the
        // queue bar below shows the staged item). The recall affordance lives
        // in the queue bar's keycap legend rather than this sentence.
        spans.push(Span::styled(
            if compact { " queue" } else { " queue message" },
            hint_style,
        ));
    } else {
        spans.push(Span::styled(" send", hint_style));
    }

    spans
}

/// Draw the single-line hint bar pinned below the input box. Carries the
/// action performed by the next Enter (left) plus the model name and
/// context-usage info (right) that the old top header showed, collapsed onto
/// one row so the transcript reclaims vertical space.
///
/// Layout: current input action on the left, right-aligned cluster of
/// `model · reasoning · context-usage` on the right. Session-level state flags
/// (such as `autopilot`) deliberately do **not** live here — they moved to the
/// status bar directly below. On narrow terminals, the action sentence compacts
/// first and ambient model metadata drops before the action.
pub fn draw_hint_bar(
    frame: &mut Frame,
    rect: Rect,
    view: HintBarView<'_>,
    theme: &Theme,
) -> Option<Rect> {
    let HintBarView {
        current_model,
        provider_name,
        messages: _,
        reasoning_effort,
        shell_active,
        busy,
        context_tokens,
        ignition_elapsed_ms,
    } = view;

    let bg = theme.surface();
    let full_w = rect.width as usize;

    // --- Left cluster: one sentence describing what the next Enter does.
    // Keep product language here: users should not need to learn the internal
    // round/turn distinction before sending. The queue affordances live in
    // the persistent queue bar, not here.
    let mut action_density = ActionDensity::Full;
    let mut zone_spans = input_action_spans(shell_active, busy, action_density, theme, bg);
    let mut zone_pill_width = zone_spans.iter().map(|s| s.content.width()).sum::<usize>();

    // --- Right cluster: model name and context bar.
    // Build each segment separately so we can drop optional ones when the
    // terminal is too narrow.
    let context_max = crate::tui::providers::model_context_window(current_model);

    let inner = HINT_BAR_INNER_PADDING;

    // Build right-side segments independently. Model identity is the last
    // ambient item to drop; reasoning effort drops first, then the instance
    // suffix, then context usage. The input action always wins when the row
    // cannot hold both clusters.
    let model_label = crate::tui::providers::model_display_name(current_model);
    let model_width = model_label.width();
    let model_spans = vec![Span::styled(
        model_label,
        Style::default()
            .fg(theme.brand())
            .add_modifier(Modifier::BOLD)
            .bg(bg),
    )];

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
    // attribute of the model — "Kimi K3 max @kimi-code  12k (1%)".
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

    // Context-usage segment: `89.2k (8%)`. Always shown when the model
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

    let mut show_model = model_width > 0;
    let mut show_reasoning = reasoning_width > 0;
    let mut show_instance = instance_width > 0;
    let mut show_context = context_seg_width > 0;
    #[allow(clippy::too_many_arguments)]
    let right_width_for = |model: bool, reasoning: bool, instance: bool, context: bool| {
        // The model name, its reasoning effort, and the provider instance
        // form one identity group (`Kimi K3 high @111xianyu`) joined by the
        // tighter model gap; only the context segment sits across the wider
        // segment gap.
        let identity_count = usize::from(model) + usize::from(reasoning) + usize::from(instance);
        let identity_width = usize::from(model) * model_width
            + usize::from(reasoning) * reasoning_width
            + usize::from(instance) * instance_width
            + identity_count.saturating_sub(1) * HINT_BAR_MODEL_GAP;
        let context_width = usize::from(context) * context_seg_width;
        let group_gap = usize::from(identity_count > 0 && context) * HINT_BAR_SEGMENT_GAP;
        identity_width + context_width + group_gap
    };
    let fits = |left_width: usize, right_width: usize| {
        inner + left_width + if right_width > 0 { HINT_BAR_GAP_MIN } else { 0 } + right_width
            <= full_w
    };

    let mut right_width = right_width_for(show_model, show_reasoning, show_instance, show_context);
    if !fits(zone_pill_width, right_width) {
        action_density = ActionDensity::Compact;
        zone_spans = input_action_spans(shell_active, busy, action_density, theme, bg);
        zone_pill_width = zone_spans.iter().map(|s| s.content.width()).sum::<usize>();
    }
    // Drop order under width pressure: the instance suffix first (pure
    // provenance — nice-to-have), then reasoning, then context, then the
    // model name. The action on the left always wins last. (Session-state
    // flags such as `autopilot` are not on this row — they live on the
    // status bar.)
    if !fits(zone_pill_width, right_width) && show_instance {
        show_instance = false;
        right_width = right_width_for(show_model, show_reasoning, show_instance, show_context);
    }
    if !fits(zone_pill_width, right_width) && show_reasoning {
        show_reasoning = false;
        right_width = right_width_for(show_model, show_reasoning, show_instance, show_context);
    }
    if !fits(zone_pill_width, right_width) && show_context {
        show_context = false;
        right_width = right_width_for(show_model, show_reasoning, show_instance, show_context);
    }
    if !fits(zone_pill_width, right_width) {
        action_density = ActionDensity::Tiny;
        zone_spans = input_action_spans(shell_active, busy, action_density, theme, bg);
        zone_pill_width = zone_spans.iter().map(|s| s.content.width()).sum::<usize>();
    }
    if !fits(zone_pill_width, right_width) && show_model {
        show_model = false;
        right_width = right_width_for(show_model, show_reasoning, show_instance, show_context);
    }

    // Ignition label takeover: during the `M A X` label phase the identity
    // cluster (and the context segment — the label needs the room) renders
    // as the converging label instead; the caller's band tint paints the
    // glow over it.
    let label_spans = ignition_elapsed_ms
        .and_then(|ms| super::effort_ignition::label_cluster(right_width, ms, bg, theme));

    let mut right_spans: Vec<Span<'static>> = Vec::new();
    if let Some(label) = label_spans {
        right_spans = label;
    } else {
        let identity_separator =
            || Span::styled(" ".repeat(HINT_BAR_MODEL_GAP), Style::default().bg(bg));
        let group_separator =
            || Span::styled(" ".repeat(HINT_BAR_SEGMENT_GAP), Style::default().bg(bg));
        // Identity group: `model effort @instance` — single-space joins.
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
        // Context usage sits across the wider segment gap.
        if show_context {
            if !right_spans.is_empty() {
                right_spans.push(group_separator());
            }
            right_spans.extend(context_spans);
        }
    }

    let left_used = inner + zone_pill_width;
    let right_rendered_width: usize = right_spans.iter().map(|s| s.content.width()).sum();

    let gap = full_w
        .saturating_sub(left_used + right_width)
        .max(if right_width > 0 { HINT_BAR_GAP_MIN } else { 0 });

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(8 + right_spans.len());
    spans.push(Span::styled(" ".repeat(inner), Style::default().bg(bg)));
    spans.extend(zone_spans);
    spans.push(Span::styled(" ".repeat(gap), Style::default().bg(bg)));
    spans.extend(right_spans);
    // Trailing fill so the row owns every cell on this line.
    let used: usize = left_used + gap + right_rendered_width;
    spans.push(Span::styled(
        " ".repeat(full_w.saturating_sub(used)),
        Style::default().bg(bg),
    ));

    frame.render_widget(Paragraph::new(Line::from(spans)), rect);

    // Compute the context-meter segment's screen rect so the caller can make
    // it clickable. Context is the final segment whenever it survives the
    // narrow-width priority pass.
    let mut context_rect: Option<Rect> = None;
    if show_context {
        let right_start = (inner + zone_pill_width + gap) as u16;
        let ctx_offset = (right_width - context_seg_width) as u16;
        let x = rect.x + right_start + ctx_offset;
        context_rect = Some(Rect::new(x, rect.y, context_seg_width as u16, rect.height));
    }
    context_rect
}

/// Abbreviate an absolute path to its `~/...` form so the workspace reads as a
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
                Some(rest) => format!("~/{}", rest.display()),
                // Unreachable: starts_with was just checked.
                None => path.display().to_string(),
            };
        }
    }
    path.display().to_string()
}

/// One queued outbox item projected for the [`QueueBarView`] / queue modal. It
/// is a small owned snapshot of the relevant fields of a
/// [`crate::tui::app::QueuedDispatch`], so the renderers stay decoupled from
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
    /// `true` while the item is an in-flight mid-round steer (`F4` —
    /// [`crate::tui::app::QueuedDispatchState::Inserting`]): handed to the
    /// running round, waiting for admission at the next safe turn boundary.
    /// The bar and the modal mark it with a `steer›` badge so it never reads
    /// as an ordinary next-round entry.
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
    /// `true` while the user has hard-blocked the outbox (`F3`, or the queue
    /// modal being open). While blocked, no message auto-drains even after the
    /// round completes. Surfaced as a distinct `blocked` tag + the legend key
    /// flipping to `F3 resume`, so it never reads as an ordinary pause.
    pub blocked: bool,
}

/// The persistent one-row outbox summary pinned below the transcript gap.
///
/// A brand-colored `QUEUE` label on the plain surface, quietly matching the
/// todo bar above it. The single row carries, left → right: the identity
/// (`QUEUE` + count, plus a `· blocked` state tag while the user holds the
/// outbox), a one-line preview of the next item to pop (a `steer›` badge
/// marks an in-flight mid-round steer), and the right-pinned keycap legend
/// (`F4` insert into the running round, `F3` block/resume, `F2` expand).
///
/// Width pressure degrades the middle and the legend before the identity:
/// the preview truncates with `…`, then the legend sheds its labels and the
/// `F2`/`F4` clusters (keeping `F3`, the state toggle), then the legend
/// drops entirely. An empty queue is never rendered (the layout hides the
/// row), so there is no empty-hint state.
///
/// `paused` recolors the count (warn) so the user can see the queue is held
/// back because the running round has not yet completed, not forgotten. A user
/// `blocked` state (error color + `blocked` tag + the legend's `F3 resume`)
/// is the explicit "send nothing even after the round ends" override.
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
    // Dispatch is FIFO: the front-most item pops first. We preview the first
    // item in dispatch order; an in-flight steer (`Inserting`) leads the deque
    // and wears the `steer›` badge.
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

    // ── Left: `QUEUE N [· blocked]` ─────────────────────────────────────────
    let mut left: Vec<Span<'static>> =
        vec![Span::styled("QUEUE", tag_style), Span::styled(" ", dim)];
    let count_label = if count > 99 {
        "99+".to_string()
    } else {
        count.to_string()
    };
    left.push(Span::styled(count_label, count_style));
    // When blocked, append an explicit `· blocked` tag in the error color so
    // the held-back state never reads as an ordinary pause — the count is
    // already error-colored, and this label spells out why. R1: `blocked` is
    // a state of the queue (JOIN_MODIFY).
    if blocked {
        left.push(Span::styled(JOIN_MODIFY, dim));
        left.push(Span::styled("blocked", count_style));
    }

    // ── Right-side keycap legend ────────────────────────────────────────────
    // The keys explain the three outbox affordances:
    //   F4 — insert the composer text into the running round (mid-round steer)
    //   F3 — block / resume the outbox (toggles; label flips with state)
    //   F2 — expand the full queue list
    // The keycap units are same-rank peers (R2), so they are separated by
    // plain whitespace — no dot. Under width pressure the labels drop first,
    // then the F2 and F4 clusters, keeping `F3` (the state toggle) last.
    let mk_right = |density: LegendDensity| -> Vec<Span<'static>> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let sep = |spans: &mut Vec<Span<'static>>| {
            spans.push(Span::styled(" ".repeat(JOIN_ENUMERATE_COLS), dim));
        };
        if !matches!(density, LegendDensity::Tiny) {
            spans.push(keycap_span(theme, Key::F4.display()));
            if matches!(density, LegendDensity::Full) {
                spans.push(Span::styled(" insert", dim));
            }
            sep(&mut spans);
        }
        spans.push(keycap_span(theme, Key::F3.display()));
        if matches!(density, LegendDensity::Full) {
            spans.push(Span::styled(
                if blocked { " resume" } else { " block" },
                dim,
            ));
        }
        if matches!(density, LegendDensity::Full) {
            sep(&mut spans);
            spans.push(keycap_span(theme, Key::F2.display()));
            spans.push(Span::styled(" expand", dim));
        }
        spans
    };

    // ── Middle: next-item preview ───────────────────────────────────────────
    // One-line, control-chars-collapsed; an in-flight mid-round steer leads
    // with a brand-colored `steer›` badge so it never reads as an ordinary
    // next-round entry. The preview is the most elastic segment: it truncates
    // to whatever the identity and the legend leave behind, and disappears
    // entirely on very narrow rows.
    let preview_text = next.map(|item| {
        let one_line = crate::tui::overlays::common::one_line(item.text.trim());
        if item.steering {
            format!("steer› {one_line}")
        } else {
            one_line
        }
    });

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
                crate::tui::overlays::common::truncate_ellipsis(&text, preview_budget)
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
    /// Keys + labels: `F4 insert  F3 block  F2 expand`.
    Full,
    /// Bare keycaps: `F4  F3  F2`.
    Compact,
    /// Only the block/resume toggle: `F3`.
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

/// Context-window usage indicator: `89.2k (8%)`. The percentage takes the
/// green → yellow → red threshold color so a nearly full window is
/// unmissable; the token count stays muted. `bg` is applied to every span so
/// the indicator reads on a solid background.
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

    vec![
        Span::styled(
            format_token_count(used),
            Style::default().fg(theme.muted()).bg(bg),
        ),
        Span::styled(format!(" ({}%)", pct), Style::default().fg(color).bg(bg)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn activity_row_text(width: u16, status: &str, phase: usize) -> String {
        let mut terminal = neenee_tui_engine::TestTerminal::new(width, 1);
        terminal.draw(|frame| {
            draw_activity_bar(
                frame,
                Rect::new(0, 0, width, 1),
                "",
                None,
                status,
                false,
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

    /// Render the activity bar with `awaiting_permission` set and collect the
    /// foreground color of each cell, so a test can assert the permission state
    /// paints in the warning hue rather than the shimmer palette.
    fn activity_row_colors(width: u16, status: &str, awaiting: bool, phase: usize) -> Vec<Color> {
        let mut terminal = neenee_tui_engine::TestTerminal::new(width, 1);
        terminal.draw(|frame| {
            draw_activity_bar(
                frame,
                Rect::new(0, 0, width, 1),
                "",
                None,
                status,
                awaiting,
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
    fn activity_bar_shimmers_and_always_shows_interrupt_hint() {
        let theme = Theme::default();
        let initial_colors: Vec<Color> = shimmer_spans("Working", 0, &theme)
            .iter()
            .map(|span| span.style.fg)
            .collect();
        let swept_colors: Vec<Color> = shimmer_spans("Working", 8, &theme)
            .iter()
            .map(|span| span.style.fg)
            .collect();

        assert_ne!(initial_colors, swept_colors);
        assert!(swept_colors.iter().any(|color| *color != theme.muted()));

        let row = activity_row_text(80, "Working", 8);
        assert!(row.contains("Working"));
        assert!(row.contains("Esc Esc interrupt"));
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
        let home = std::path::PathBuf::from(
            std::env::var_os("HOME").unwrap_or_else(|| std::ffi::OsString::from("/tmp")),
        );
        let under = home.join("projects").join("xx");
        let rendered = tilde_home(&under);
        assert!(rendered.starts_with("~/"), "got {rendered:?}");
        assert!(rendered.ends_with("projects/xx"), "got {rendered:?}");

        // The home directory itself collapses to a bare `~`.
        assert_eq!(tilde_home(&home), "~");
    }

    fn todo_list_with(item: &str, status: neenee_core::TodoStatus) -> neenee_core::TodoList {
        let mut todos = neenee_core::TodoList::new();
        todos.items.push(neenee_core::TodoItem {
            id: neenee_core::TodoId(1),
            content: item.to_string(),
            status,
            created_at: 0,
            updated_at: 0,
        });
        todos
    }

    fn todo_row_text(todos: &neenee_core::TodoList, width: u16) -> String {
        let mut terminal = neenee_tui_engine::TestTerminal::new(width, 1);
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
    fn todo_bar_leads_with_brand_tag_on_a_plain_surface() {
        // The tag treatment: `TODOS` leads at the gutter in the brand accent
        // on the plain frame surface — no pin glyph, no raised tint — so the
        // row reads as quiet metadata rather than another pinned panel. We
        // assert all of this against the real buffer cells (the substring-only
        // tests can't see color or background).
        let theme = Theme::default();
        let todos = todo_list_with("write the docs", neenee_core::TodoStatus::InProgress);
        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 1);
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
        let todos = todo_list_with("write the docs", neenee_core::TodoStatus::InProgress);
        let text = todo_row_text(&todos, 80);
        assert!(text.contains("TODOS 0/1"), "row was {text:?}");
        assert!(text.contains("write the docs"), "row was {text:?}");
        assert!(text.contains("Ctrl+T expand"), "row was {text:?}");
    }

    #[test]
    fn todo_bar_falls_back_to_first_pending_when_nothing_is_in_progress() {
        let todos = todo_list_with("write the docs", neenee_core::TodoStatus::Pending);
        let text = todo_row_text(&todos, 80);
        assert!(text.contains("TODOS 0/1"), "row was {text:?}");
        // The first Pending item reads as "next up" when nothing is mid-flight.
        assert!(text.contains("write the docs"), "row was {text:?}");
    }

    #[test]
    fn todo_bar_drops_legend_under_width_pressure() {
        let todos = todo_list_with("write the docs", neenee_core::TodoStatus::InProgress);
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
            neenee_core::TodoStatus::InProgress,
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
        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 1);
        terminal.draw(|frame| {
            draw_activity_bar(
                frame,
                Rect::new(0, 0, 80, 1),
                "",
                None,
                "Working",
                false,
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
        let mut terminal = neenee_tui_engine::TestTerminal::new(36, 1);
        terminal.draw(|frame| {
            draw_activity_bar(
                frame,
                Rect::new(0, 0, 36, 1),
                "",
                None,
                "retrying a provider request after a detailed transient failure",
                false,
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
            awaiting.iter().any(|&c| c == theme.warning),
            "awaiting-permission row must use the warning hue"
        );
        // A permission state must not shimmer (the shimmer sweeps the brand hue
        // across phases). The normal row, by contrast, carries brand-derived
        // colors at this phase.
        assert!(
            !awaiting.iter().any(|&c| c == theme.warning) || awaiting != normal,
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

    /// The `@<instance>` suffix is the first hint-bar segment to hide as the
    /// row narrows: provenance is nice-to-have, so it drops while the model
    /// name, reasoning tag, and context meter all still fit.
    #[test]
    fn narrow_hint_bar_hides_the_instance_suffix_first() {
        let messages: Vec<TranscriptMessage> = Vec::new();
        let row_text = |width: u16| -> String {
            let mut terminal = neenee_tui_engine::TestTerminal::new(width, 1);
            terminal.draw(|f| {
                draw_hint_bar(
                    f,
                    Rect::new(0, 0, width, 1),
                    HintBarView {
                        current_model: "kimi-k2.7-code",
                        provider_name: Some("kimi-code"),
                        messages: &messages,
                        reasoning_effort: Some("max"),
                        shell_active: false,
                        busy: false,
                        context_tokens: None,
                        ignition_elapsed_ms: None,
                    },
                    &Theme::default(),
                );
            });
            let buf = terminal.buffer();
            (0..buf.area().width as usize)
                .map(|x| buf.content[x].symbol().to_string())
                .collect::<String>()
        };

        // Wide enough: the full cluster `model effort @instance  ctx` shows.
        let wide = row_text(50);
        assert!(wide.contains("@kimi-code"), "{wide:?}");
        assert!(wide.contains("(0%)"), "{wide:?}");

        // One column narrower and the instance suffix is gone — while the
        // model name, the effort tag, and the context meter all survive.
        let narrow = row_text(49);
        assert!(
            !narrow.contains('@'),
            "instance should hide first: {narrow:?}"
        );
        assert!(narrow.contains("Kimi K2.7 Code"), "{narrow:?}");
        assert!(narrow.contains("max"), "{narrow:?}");
        assert!(narrow.contains("(0%)"), "{narrow:?}");
    }

    #[test]
    fn hint_bar_renders_model_and_context() {
        let theme = Theme::default();
        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 3);
        let messages = vec![TranscriptMessage::new(neenee_core::Role::User, "hi")];
        terminal.draw(|f| {
            draw_hint_bar(
                f,
                Rect::new(0, 2, 80, 1),
                HintBarView {
                    current_model: "mock-model",
                    provider_name: Some("mock-instance"),
                    messages: &messages,
                    reasoning_effort: None,
                    shell_active: false,
                    busy: false,
                    context_tokens: None,
                    ignition_elapsed_ms: None,
                },
                &theme,
            );
        });
    }

    #[test]
    fn hint_bar_describes_the_current_enter_action() {
        fn row_text(terminal: &mut neenee_tui_engine::TestTerminal, shell_active: bool) -> String {
            let mut captured = String::new();
            terminal.draw(|f| {
                let view = HintBarView {
                    current_model: "",
                    provider_name: None,
                    messages: &Vec::<TranscriptMessage>::new(),
                    reasoning_effort: None,
                    shell_active,
                    busy: false,
                    context_tokens: None,
                    ignition_elapsed_ms: None,
                };
                draw_hint_bar(f, Rect::new(0, 0, 80, 1), view, &Theme::default());
            });
            let buf = terminal.buffer();
            let bw = buf.area().width as usize;
            for x in 0..bw {
                let cell = &buf.content[x];
                captured.push_str(cell.symbol());
            }
            captured
        }

        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 1);
        assert!(row_text(&mut terminal, false).contains("Enter send"));
        assert!(row_text(&mut terminal, true).contains("Enter run command"));
    }

    #[test]
    fn hint_bar_enter_keycap_uses_the_unified_brand_color() {
        // The "Enter" keycap on the hint bar must route through the unified
        // keycap style (brand + bold) so it matches every other keycap in the
        // app (activity bar, queue bar, modal footers) instead of a divergent
        // fg tone. The 'E' of "Enter" sits right after the 1-space indent.
        let theme = Theme::default();
        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 1);
        terminal.draw(|frame| {
            draw_hint_bar(
                frame,
                Rect::new(0, 0, 80, 1),
                HintBarView {
                    current_model: "",
                    provider_name: None,
                    messages: &Vec::<TranscriptMessage>::new(),
                    reasoning_effort: None,
                    shell_active: false,
                    busy: false,
                    context_tokens: None,
                    ignition_elapsed_ms: None,
                },
                &theme,
            );
        });
        let cells = terminal.buffer().content.clone();
        // Layout: [indent(1)] [Enter(5)] [ send]. 'E' lands at index 1.
        assert_eq!(cells[1].symbol(), "E", "expected 'Enter' at col 1");
        assert_eq!(
            cells[1].fg(),
            theme.brand(),
            "Enter keycap not brand-colored"
        );
        assert!(
            cells[1].style.add.contains(Modifier::BOLD),
            "Enter keycap not bold"
        );
        // The surface tint must cover the keycap cell.
        assert_eq!(
            cells[1].bg(),
            theme.surface(),
            "Enter keycap not on surface"
        );
    }

    #[test]
    fn hint_bar_busy_shows_queue_action() {
        // When the agent is mid-round, Enter stages the message in the queue
        // (the queue bar below shows the staged item). The queue affordances
        // live in the queue bar, not this sentence.
        let mut terminal = neenee_tui_engine::TestTerminal::new(120, 1);
        terminal.draw(|frame| {
            draw_hint_bar(
                frame,
                Rect::new(0, 0, 120, 1),
                HintBarView {
                    current_model: "mock",
                    provider_name: None,
                    messages: &[],
                    reasoning_effort: None,
                    shell_active: false,
                    busy: true,
                    context_tokens: None,
                    ignition_elapsed_ms: None,
                },
                &Theme::default(),
            );
        });
        let buffer = terminal.buffer();
        let text = (0..buffer.area().width as usize)
            .map(|x| buffer.content[x].symbol().to_string())
            .collect::<String>();
        assert!(text.contains("Enter queue message"), "row was {text:?}");
        // Queue affordances live in the queue bar, not the hint bar.
        assert!(!text.contains("waiting"));
        assert!(!text.contains("edit latest"));
        assert!(!text.contains("Tab"));
    }

    #[test]
    fn narrow_hint_bar_preserves_the_enter_action_before_metadata() {
        let mut terminal = neenee_tui_engine::TestTerminal::new(36, 1);
        terminal.draw(|frame| {
            draw_hint_bar(
                frame,
                Rect::new(0, 0, 36, 1),
                HintBarView {
                    current_model: "mock",
                    provider_name: None,
                    messages: &[],
                    reasoning_effort: Some("high"),
                    shell_active: false,
                    busy: true,
                    context_tokens: None,
                    ignition_elapsed_ms: None,
                },
                &Theme::default(),
            );
        });
        let text = terminal
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        // The shorter busy action ("queue message" vs the old insert/next-round
        // sentence) leaves room for the reasoning tag at this width.
        assert!(text.contains("Enter queue"), "row was {text:?}");
        // Queue counts no longer live in the hint bar.
        assert!(!text.contains("waiting"), "row was {text:?}");
    }

    #[test]
    fn hint_bar_reasoning_tag_shows_effort_when_set() {
        // Render the full hint row for three effort states and read back the
        // whole line: the bare `{effort}` tag must appear right after the
        // model name when reasoning is in use and be absent entirely
        // otherwise.
        fn row_text(effort: Option<&str>) -> String {
            let mut terminal = neenee_tui_engine::TestTerminal::new(80, 1);
            terminal.draw(|f| {
                draw_hint_bar(
                    f,
                    Rect::new(0, 0, 80, 1),
                    HintBarView {
                        current_model: "mock",
                        provider_name: None,
                        messages: &Vec::<TranscriptMessage>::new(),
                        reasoning_effort: effort,
                        shell_active: false,
                        busy: false,
                        context_tokens: None,
                        ignition_elapsed_ms: None,
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
    fn hint_bar_shows_the_instance_suffix_after_the_model_name() {
        // The `@<instance>` suffix must trail the model name so identical
        // models served by different instances stay attributable — and must
        // vanish entirely when no instance is known.
        fn row_text(provider_name: Option<&str>) -> String {
            let mut terminal = neenee_tui_engine::TestTerminal::new(80, 1);
            terminal.draw(|f| {
                draw_hint_bar(
                    f,
                    Rect::new(0, 0, 80, 1),
                    HintBarView {
                        current_model: "mock",
                        provider_name,
                        messages: &Vec::<TranscriptMessage>::new(),
                        reasoning_effort: None,
                        shell_active: false,
                        busy: false,
                        context_tokens: None,
                        ignition_elapsed_ms: None,
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
        // The suffix is the last cluster segment before the context meter:
        // `Model effort @instance`.
        let model_pos = named.find("mock").expect("model name on the row");
        let inst_pos = named.find("@kimi-code").expect("instance suffix");
        assert!(model_pos < inst_pos, "instance must follow the model name");
        // Unknown / empty instance → no `@` anywhere on the row.
        assert!(!row_text(None).contains('@'));
        assert!(!row_text(Some("")).contains('@'));
    }

    #[test]
    fn hint_bar_full_cluster_orders_model_effort_instance() {
        // The right cluster reads `Kimi K3 max @kimi-code  89.2k (8%)` —
        // effort tight after the model name, the @instance provenance last.
        // The identity group (`model effort @instance`) joins with single
        // spaces; only the context segment sits across the wider gap.
        let mut terminal = neenee_tui_engine::TestTerminal::new(120, 1);
        terminal.draw(|f| {
            draw_hint_bar(
                f,
                Rect::new(0, 0, 120, 1),
                HintBarView {
                    current_model: "mock",
                    provider_name: Some("kimi-code"),
                    messages: &Vec::<TranscriptMessage>::new(),
                    reasoning_effort: Some("max"),
                    shell_active: false,
                    busy: false,
                    context_tokens: None,
                    ignition_elapsed_ms: None,
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
    fn hint_bar_ignition_label_takes_over_the_identity_cluster() {
        // During the ignition's label phase the right cluster swaps the whole
        // `model effort @instance  ctx` identity for the converging `M A X`
        // label; once the phase ends the normal cluster returns.
        fn row_text(elapsed_ms: Option<u128>) -> String {
            let mut terminal = neenee_tui_engine::TestTerminal::new(100, 1);
            terminal.draw(|f| {
                draw_hint_bar(
                    f,
                    Rect::new(0, 0, 100, 1),
                    HintBarView {
                        current_model: "k3",
                        provider_name: Some("kimi-code"),
                        messages: &Vec::<TranscriptMessage>::new(),
                        reasoning_effort: Some("max"),
                        shell_active: false,
                        busy: false,
                        context_tokens: Some(12_400),
                        ignition_elapsed_ms: elapsed_ms,
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
        assert!(plain.contains("Kimi K3"), "model name renders: {plain:?}");
    }

    /// Paint the completion menu into a test buffer and return the rect the
    /// popup actually occupied (found by scanning for the popup background),
    /// so assertions can check alignment and full-width highlighting without
    /// duplicating the layout math.
    fn paint_completion_menu(
        input_anchor_x: u16,
        selected: Option<usize>,
    ) -> (neenee_tui_engine::TestTerminal, Rect) {
        let theme = Theme::default();
        let completions = vec![
            crate::tui::completion::Completion {
                label: "/repeat".to_string(),
                description: "Schedule a prompt on a cron".to_string(),
                replace_start: 0,
                replace_end: 2,
                kind: crate::tui::completion::CompletionItemKind::Slash,
            },
            crate::tui::completion::Completion {
                label: "/permissions".to_string(),
                description: "Manage permissions".to_string(),
                replace_start: 0,
                replace_end: 2,
                kind: crate::tui::completion::CompletionItemKind::Slash,
            },
        ];
        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 12);
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
        // The band spans further than the row's text: the longest candidate
        // (`/permissions  Manage permissions`) ends well before the popup
        // edge, and the highlight must still cover the fill.
        assert!(
            last - first >= 30,
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

    /// A menu whose longest description would stretch to the viewport edge
    /// must stay compact instead: the description truncates with an ellipsis
    /// and the popup's leading edge keeps the anchor column (this is the
    /// bare-`/` slash menu case — the full command table at 80 columns).
    #[test]
    fn completion_menu_caps_width_and_truncates_long_descriptions() {
        let theme = Theme::default();
        let completions = [
            ("/models", "Switch the active model"),
            ("/tools", "Manage session tools (enable/disable)"),
            (
                "/autopilot",
                "Toggle autopilot mode — agent runs without human intervention (on/off)",
            ),
        ]
        .iter()
        .map(|(l, d)| crate::tui::completion::Completion {
            label: l.to_string(),
            description: d.to_string(),
            replace_start: 0,
            replace_end: 1,
            kind: crate::tui::completion::CompletionItemKind::Slash,
        })
        .collect::<Vec<_>>();
        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 12);
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
        // The truncated description ends with an ellipsis.
        let row_text: String = (first..=last)
            .filter_map(|x| buf.get(x, y).map(|c| c.symbol().to_string()))
            .collect();
        assert!(row_text.trim_end().ends_with('…'), "row was {row_text:?}");
        assert!(row_text.starts_with("/autopilot"), "row was {row_text:?}");
    }

    #[test]
    fn completion_menu_has_no_dot_separator_and_two_space_gap() {
        let (terminal, popup) = paint_completion_menu(2, None);
        let buf = terminal.buffer();
        let row_text = |y: u16| -> String {
            (0..buf.area().width)
                .filter_map(|x| buf.get(x, y).map(|c| c.symbol().to_string()))
                .collect()
        };
        let first = row_text(popup.y);
        // Label and description sit in plain padded columns — no `·` (or any
        // other ornament) between them; weight/brightness carry the hierarchy.
        assert!(first.contains("/repeat"), "row was {first:?}");
        assert!(
            first.contains("Schedule a prompt on a cron"),
            "row was {first:?}"
        );
        assert!(!first.contains('·'), "row was {first:?}");
    }

    /// Read back the one-row bar as joined text for assertion.
    fn queue_row_text(view: QueueBarView<'_>, width: u16, theme: &Theme) -> String {
        let mut terminal = neenee_tui_engine::TestTerminal::new(width, 1);
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
        let mut terminal = neenee_tui_engine::TestTerminal::new(70, 1);
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
            70,
            &Theme::default(),
        );
        // Identity + count reflects the one item; no time label anymore.
        assert!(text.contains("QUEUE 1"), "row was {text:?}");
        assert!(!text.contains(":"), "time label leaked: {text:?}");
        // Legend: the three keycap units are same-rank peers (R2) — joined by
        // plain whitespace, never a `·` (which would imply one modifies the
        // other).
        assert!(
            text.contains("F4 insert  F3 block  F2 expand"),
            "peer keycaps must use R2 whitespace: {text:?}"
        );
        assert!(!text.contains('·'), "no R1 dot between peers: {text:?}");
        // A non-steering item previews plainly — no `steer›` badge (that marks
        // an in-flight mid-round steer). The legend's `F4 insert` affordance
        // is always present and is unrelated to the badge.
        assert!(!text.contains("steer›"), "steer badge leaked: {text:?}");
        // The preview rides inline on the same row.
        assert!(
            text.contains("fix the flaky test"),
            "preview text missing: {text:?}"
        );
    }

    #[test]
    fn queue_bar_marks_an_in_flight_steer_with_a_badge() {
        // An `F4` steer already handed to the running round must never read
        // as an ordinary next-round entry: it leads with the `steer›` badge.
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
            70,
            &Theme::default(),
        );
        assert!(text.contains("steer›"), "steer badge missing: {text:?}");
        // The badge borrows the preview budget, so the text truncates — but
        // the head of the message must survive the ellipsis.
        assert!(
            text.contains("also cover the ed…"),
            "steer preview missing: {text:?}"
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
