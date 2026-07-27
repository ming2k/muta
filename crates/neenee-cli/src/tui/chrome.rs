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
use super::components::keycap::keycap_span;
use super::design::{HINT_BAR_GAP_MIN, HINT_BAR_INNER_PADDING, HINT_BAR_SEGMENT_GAP};
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

/// Draw the transient activity bar that sits directly above the todo bar.
/// Replaces the old inline `┃ neenee ⟳ <status>` indicator: the brand prefix
/// is dropped (the header already shows it) and the static `⟳` glyph is
/// replaced by a breathing-dot indicator so the harness never looks frozen.
///
/// Layout:
/// ```text
/// <spinner> <status> (<elapsed> · Esc Esc to interrupt) [· » <pursuit>] [⚠ <alert>]
/// ```
/// The whole bar is transient (turn-scoped): it shows only while a round is
/// active and is hidden while idle, so the row returns to the transcript.
/// Session-state flags such as `unattended` deliberately do not live here:
/// they fold onto the hint bar's right cluster ([`draw_hint_bar`]) so this row
/// stays a pure activity surface. The persistent task-list summary now lives
/// on its own [`draw_todo_bar`] below this row.
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
        .map(|value| {
            UnicodeWidthStr::width(format!(" ({value} · Esc Esc to interrupt)").as_str())
        })
        .unwrap_or_else(|| UnicodeWidthStr::width(" (Esc Esc to interrupt)"));
    let interrupt_hint_width = UnicodeWidthStr::width(" (Esc Esc to interrupt)");
    let tiny_interrupt_hint_width = UnicodeWidthStr::width(" Esc Esc");
    let prefix_width = UnicodeWidthStr::width(" ● ");
    const MIN_STATUS_WIDTH: usize = 4;
    const MIN_TINY_STATUS_WIDTH: usize = 1;
    let show_elapsed = elapsed.is_some()
        && available_width >= prefix_width + full_hint_width + MIN_STATUS_WIDTH;
    let show_interrupt_words = show_elapsed
        || available_width >= prefix_width + interrupt_hint_width + MIN_STATUS_WIDTH;
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
    spans.push(Span::raw(" "));
    spans.extend(shimmer_spans(&status, spinner_phase, theme));

    // Keep the interrupt instruction immediately after the live status,
    // matching the place users look while waiting. Elapsed time is useful
    // context, but it drops before the key hint on narrow terminals.
    if show_interrupt_words {
        spans.push(Span::styled(" (", dim));
        if show_elapsed {
            spans.push(Span::styled(elapsed.unwrap_or_default(), dim));
            spans.push(Span::styled(" · ", dim));
        }
        spans.push(keycap_span(theme, Key::ESC.display()));
        spans.push(Span::styled(" ", dim));
        spans.push(keycap_span(theme, Key::ESC.display()));
        spans.push(Span::styled(" to interrupt)", dim));
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

/// The one-row todo summary pinned directly below the activity bar (and above
/// the queue bar). It is the permanent home for task-list affordances: a fixed
/// `todo` tag, the done/total progress, and a one-line preview of the current
/// item — the `InProgress` one, or the first `Pending` when nothing is
/// mid-flight (so the bar always points at "what is happening / what is next").
///
/// Layout:
/// ```text
/// todo · d/t · {current item preview…}        Ctrl+T expand
/// ```
/// The right-pinned `Ctrl+T expand` legend is the keyboard affordance that
/// opens the Activity modal on the Todos section; it drops under width
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

    let bg = theme.surface();
    let full_w = rect.width as usize;
    let dim = Style::default().fg(theme.muted()).bg(bg);
    let fg = Style::default().fg(theme.fg()).bg(bg);
    let bold = Style::default()
        .fg(theme.fg())
        .bg(bg)
        .add_modifier(Modifier::BOLD);

    let done = todos.count(TodoStatus::Completed);
    let total = todos.items.len();
    let progress = format!("{done}/{total}");

    // Current item: the InProgress one, else the first Pending (next up).
    let current: Option<&TodoItem> = todos
        .items
        .iter()
        .find(|i| i.status == TodoStatus::InProgress)
        .or_else(|| todos.items.iter().find(|i| i.status == TodoStatus::Pending));

    // ── Left identity: `todo · d/t` ──
    let mut left: Vec<Span<'static>> = Vec::new();
    left.push(Span::styled("todo", bold));
    left.push(Span::styled(" · ", dim));
    left.push(Span::styled(progress, bold));
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
    // leads the preview (only when there is one) plus the inter-cluster gap.
    let content_sep = UnicodeWidthStr::width(" · ");
    let gap_for = |legend_w: usize| if legend_w > 0 { HINT_BAR_GAP_MIN } else { 0 };
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
        row.push(Span::styled(" · ", dim));
        row.push(Span::styled(preview, fg));
        let pad = full_w
            .saturating_sub(left_w + content_sep + preview_w + gap + legend_w);
        row.push(Span::styled(" ".repeat(pad), dim));
    } else {
        // No current item (e.g. everything terminal just before auto-clear):
        // right-pin the legend directly.
        let pad = full_w.saturating_sub(left_w + gap + legend_w);
        row.push(Span::styled(" ".repeat(pad), dim));
    }

    row.extend(legend);

    // Paint a trailing fill so the surface background covers the full width.
    let used_total: usize = row.iter().map(|s| s.content.width()).sum();
    row.push(Span::styled(
        " ".repeat(full_w.saturating_sub(used_total)),
        dim,
    ));

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
    // menu's longest description (e.g. /unattended's) would otherwise fill
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
/// the old top header showed, now collapsed onto one row.
pub struct HintBarView<'a> {
    pub current_model: &'a str,
    #[allow(dead_code)]
    pub messages: &'a [TranscriptMessage],
    /// Effective reasoning effort of the active model, shown as a `◆ {effort}`
    /// tag right after the model name — only when reasoning is actually in use
    /// for this model. The caller resolves the value and applies the
    /// per-protocol gating (Anthropic: shown only when thinking is opted in;
    /// OpenAI: shown whenever the model exposes an effort knob; Gemini:
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
    /// Session-state flag folded onto the row (replaces the old dedicated state
    /// bar): `unattended` shows as a right-aligned warning tag so the row the
    /// user already reads also carries the one ambient flag.
    pub unattended: bool,
    /// Session-scoped size of the AI-visible request context. Produced by the
    /// harness from provider API usage when available, otherwise from the
    /// projected `model_window`; it is deliberately unrelated to durable or
    /// rendered transcript size. `None` is shown as zero until the first
    /// projection snapshot arrives.
    pub context_tokens: Option<usize>,
}

