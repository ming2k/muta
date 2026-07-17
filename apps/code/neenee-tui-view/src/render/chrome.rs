//! Transient chrome around the input box: the activity bar with an
//! animated breathing-dot indicator shown above the input, the completion menu
//! anchored above the input, and the persistent hint bar pinned below the
//! input. The activity bar is also the click target that opens the Activity
//! modal (pursuit + plan + live activity), replacing the old always-pinned pursuit
//! bar and task panel.

use neenee_tui::{
    Block as RtBlock, Clear, Color, Frame, Line, Modifier, Paragraph, Rect, Span, Style,
};
use std::time::{Duration, Instant};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::document::TranscriptMessage;
use crate::layout::LayoutMap;

use super::Theme;
use super::design::{HINT_BAR_GAP_MIN, HINT_BAR_INNER_PADDING, HINT_BAR_SEGMENT_GAP};
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

/// Hit-test information returned by [`draw_activity_bar`]. Carries the
/// screen rect of the full bar plus an optional sub-rect covering the
/// `todos d/t` segment so the event loop can route a click on the todos
/// badge directly to the Todos section of the Activity modal.
pub struct ActivityBarHit {
    /// The full bar rect (click → Activity modal, Activity section).
    pub bar_rect: Rect,
    /// The `todos d/t` badge sub-rect (click → Activity modal, Todos section).
    /// `None` when no todos are shown (empty task list).
    pub todos_rect: Option<Rect>,
}