#[derive(Clone, Copy)]
enum ActionDensity {
    Full,
    Compact,
    Tiny,
}

/// Build the left side of the bottom row as a short action sentence. The
/// detailed insert/next-round wording and the Tab alternative used to live
/// here; the persistent queue bar now carries those affordances (with a
/// keycap legend), so this stays a pure "what will Enter do" surface: send
/// when idle, queue when the agent is mid-round, or run a shell command.
fn input_action_spans(
    shell_active: bool,
    busy: bool,
    density: ActionDensity,
    theme: &Theme,
    bg: Color,
) -> Vec<Span<'static>> {
    let key_style = Style::default()
        .fg(theme.fg())
        .bg(bg)
        .add_modifier(Modifier::BOLD);
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
        // queue bar below shows the staged item). The Tab toggle for insert vs
        // next-round, and the recall/edit affordance, live in the queue bar's
        // keycap legend rather than this sentence.
        spans.push(Span::styled(
            if compact { " queue" } else { " queue message" },
            hint_style,
        ));
    } else {
        spans.push(Span::styled(" send", hint_style));
    }

    spans
}

/// Draw the single-line hint bar pinned below the input box. Carries the model
/// name and context-usage info that the old top header showed, plus the action
/// performed by the next Enter, now collapsed onto one row so the transcript
/// reclaims vertical space.
///
/// Layout: current input action on the left, right-aligned cluster of
/// `model · context-usage` on the right. On narrow terminals, the action
/// sentence compacts first and ambient model metadata drops before the action.
pub fn draw_hint_bar(
    frame: &mut Frame,
    rect: Rect,
    view: HintBarView<'_>,
    theme: &Theme,
) -> Option<Rect> {
    let HintBarView {
        current_model,
        messages: _,
        reasoning_effort,
        shell_active,
        busy,
        unattended,
        context_tokens,
    } = view;

    let bg = theme.surface();
    let full_w = rect.width as usize;

    // --- Left cluster: one sentence describing what the next Enter does.
    // Keep product language here: users should not need to learn the internal
    // round/turn distinction or decode transport arrows before sending. The
    // detailed insert/next-round wording, the Tab toggle, and the recall
    // affordance all live in the persistent queue bar now, not here.
    let mut action_density = ActionDensity::Full;
    let mut zone_spans = input_action_spans(shell_active, busy, action_density, theme, bg);
    let mut zone_pill_width = zone_spans.iter().map(|s| s.content.width()).sum::<usize>();

    // --- Right cluster: model name and context bar.
    // Build each segment separately so we can drop optional ones when the
    // terminal is too narrow.
    let context_max = crate::tui::providers::model_context_window(current_model);

    let inner = HINT_BAR_INNER_PADDING;

    // Build right-side segments independently. Model identity is the last
    // ambient item to drop; reasoning effort drops first, then context usage.
    // The input action always wins when the row cannot hold both clusters.
    let model_label = crate::tui::providers::model_display_name(current_model);
    let model_width = model_label.width();
    let model_spans = vec![Span::styled(
        model_label,
        Style::default()
            .fg(theme.brand())
            .add_modifier(Modifier::BOLD)
            .bg(bg),
    )];

    // Reasoning-effort tag: `◆ high`. Optional — only present when the active
    // model is actually reasoning (caller-resolved and protocol-gated). Sits
    // right after the model name so it reads as an attribute of the model —
    // "Claude Opus 4.8  ◆ high  12k (1%)" — mirroring the `◆` glyph the
    // `/models` picker uses for reasoning models. The context meter stays
    // the last segment of the cluster, so the click-target rect math below is
    // unaffected.
    let mut reasoning_spans: Vec<Span<'static>> = Vec::new();
    if let Some(effort) = reasoning_effort {
        // `◆ {effort}` — the diamond marks a reasoning model, mirroring the
        // glyph the `/models` picker uses.
        reasoning_spans.extend([
            Span::styled("◆", Style::default().fg(theme.info()).bg(bg)),
            Span::styled(" ", Style::default().bg(bg)),
            Span::styled(
                effort.to_string(),
                Style::default()
                    .fg(theme.info())
                    .add_modifier(Modifier::BOLD)
                    .bg(bg),
            ),
        ]);
    }
    let reasoning_width = reasoning_spans
        .iter()
        .map(|span| span.content.width())
        .sum::<usize>();

    // Session-state tag: `unattended`. Folded onto this row from the old
    // dedicated state bar. It is a safety flag, so it leads the right cluster
    // (after the model name) and is the last ambient item to drop under width
    // pressure — a warning tone so it reads as "on" at a glance.
    let unattended_spans: Vec<Span<'static>> = if unattended {
        vec![Span::styled(
            "unattended",
            Style::default()
                .fg(theme.warn())
                .add_modifier(Modifier::BOLD)
                .bg(bg),
        )]
    } else {
        Vec::new()
    };
    let unattended_width = unattended_spans
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
    let mut show_unattended = unattended_width > 0;
    let mut show_context = context_seg_width > 0;
    let right_width_for = |model: bool, reasoning: bool, unattended: bool, context: bool| {
        let segment_count = usize::from(model)
            + usize::from(reasoning)
            + usize::from(unattended)
            + usize::from(context);
        usize::from(model) * model_width
            + usize::from(reasoning) * reasoning_width
            + usize::from(unattended) * unattended_width
            + usize::from(context) * context_seg_width
            + segment_count.saturating_sub(1) * HINT_BAR_SEGMENT_GAP
    };
    let fits = |left_width: usize, right_width: usize| {
        inner + left_width + if right_width > 0 { HINT_BAR_GAP_MIN } else { 0 } + right_width
            <= full_w
    };

    let mut right_width =
        right_width_for(show_model, show_reasoning, show_unattended, show_context);
    if !fits(zone_pill_width, right_width) {
        action_density = ActionDensity::Compact;
        zone_spans = input_action_spans(shell_active, busy, action_density, theme, bg);
        zone_pill_width = zone_spans.iter().map(|s| s.content.width()).sum::<usize>();
    }
    // Drop order under width pressure: reasoning first, then context, then the
    // unattended safety flag, then the model name. The action on the left
    // always wins last.
    if !fits(zone_pill_width, right_width) && show_reasoning {
        show_reasoning = false;
        right_width = right_width_for(show_model, show_reasoning, show_unattended, show_context);
    }
    if !fits(zone_pill_width, right_width) && show_context {
        show_context = false;
        right_width = right_width_for(show_model, show_reasoning, show_unattended, show_context);
    }
    if !fits(zone_pill_width, right_width) {
        action_density = ActionDensity::Tiny;
        zone_spans = input_action_spans(shell_active, busy, action_density, theme, bg);
        zone_pill_width = zone_spans.iter().map(|s| s.content.width()).sum::<usize>();
    }
    if !fits(zone_pill_width, right_width) && show_unattended {
        show_unattended = false;
        right_width = right_width_for(show_model, show_reasoning, show_unattended, show_context);
    }
    if !fits(zone_pill_width, right_width) && show_model {
        show_model = false;
        right_width = right_width_for(show_model, show_reasoning, show_unattended, show_context);
    }

    let mut right_spans: Vec<Span<'static>> = Vec::new();
    let separator = || Span::styled(" ".repeat(HINT_BAR_SEGMENT_GAP), Style::default().bg(bg));
    for segment in [
        show_model.then_some(model_spans),
        show_reasoning.then_some(reasoning_spans),
        show_unattended.then_some(unattended_spans),
        show_context.then_some(context_spans),
    ]
    .into_iter()
    .flatten()
    {
        if !right_spans.is_empty() {
            right_spans.push(separator());
        }
        right_spans.extend(segment);
    }

    let left_used = inner + zone_pill_width;

    let gap = full_w
        .saturating_sub(left_used + right_width)
        .max(if right_width > 0 { HINT_BAR_GAP_MIN } else { 0 });

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(8 + right_spans.len());
    spans.push(Span::styled(" ".repeat(inner), Style::default().bg(bg)));
    spans.extend(zone_spans);
    spans.push(Span::styled(" ".repeat(gap), Style::default().bg(bg)));
    spans.extend(right_spans);
    // Trailing fill so the row owns every cell on this line.
    let used = left_used + gap + right_width;
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

/// One queued outbox item projected for the [`QueueBarView`] / queue modal. It
/// is a small owned snapshot of the relevant fields of a
/// [`crate::tui::app::QueuedDispatch`], so the renderers stay decoupled from
/// the full dispatch state machine (images, pastes, lifecycle states) and
/// borrowing the outbox does not entangle the view layer with the app's
/// mutable state.
#[derive(Clone)]
pub struct QueueItemView {
    /// What the next Enter does with this item: insert at the next safe
    /// boundary, or wait for a fresh round. Surfaced as a modifier glyph.
    pub target: crate::tui::app::SendTarget,
    /// When the item was queued (epoch ms), rendered as a local `HH:MM`.
    pub queued_at_ms: u64,
    /// The user's literal prompt text (the first run is previewed in the bar).
    pub text: String,
}

/// Inputs for [`draw_queue_bar`]: the persistent two-row outbox summary pinned
/// below the transcript gap. This is the permanent home for queue affordances.
pub struct QueueBarView<'a> {
    /// Outbox items for the viewed session, in dispatch order (front pops
    /// first). May be empty — the bar then renders a muted empty state.
    pub items: &'a [QueueItemView],
    /// `true` while next-round items are held back because the running round
    /// has not yet naturally completed. Recolors the count so the user can see
    /// the queue is paused, not forgotten.
    pub paused: bool,
}