/// Draw the transient activity bar that sits directly above the input box.
/// Replaces the old inline `┃ neenee ⟳ <status>` indicator: the brand prefix
/// is dropped (the header already shows it) and the static `⟳` glyph is
/// replaced by a breathing-dot indicator so the harness never looks frozen.
///
/// Layout:
/// ```text
/// active:  <spinner> <status> (<elapsed> · Esc Esc to interrupt) [· » <pursuit>] [⚠ <alert>]      todos d/t
/// idle:   ready                                                                     todos d/t
/// ```
/// The left half is transient (turn-scoped); the right-pinned todos badge is
/// persistent and shows even while idle. Session-state flags such as
/// `unattended` deliberately do not live here: they have their own state bar
/// ([`draw_state_bar`]) so this row stays a pure activity surface.
///
/// The bar surfaces what the user most wants to know mid-turn — the live
/// status, whether a pursuit/plan is in flight, and how long the turn has
/// run — and is the click target that opens the Activity modal for the full
/// detail. Each segment is independently clickable: a click on the `todos`
/// badge opens the Todos section directly, while a click anywhere else opens
/// the Activity section. The structural counters (`turn N · round M ·
/// <model>`) live in the modal: they change rarely and take space, while the
/// bar is a glance surface. Segments are omitted when there is nothing to
/// report:
/// - pursuit badge only when a pursuit is armed (`⟴ <truncated objective>`);
/// - elapsed only while the turn timer is running;
/// - the whole left half only while a turn is active.
///
/// When the status string already carries a reason (e.g.
/// `retry 1/4 in 3s · <message>`), it flows through unchanged as the lead.
///
/// Returns `Some(ActivityBarHit)` when the bar is drawn so the event loop
/// can hit-test clicks and open the Activity modal; `None` when the bar is
/// hidden (no transient activity AND no todos).
#[allow(clippy::too_many_arguments)]
pub fn draw_activity_bar(
    frame: &mut Frame,
    rect: Rect,
    pursuit: Option<&neenee_core::Pursuit>,
    todos: Option<&neenee_core::TodoList>,
    review_alert: &str,
    turn_started_at: Option<Instant>,
    status: &str,
    spinner_phase: usize,
    theme: &Theme,
) -> Option<ActivityBarHit> {
    // The bar has two halves: a transient LEFT segment (spinner + shimmering
    // status + elapsed/interrupt hint + pursuit + review alert) shown only
    // while a turn is active, and a persistent RIGHT-pinned todos badge.
    // If neither half has content, the bar is hidden entirely.
    let status_active = !status.is_empty() && status != "idle";
    let dim = Style::default().fg(theme.muted());

    // ── Build the right-pinned todos badge ──
    // `todos d/t`, always right-aligned so it reads as a persistent status
    // chip distinct from the transient activity on the left.
    let mut todos_rect: Option<Rect> = None;
    let todos_badge: Option<(String, usize)> = todos.filter(|l| !l.items.is_empty()).map(|list| {
        use neenee_core::TodoStatus;
        let done = list.count(TodoStatus::Completed);
        let total = list.items.len();
        let badge = format!("todos {done}/{total}");
        let w = UnicodeWidthStr::width(badge.as_str());
        (badge, w)
    });
    let persistent_width = todos_badge.as_ref().map(|(_, width)| *width).unwrap_or(0);

    // If there is nothing to show at all, hide the bar — no point painting a
    // blank row.
    if !status_active && persistent_width == 0 {
        return None;
    }

    // ── Build the transient left segment ──
    let mut spans: Vec<Span> = Vec::new();
    if status_active {
        let spinner = spinner_glyph();
        let spinner_color = breathing_color(spinner_phase, theme.brand(), theme.surface());

        let row_width = rect.width as usize;
        // Keep one cell between transient activity and the persistent cluster,
        // plus the cluster's one-cell right margin. The separation is allowed
        // to disappear only when the terminal is too narrow to show any
        // transient text at all.
        let persistent_reserve = persistent_width + usize::from(persistent_width > 0) * 2;
        let available_width = row_width.saturating_sub(persistent_reserve);
        let elapsed = turn_started_at.map(|started| format_elapsed(started.elapsed()));
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

        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            spinner,
            Style::default()
                .fg(spinner_color)
                .add_modifier(Modifier::BOLD),
        ));

        // Lead segment: the live status — the thing that changes frame to
        // frame, so it receives the left-to-right shimmer. The structural
        // counters (turn/round/model) are deliberately absent; they live in
        // the Activity modal that this bar opens on click. Truncate this
        // segment first so the interrupt affordance survives narrow widths.
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
            spans.push(Span::styled(
                "Esc",
                Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(" ", dim));
            spans.push(Span::styled(
                "Esc",
                Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(" to interrupt)", dim));
        } else if show_interrupt_keys {
            // At the minimum supported terminal width, keep the actual keys
            // and drop only the explanatory words. The Activity help entry
            // supplies the long form if the user needs it.
            spans.push(Span::styled(" ", dim));
            spans.push(Span::styled(
                "Esc",
                Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(" ", dim));
            spans.push(Span::styled(
                "Esc",
                Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
            ));
        }

        // Pursuit badge: shown only while a pursuit is armed, as
        // `» <objective>`, so the user can tell at a glance that the turn is
        // part of a larger goal. The objective is truncated to keep the
        // single-line bar compact; the full text is one click away in the
        // Activity modal.
        if let Some(p) = pursuit.filter(|p| !p.is_complete) {
            // `»` (Latin-1) rather than a rarer arrow glyph (the old `⟴`
            // U+27F4 is absent from many terminal fonts and rendered as a
            // tofu box); width is unambiguously 1 cell everywhere.
            let objective = truncate_for_bar(&p.objective, 32);
            let pursuit_width =
                UnicodeWidthStr::width(" · » ") + UnicodeWidthStr::width(objective.as_str());
            let used_width: usize = spans
                .iter()
                .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
                .sum();
            if used_width + pursuit_width <= available_width {
                spans.push(Span::styled(" · » ", dim));
                spans.push(Span::styled(objective, dim));
            }
        }

        // Session-review alert (ADR-0016): surfaced when a periodic
        // diagnostic judged the turn watch-worthy or stuck. Rendered with
        // the same breathing luminance sweep as the running-indicator dot so
        // the alert pulses gently rather than sitting as a flat warning chip
        // — the motion draws the eye without being frantic. The persistent
        // interrupt hint before it already tells the user how to stop the
        // turn. Empty alert = clear (nothing rendered).
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
    } else {
        // A persistent right-side todos badge keeps this row alive while the
        // agent is idle. Name that state explicitly so the row does not look
        // like unexplained empty padding.
        spans.push(Span::styled(" ready", dim));
    }

    // ── Right-pin the persistent todos badge ──
    if persistent_width > 0 {
        let left_w: usize = spans
            .iter()
            .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
            .sum();
        let row_w = rect.width as usize;
        // Place the badge flush against the right edge with a 1-cell margin.
        let right_margin = 1;
        let gap = row_w.saturating_sub(left_w + persistent_width + right_margin);
        // The badge's absolute column = left_w + gap.
        let badge_col = rect.x + (left_w + gap) as u16;
        // Pad between the left segment and the badge. When idle the badge is
        // the only content, so the same leading padding pushes it to the
        // right edge rather than leaving it left-aligned.
        spans.push(Span::raw(" ".repeat(gap)));
        if let Some((badge, badge_w)) = todos_badge {
            spans.push(Span::styled(badge, dim));
            todos_rect = Some(Rect::new(badge_col, rect.y, badge_w as u16, 1));
        }
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
    Some(ActivityBarHit {
        bar_rect: rect,
        todos_rect,
    })
}

/// Draw the persistent state bar: one row directly below the input box that
/// hosts session-state indicators staying on for minutes or the whole session.
/// Today that is the unattended flag; the row is the designated home for
/// future ambient state (workspace, and friends) so neither the activity bar
/// above nor the hint bar below has to make room.
///
/// Flags are left-aligned and joined by ` · `. The caller allocates zero
/// rows when no flag is active, so an empty bar never consumes vertical
/// space; this function simply renders whatever flags it is given.
pub fn draw_state_bar(frame: &mut Frame, rect: Rect, unattended: bool, theme: &Theme) {
    // Each active session-state indicator becomes one flag on the row. New
    // indicators push onto this vec; the join below keeps the separator
    // handling identical no matter how many flags exist.
    let mut flags: Vec<Span> = Vec::new();
    if unattended {
        // The one flag that bypasses human oversight (no confirmations, no
        // questions) gets a clear but quiet treatment on the row: lowercase,
        // warning tone, bold.
        flags.push(Span::styled(
            "unattended",
            Style::default()
                .fg(theme.warn())
                .add_modifier(Modifier::BOLD),
        ));
    }

    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    for (index, flag) in flags.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" · ", Style::default().fg(theme.muted())));
        }
        spans.push(flag);
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
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
    pub messages: &'a [TranscriptMessage],
    /// Effective reasoning effort of the active model, shown as a `◆ {effort}`
    /// tag right after the model name — only when reasoning is actually in use
    /// for this model. The caller resolves the value and applies the
    /// per-protocol gating (Anthropic: shown only when thinking is opted in;
    /// OpenAI: shown whenever the model exposes an effort knob; Gemini:
    /// never), so this is `None` for models that are not reasoning. Mirrors
    /// the `◆ think on · {effort}` tag the `/provider` picker shows on a row.
    pub reasoning_effort: Option<&'a str>,
    /// True while the prompt is a `!`-prefixed shell command and no transcript
    /// step is focused. The left side explains the resulting Enter action in
    /// plain language instead of exposing an implementation-mode badge.
    pub shell_active: bool,
    /// Busy-send target for the next Enter. `false` is the default insert at
    /// the next safe boundary; `true` waits for a fresh round. Tab toggles it.
    pub busy: bool,
    pub send_next_round: bool,
    /// Compact outbox counts for the viewed session. Pending content stays out
    /// of scrollback; this fixed one-row summary is its persistent affordance.
    pub pending_insert: usize,
    pub pending_next_round: usize,
    pub outbox_paused: bool,
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