/// The persistent two-row outbox summary pinned below the transcript gap.
///
/// Layout:
/// ```text
/// queue · N · HH:MM        esc insert · tab next round · F2 expand
/// {modifier} {preview…}
/// ```
/// - Row 1 carries the identity (a fixed `queue` tag, the total count, the
///   send time of the *next item to pop*) on the left and a compact keycap
///   legend on the right (what Enter/Tab do, and how to expand the full
///   list). The legend is what the user used to infer from the hint bar's
///   embedded counts.
/// - Row 2 previews the next item to pop: a modifier glyph marking whether it
///   inserts at the next boundary (`→`) or starts a fresh round (`⇥`), then as
///   many characters of its text as the width allows (truncated with `…`).
///
/// Returns the full bar rect so the event loop can make the region clickable
/// (click → expand the Queue modal).
pub fn draw_queue_bar(
    frame: &mut Frame,
    rect: Rect,
    view: QueueBarView<'_>,
    theme: &Theme,
) -> Rect {
    let QueueBarView { items, paused } = view;

    let bg = theme.surface();
    let full_w = rect.width as usize;
    // The bar spans two rows; reserve them up front.
    let row_height = 1u16;

    // ── Resolve the next item to pop ────────────────────────────────────────
    // Dispatch order: Insert items ship as soon as the harness admits them
    // (they were forwarded at enqueue time), so the *next to pop* is the
    // front-most Waiting item of either kind — matches `recall_queued`'s LIFO
    // undo view of "the newest staged" only for editing; for *popping* the
    // front wins. We show the first item in dispatch order.
    let next = items.first().cloned();

    let count = items.len();
    let dim = Style::default().fg(theme.muted()).bg(bg);
    let fg = Style::default().fg(theme.fg()).bg(bg);
    let count_color = if paused { theme.warn() } else { theme.fg() };
    let count_style = Style::default()
        .fg(count_color)
        .bg(bg)
        .add_modifier(Modifier::BOLD);
    let key_style = Style::default()
        .fg(theme.fg())
        .bg(bg)
        .add_modifier(Modifier::BOLD);

    // ── Row 1: `queue · N · HH:MM`  …  `esc insert · tab next · F2 expand` ──
    let mut left1: Vec<Span<'static>> = Vec::new();
    left1.push(Span::styled("queue", key_style));
    left1.push(Span::styled(" · ", dim));
    let count_label = if count > 99 {
        "99+".to_string()
    } else {
        count.to_string()
    };
    left1.push(Span::styled(count_label, count_style));
    left1.push(Span::styled(" · ", dim));
    let time_label = next
        .as_ref()
        .map(|item| crate::tui::time::sent_time_label(item.queued_at_ms))
        .unwrap_or_else(|| "--:--".to_string());
    left1.push(Span::styled(time_label, dim));

    // Right-side keycap legend. The keys explain the three outbox affordances
    // the old hint-bar used to embed as prose:
    //   Esc   — recall/dispatch the newest staged message immediately
    //   Tab   — flip the next busy Enter between insert and next-round
    //   F2    — expand the full queue list
    // The right cluster drops under width pressure (F2 first, then Tab, then
    // Esc), so the identity on the left always survives.
    let mk_right = |density: LegendDensity| -> Vec<Span<'static>> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let sep = |spans: &mut Vec<Span<'static>>| {
            spans.push(Span::styled(" · ", dim));
        };
        spans.push(keycap_span(theme, Key::ESC.display()));
        if matches!(density, LegendDensity::Full) {
            spans.push(Span::styled(" insert now", dim));
        }
        if !matches!(density, LegendDensity::Tiny) {
            sep(&mut spans);
            spans.push(keycap_span(theme, Key::TAB.display()));
            if matches!(density, LegendDensity::Full) {
                spans.push(Span::styled(" next round", dim));
            }
        }
        if !matches!(density, LegendDensity::Tiny) {
            sep(&mut spans);
            spans.push(keycap_span(theme, Key::F2.display()));
            if matches!(density, LegendDensity::Full) {
                spans.push(Span::styled(" expand", dim));
            }
        }
        spans
    };

    let left1_w: usize = left1.iter().map(|s| s.content.width()).sum();
    let fits = |left: usize, right: &[Span<'static>]| {
        let rw: usize = right.iter().map(|s| s.content.width()).sum();
        left + if rw > 0 { HINT_BAR_GAP_MIN } else { 0 } + rw <= full_w
    };
    let mut legend_density = LegendDensity::Full;
    let mut right1 = mk_right(legend_density);
    if !fits(left1_w, &right1) {
        legend_density = LegendDensity::Compact;
        right1 = mk_right(legend_density);
    }
    if !fits(left1_w, &right1) {
        legend_density = LegendDensity::Tiny;
        right1 = mk_right(legend_density);
    }
    if !fits(left1_w, &right1) && legend_density == LegendDensity::Tiny {
        // Still too tight: drop the legend entirely and keep the identity.
        right1.clear();
    }
    let right1_w: usize = right1.iter().map(|s| s.content.width()).sum();
    let gap1 = full_w
        .saturating_sub(left1_w + right1_w)
        .max(if right1_w > 0 { HINT_BAR_GAP_MIN } else { 0 });

    let mut row1: Vec<Span<'static>> = Vec::with_capacity(2 + left1.len() + right1.len());
    row1.extend(left1);
    row1.push(Span::styled(" ".repeat(gap1), dim));
    row1.extend(right1);
    let used1 = left1_w + gap1 + right1_w;
    row1.push(Span::styled(" ".repeat(full_w.saturating_sub(used1)), dim));

    let row1_rect = Rect::new(rect.x, rect.y, rect.width, row_height);
    frame.render_widget(Paragraph::new(Line::from(row1)), row1_rect);

    // ── Row 2: `{preview…}` with a right-pinned target badge ────────────────
    // The arrow modifier glyphs were dropped; the send-target state is now a
    // short coloured badge pinned to the right (`insert`/`next`), matching the
    // tools modal's `[on]`/`[off]` pattern. Empty queue → muted hint.
    let mut row2: Vec<Span<'static>> = Vec::new();
    if let Some(item) = next.as_ref() {
        let (badge, badge_color) = match item.target {
            crate::tui::app::SendTarget::Insert => ("insert", theme.ok()),
            crate::tui::app::SendTarget::NextRound => ("next", theme.info()),
        };
        let badge_w = badge.width();
        // Reserve a 2-col gap before the right-pinned badge.
        let preview_budget = full_w.saturating_sub(badge_w + 2);
        // One-line, control-chars-collapsed preview; truncated to the budget
        // with an ellipsis so a multi-line paste never wraps the bar.
        let one_line = crate::tui::overlays::common::one_line(item.text.trim());
        let preview = if one_line.width() > preview_budget {
            crate::tui::overlays::common::truncate_ellipsis(&one_line, preview_budget)
        } else {
            one_line
        };
        let pad = full_w.saturating_sub(preview.width() + 2 + badge_w);
        row2.push(Span::styled(preview, fg));
        row2.push(Span::styled(" ".repeat(pad), dim));
        row2.push(Span::styled("  ", dim));
        row2.push(Span::styled(
            badge.to_string(),
            Style::default().fg(badge_color).bg(bg),
        ));
    } else {
        let hint = "queue empty — press Enter while the agent runs to stage a message";
        let hint_budget = full_w;
        let hint_text = if hint.width() > hint_budget {
            crate::tui::overlays::common::truncate_ellipsis(hint, hint_budget)
        } else {
            hint.to_string()
        };
        let pad = full_w.saturating_sub(hint_text.width());
        row2.push(Span::styled(hint_text, dim));
        row2.push(Span::styled(" ".repeat(pad), dim));
    }

    let row2_rect = Rect::new(rect.x, rect.y + row_height, rect.width, row_height);
    frame.render_widget(Paragraph::new(Line::from(row2)), row2_rect);

    rect
}