/// Build the left side of the bottom row as an action sentence, not a mode
/// badge. Density changes wording only; the selected Enter behavior never
/// disappears. At the smallest density, secondary instructions yield to the
/// current action and the number of messages waiting.
#[allow(clippy::too_many_arguments)]
fn input_action_spans(
    shell_active: bool,
    busy: bool,
    send_next_round: bool,
    pending_total: usize,
    outbox_paused: bool,
    density: ActionDensity,
    theme: &Theme,
    bg: Color,
) -> Vec<Span<'static>> {
    let key_style = Style::default()
        .fg(theme.fg())
        .bg(bg)
        .add_modifier(Modifier::BOLD);
    let hint_style = Style::default().fg(theme.muted()).bg(bg);
    let queue_style = Style::default()
        .fg(if outbox_paused {
            theme.warn()
        } else {
            theme.muted()
        })
        .bg(bg);
    let compact = matches!(density, ActionDensity::Compact | ActionDensity::Tiny);
    let tiny = matches!(density, ActionDensity::Tiny);
    let mut spans = vec![Span::styled("Enter", key_style)];

    if shell_active {
        spans.push(Span::styled(
            if compact { " run" } else { " run command" },
            hint_style,
        ));
    } else if busy {
        let action = match (send_next_round, compact) {
            (true, false) => " send after current reply",
            (true, true) => " send later",
            (false, false) => " add to current reply",
            (false, true) => " add now",
        };
        spans.push(Span::styled(action, hint_style));

        // On a tiny row with queued work, keep the chosen action and queue
        // count. The Tab alternative remains discoverable in Help and returns
        // automatically as soon as there is enough horizontal space.
        if !(tiny && pending_total > 0) {
            spans.push(Span::styled(" · ", hint_style));
            spans.push(Span::styled("Tab", key_style));
            let alternative = match (send_next_round, compact) {
                (true, false) => " add to current reply",
                (true, true) => " add now",
                (false, false) => " send after reply",
                (false, true) => " send later",
            };
            spans.push(Span::styled(alternative, hint_style));
        }
    } else {
        spans.push(Span::styled(" send", hint_style));
    }

    if pending_total > 0 {
        let state = if outbox_paused { "paused" } else { "waiting" };
        let count = if pending_total > 99 {
            "99+".to_string()
        } else {
            pending_total.to_string()
        };
        spans.push(Span::styled(format!(" · {count} {state}"), queue_style));
        if !tiny {
            spans.push(Span::styled(" · ", queue_style));
            spans.push(Span::styled("↑", key_style));
            spans.push(Span::styled(
                if compact { " edit" } else { " edit latest" },
                queue_style,
            ));
        }
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
        send_next_round,
        pending_insert,
        pending_next_round,
        outbox_paused,
        context_tokens,
    } = view;

    let bg = theme.surface();
    let full_w = rect.width as usize;

    // --- Left cluster: one sentence describing what the next Enter does.
    // Keep product language here: users should not need to learn the internal
    // round/turn distinction or decode transport arrows before sending.
    let pending_total = pending_insert + pending_next_round;
    let mut action_density = ActionDensity::Full;
    let mut zone_spans = input_action_spans(
        shell_active,
        busy,
        send_next_round,
        pending_total,
        outbox_paused,
        action_density,
        theme,
        bg,
    );
    let mut zone_pill_width = zone_spans.iter().map(|s| s.content.width()).sum::<usize>();

    // --- Right cluster: model name and context bar.
    // Build each segment separately so we can drop optional ones when the
    // terminal is too narrow.
    let context_max = crate::providers::model_context_window(current_model);

    let inner = HINT_BAR_INNER_PADDING;

    // Build right-side segments independently. Model identity is the last
    // ambient item to drop; reasoning effort drops first, then context usage.
    // The input action always wins when the row cannot hold both clusters.
    let model_label = crate::providers::model_display_name(current_model);
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
    // `/provider` picker uses for reasoning models. The context meter stays
    // the last segment of the cluster, so the click-target rect math below is
    // unaffected.
    let mut reasoning_spans: Vec<Span<'static>> = Vec::new();
    if let Some(effort) = reasoning_effort {
        // `◆ {effort}` — the diamond marks a reasoning model, mirroring the
        // glyph the `/provider` picker uses.
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

    // Context-usage segment: `89.2k (8%)`. Always shown when the model
    // reports a context window; the percentage takes the threshold color so
    // a nearly full window is unmissable.
    //
    // The harness owns projection semantics. Never infer AI context from the
    // rendered transcript: it contains durable command echoes, archived turns,
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
    let mut show_context = context_seg_width > 0;
    let right_width_for = |model: bool, reasoning: bool, context: bool| {
        let segment_count = usize::from(model) + usize::from(reasoning) + usize::from(context);
        usize::from(model) * model_width
            + usize::from(reasoning) * reasoning_width
            + usize::from(context) * context_seg_width
            + segment_count.saturating_sub(1) * HINT_BAR_SEGMENT_GAP
    };
    let fits = |left_width: usize, right_width: usize| {
        inner + left_width + if right_width > 0 { HINT_BAR_GAP_MIN } else { 0 } + right_width
            <= full_w
    };

    let mut right_width = right_width_for(show_model, show_reasoning, show_context);
    if !fits(zone_pill_width, right_width) {
        action_density = ActionDensity::Compact;
        zone_spans = input_action_spans(
            shell_active,
            busy,
            send_next_round,
            pending_total,
            outbox_paused,
            action_density,
            theme,
            bg,
        );
        zone_pill_width = zone_spans.iter().map(|s| s.content.width()).sum::<usize>();
    }
    if !fits(zone_pill_width, right_width) && show_reasoning {
        show_reasoning = false;
        right_width = right_width_for(show_model, show_reasoning, show_context);
    }
    if !fits(zone_pill_width, right_width) && show_context {
        show_context = false;
        right_width = right_width_for(show_model, show_reasoning, show_context);
    }
    if !fits(zone_pill_width, right_width) {
        action_density = ActionDensity::Tiny;
        zone_spans = input_action_spans(
            shell_active,
            busy,
            send_next_round,
            pending_total,
            outbox_paused,
            action_density,
            theme,
            bg,
        );
        zone_pill_width = zone_spans.iter().map(|s| s.content.width()).sum::<usize>();
    }
    if !fits(zone_pill_width, right_width) && show_model {
        show_model = false;
        right_width = right_width_for(show_model, show_reasoning, show_context);
    }

    let mut right_spans: Vec<Span<'static>> = Vec::new();
    let separator = || Span::styled(" ".repeat(HINT_BAR_SEGMENT_GAP), Style::default().bg(bg));
    for segment in [
        show_model.then_some(model_spans),
        show_reasoning.then_some(reasoning_spans),
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
        let mut terminal = neenee_tui::TestTerminal::new(width, 1);
        terminal.draw(|frame| {
            draw_activity_bar(
                frame,
                Rect::new(0, 0, width, 1),
                None,
                None,
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
    fn state_bar_shows_the_unattended_flag_in_warning_bold() {
        let theme = Theme::default();
        let mut terminal = neenee_tui::TestTerminal::new(80, 1);
        terminal.draw(|frame| {
            draw_state_bar(frame, Rect::new(0, 0, 80, 1), true, &theme);
        });
        let buffer = terminal.buffer();
        let text = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            text.trim_start().starts_with("unattended"),
            "row was {text:?}"
        );
        let flag_cell = buffer
            .content
            .iter()
            .find(|cell| cell.symbol() == "u")
            .expect("unattended flag cell");
        assert_eq!(flag_cell.fg, theme.warn());
        assert!(flag_cell.style.add.contains(Modifier::BOLD));
    }

    #[test]
    fn state_bar_renders_blank_when_no_flag_is_active() {
        let mut terminal = neenee_tui::TestTerminal::new(80, 1);
        terminal.draw(|frame| {
            draw_state_bar(frame, Rect::new(0, 0, 80, 1), false, &Theme::default());
        });
        let text = terminal
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.trim().is_empty(), "row was {text:?}");
    }

    #[test]
    fn narrow_runtime_row_keeps_todos_and_interrupt_keys() {
        let mut todos = neenee_core::TodoList::new();
        todos.items.push(neenee_core::TodoItem {
            id: neenee_core::TodoId(1),
            content: "keep the status row useful".to_string(),
            status: neenee_core::TodoStatus::Pending,
            created_at: 0,
            updated_at: 0,
        });
        let mut terminal = neenee_tui::TestTerminal::new(36, 1);
        terminal.draw(|frame| {
            draw_activity_bar(
                frame,
                Rect::new(0, 0, 36, 1),
                None,
                Some(&todos),
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
        assert!(text.contains("todos 0/1"), "row was {text:?}");
        // Session-state flags live on the state bar now; the activity row
        // never carries them, even when they would fit.
        assert!(!text.contains("unattended"), "row was {text:?}");
        assert!(text.ends_with("todos 0/1 "), "row was {text:?}");
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
        let mut terminal = neenee_tui::TestTerminal::new(80, 3);
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
                    send_next_round: false,
                    pending_insert: 0,
                    pending_next_round: 0,
                    outbox_paused: false,
                    context_tokens: None,
                },
                &theme,
            );
        });
    }

    #[test]
    fn hint_bar_describes_the_current_enter_action() {
        fn row_text(terminal: &mut neenee_tui::TestTerminal, shell_active: bool) -> String {
            let mut captured = String::new();
            terminal.draw(|f| {
                let view = HintBarView {
                    current_model: "",
                    messages: &Vec::<TranscriptMessage>::new(),
                    reasoning_effort: None,
                    shell_active,
                    busy: false,
                    send_next_round: false,
                    pending_insert: 0,
                    pending_next_round: 0,
                    outbox_paused: false,
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

        let mut terminal = neenee_tui::TestTerminal::new(80, 1);
        assert!(row_text(&mut terminal, false).contains("Enter send"));
        assert!(row_text(&mut terminal, true).contains("Enter run command"));
    }

    #[test]
    fn hint_bar_keeps_busy_target_and_pending_counts_fixed() {
        let mut terminal = neenee_tui::TestTerminal::new(120, 1);
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
                    send_next_round: true,
                    pending_insert: 2,
                    pending_next_round: 1,
                    outbox_paused: false,
                    context_tokens: None,
                },
                &Theme::default(),
            );
        });
        let buffer = terminal.buffer();
        let text = (0..buffer.area().width as usize)
            .map(|x| buffer.content[x].symbol().to_string())
            .collect::<String>();
        assert!(text.contains("Enter send after current reply"));
        assert!(text.contains("Tab add to current reply"));
        assert!(text.contains("3 waiting · ↑ edit latest"));
        assert!(!text.contains("INSERT"));
        assert!(!text.contains("NEXT"));
    }

    #[test]
    fn narrow_hint_bar_preserves_the_enter_action_before_metadata() {
        let mut terminal = neenee_tui::TestTerminal::new(36, 1);
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
                    send_next_round: true,
                    pending_insert: 2,
                    pending_next_round: 1,
                    outbox_paused: false,
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
        assert!(text.contains("Enter send later"), "row was {text:?}");
        assert!(text.contains("3 waiting"), "row was {text:?}");
        assert!(!text.contains("◆ high"), "row was {text:?}");
        assert!(!text.contains("Tab"), "row was {text:?}");
    }

    #[test]
    fn hint_bar_reasoning_tag_shows_effort_when_set() {
        // Render the full hint row for three effort states and read back the
        // whole line: the `◆ {effort}` tag must appear right after the model
        // name when reasoning is in use and be absent entirely otherwise.
        fn row_text(effort: Option<&str>) -> String {
            let mut terminal = neenee_tui::TestTerminal::new(80, 1);
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
                        send_next_round: false,
                        pending_insert: 0,
                        pending_next_round: 0,
                        outbox_paused: false,
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
    ) -> (neenee_tui::TestTerminal, Rect) {
        let theme = Theme::default();
        let completions = vec![
            crate::completion::Completion {
                label: "/pursue".to_string(),
                description: "Pursue a long-running objective".to_string(),
                replace_start: 0,
                replace_end: 2,
                kind: crate::completion::CompletionItemKind::Slash,
            },
            crate::completion::Completion {
                label: "/permissions".to_string(),
                description: "Manage permissions".to_string(),
                replace_start: 0,
                replace_end: 2,
                kind: crate::completion::CompletionItemKind::Slash,
            },
        ];
        let mut terminal = neenee_tui::TestTerminal::new(80, 12);
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
            ("/provider", "Select an LLM provider"),
            ("/tools", "Manage session tools (enable/disable)"),
            (
                "/unattended",
                "Toggle unattended mode — agent runs without human intervention (on/off)",
            ),
        ]
        .iter()
        .map(|(l, d)| crate::completion::Completion {
            label: l.to_string(),
            description: d.to_string(),
            replace_start: 0,
            replace_end: 1,
            kind: crate::completion::CompletionItemKind::Slash,
        })
        .collect::<Vec<_>>();
        let mut terminal = neenee_tui::TestTerminal::new(80, 12);
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
        assert!(first.contains("/pursue"), "row was {first:?}");
        assert!(
            first.contains("Pursue a long-running objective"),
            "row was {first:?}"
        );
        assert!(!first.contains('·'), "row was {first:?}");
    }
}