/// How much of the row-1 keycap legend survives under width pressure.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LegendDensity {
    /// Keys + labels: `Esc insert now · Tab next round · F2 expand`.
    Full,
    /// Keys only: `Esc · Tab · F2`.
    Compact,
    /// Only the dispatch key: `Esc`.
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
        assert!(row.contains("Esc Esc to interrupt"));
    }

    #[test]
    fn activity_bar_preserves_interrupt_hint_at_minimum_width() {
        let row = activity_row_text(
            36,
            "retrying a provider request after a very detailed transient failure",
            8,
        );
        assert!(row.contains("Esc Esc to interrupt"), "row was {row:?}");
        assert!(row.contains('…'), "long status was not truncated: {row:?}");
    }

    #[test]
    fn hint_bar_folds_the_unattended_flag_onto_the_right_cluster() {
        // The unattended flag used to have its own state row; it now folds onto
        // the hint bar's right cluster as a warning-toned tag.
        let mut terminal = neenee_tui_engine::TestTerminal::new(120, 1);
        terminal.draw(|frame| {
            draw_hint_bar(
                frame,
                Rect::new(0, 0, 120, 1),
                HintBarView {
                    current_model: "mock",
                    messages: &[],
                    reasoning_effort: None,
                    shell_active: false,
                    busy: false,
                    unattended: true,
                    context_tokens: None,
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
        assert!(text.contains("unattended"), "flag missing: {text:?}");
        // Without the flag the row must not show the tag.
        let mut terminal2 = neenee_tui_engine::TestTerminal::new(120, 1);
        terminal2.draw(|frame| {
            draw_hint_bar(
                frame,
                Rect::new(0, 0, 120, 1),
                HintBarView {
                    current_model: "mock",
                    messages: &[],
                    reasoning_effort: None,
                    shell_active: false,
                    busy: false,
                    unattended: false,
                    context_tokens: None,
                },
                &Theme::default(),
            );
        });
        let text2 = terminal2
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(!text2.contains("unattended"), "flag leaked: {text2:?}");
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
    fn todo_bar_shows_tag_progress_current_item_and_legend() {
        // InProgress item is the surfaced "current" content.
        let todos = todo_list_with("write the docs", neenee_core::TodoStatus::InProgress);
        let text = todo_row_text(&todos, 80);
        assert!(text.contains("todo · 0/1"), "row was {text:?}");
        assert!(text.contains("write the docs"), "row was {text:?}");
        assert!(text.contains("Ctrl+T expand"), "row was {text:?}");
    }

    #[test]
    fn todo_bar_falls_back_to_first_pending_when_nothing_is_in_progress() {
        let todos = todo_list_with("write the docs", neenee_core::TodoStatus::Pending);
        let text = todo_row_text(&todos, 80);
        assert!(text.contains("todo · 0/1"), "row was {text:?}");
        // The first Pending item reads as "next up" when nothing is mid-flight.
        assert!(text.contains("write the docs"), "row was {text:?}");
    }

    #[test]
    fn todo_bar_drops_legend_under_width_pressure() {
        let todos = todo_list_with("write the docs", neenee_core::TodoStatus::InProgress);
        // At 20 cols the `expand` label cannot fit alongside the preview.
        let text = todo_row_text(&todos, 20);
        assert!(text.contains("todo · 0/1"), "row was {text:?}");
        assert!(!text.contains("expand"), "legend leaked: {text:?}");
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
        assert!(!text.contains("unattended"), "row was {text:?}");
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

    /// The hint bar must render the model and context bar on a single line
    /// below the input without panicking.
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
                    messages: &messages,
                    reasoning_effort: None,
                    shell_active: false,
                    busy: false,
                    unattended: false,
                    context_tokens: None,
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
                    messages: &Vec::<TranscriptMessage>::new(),
                    reasoning_effort: None,
                    shell_active,
                    busy: false,
                    unattended: false,
                    context_tokens: None,
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
    fn hint_bar_busy_shows_queue_action() {
        // When the agent is mid-round, Enter stages the message in the queue
        // (the queue bar below shows the staged item). The detailed
        // insert/next-round wording and Tab alternative now live in the queue
        // bar, not this sentence.
        let mut terminal = neenee_tui_engine::TestTerminal::new(120, 1);
        terminal.draw(|frame| {
            draw_hint_bar(
                frame,
                Rect::new(0, 0, 120, 1),
                HintBarView {
                    current_model: "mock",
                    messages: &[],
                    reasoning_effort: None,
                    shell_active: false,
                    busy: true,
                    unattended: false,
                    context_tokens: None,
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
                    messages: &[],
                    reasoning_effort: Some("high"),
                    shell_active: false,
                    busy: true,
                    unattended: false,
                    context_tokens: None,
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
        // whole line: the `◆ {effort}` tag must appear right after the model
        // name when reasoning is in use and be absent entirely otherwise.
        fn row_text(effort: Option<&str>) -> String {
            let mut terminal = neenee_tui_engine::TestTerminal::new(80, 1);
            terminal.draw(|f| {
                draw_hint_bar(
                    f,
                    Rect::new(0, 0, 80, 1),
                    HintBarView {
                        current_model: "mock",
                        messages: &Vec::<TranscriptMessage>::new(),
                        reasoning_effort: effort,
                        shell_active: false,
                        busy: false,
                        unattended: false,
                        context_tokens: None,
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

        // No reasoning → no diamond glyph anywhere on the row.
        assert!(!row_text(None).contains('◆'));
        // Reasoning on → `◆ high` appears after the model name.
        let on = row_text(Some("high"));
        assert!(on.contains("◆ high"), "missing ◆ high tag in: {on:?}");
        // A different effort level renders its own value, not a hardcoded one.
        assert!(row_text(Some("max")).contains("◆ max"));
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
                "/unattended",
                "Toggle unattended mode — agent runs without human intervention (on/off)",
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
        assert!(row_text.starts_with("/unattended"), "row was {row_text:?}");
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

    /// Read back a two-row region as joined text for assertion.
    fn queue_row_text(view: QueueBarView<'_>, width: u16, theme: &Theme) -> String {
        let mut terminal = neenee_tui_engine::TestTerminal::new(width, 2);
        terminal.draw(|f| {
            draw_queue_bar(f, Rect::new(0, 0, width, 2), view, theme);
        });
        let buf = terminal.buffer();
        let bw = buf.area().width as usize;
        let mut out = String::new();
        for y in 0..2usize {
            for x in 0..width as usize {
                out.push_str(buf.content[y * bw + x].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn queue_bar_empty_state_hints_how_to_stage() {
        let text = queue_row_text(
            QueueBarView {
                items: &[],
                paused: false,
            },
            70,
            &Theme::default(),
        );
        // Identity + zero count on row 1.
        assert!(text.contains("queue · 0 · --:--"), "row was {text:?}");
        // Empty hint on row 2.
        assert!(text.contains("queue empty"), "row was {text:?}");
    }

    #[test]
    fn queue_bar_previews_next_item_with_badge_and_text() {
        let item = QueueItemView {
            target: crate::tui::app::SendTarget::NextRound,
            queued_at_ms: 1_700_000_000_000,
            text: "fix the flaky test in parser".to_string(),
        };
        let text = queue_row_text(
            QueueBarView {
                items: &[item],
                paused: true,
            },
            70,
            &Theme::default(),
        );
        // Count reflects the one item.
        assert!(text.contains("queue · 1 ·"), "row was {text:?}");
        // Right-pinned target badge + preview text on row 2 (no arrow glyph).
        assert!(text.contains("next"), "next-round badge missing: {text:?}");
        assert!(!text.contains('⇥'), "arrow glyph leaked: {text:?}");
        assert!(
            text.contains("fix the flaky test"),
            "preview text missing: {text:?}"
        );
    }

    #[test]
    fn queue_bar_insert_target_uses_insert_badge() {
        let item = QueueItemView {
            target: crate::tui::app::SendTarget::Insert,
            queued_at_ms: 1_700_000_000_000,
            text: "add a comment".to_string(),
        };
        let text = queue_row_text(
            QueueBarView {
                items: &[item],
                paused: false,
            },
            70,
            &Theme::default(),
        );
        assert!(text.contains("insert"), "insert badge missing: {text:?}");
        assert!(!text.contains('→'), "arrow glyph leaked: {text:?}");
    }
}
