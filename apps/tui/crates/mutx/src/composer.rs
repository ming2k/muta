//! The live editable prompt box at the bottom of the screen: a rounded line
//! frame (`╭─ … ─╮` / `│` / `╰─ … ─╯`) drawn with stroke glyphs on the plain
//! surface — no filled panel — whose border rows carry the meta information
//! (compose target, Enter action, char counter) inlaid into the line itself.
//! Inside the frame, in-box text wrapping with vertical scroll keeps the
//! caret visible, and per-row layout-map recording drives semantic
//! selection / copy.

use mutx_engine::text::{cursor_column, str_len};
use mutx_engine::{
    Color, Frame, Modifier, Paragraph, Rect, Style, {Line, Span},
};

use crate::composer_attachments::{ChipKind, iter_chips};
use crate::model::layout::{BlockRegion, LayoutMap};
use crate::model::selection::SelectionState;

use super::Theme;
use super::components::composer_hints::compose_target_spans;
use super::design::{
    COMPOSER_PROMPT_PREFIX_COLS, COMPOSER_RIGHT_PAD_COLS, COMPOSER_RAIL_GAP_COLS,
    COMPOSER_TEXT_ROW_OFFSET, COMPOSER_VERTICAL_CHROME_ROWS,
};
use super::text_layout::{
    WrappedLine, block_selection_range, line_selection, wrap_text,
};

/// Build one rounded border row of the composer's line frame:
/// `╭── as: prompt ─────╮` (top) or `╰── Enter send ── 12 chars ─╯` (bottom).
///
/// The row is a run of `─` stroke glyphs in `frame_fg` between the two
/// rounded corners. The `info` spans are inlaid starting exactly at the
/// text column (`COMPOSER_PROMPT_PREFIX_COLS`, minus the corner glyph) so
/// the label reads as part of the same left margin as the text it
/// describes; the optional `tail` label is right-aligned with one stroke
/// column before the closing corner.
///
/// Degradation ladder (the frame never overflows or wraps):
/// 1. `info + tail` both inlaid;
/// 2. `tail` dropped (keys are the non-negotiable part on the bottom row);
/// 3. `info` dropped too — a plain full-width stroke run, with `tail` alone
///    right-aligned if it fits.
pub(super) fn build_frame_border_row(
    full_w: usize,
    top: bool,
    info: Option<Vec<Span<'static>>>,
    tail: Option<String>,
    frame_fg: Color,
    interior_bg: Color,
    muted_fg: Color,
) -> Line<'static> {
    if full_w < 2 {
        return Line::from(Span::styled(
            " ".repeat(full_w),
            Style::default().bg(interior_bg),
        ));
    }
    let (corner_l, corner_r) = if top { ("╭", "╮") } else { ("╰", "╯") };
    let corner_style = Style::default().fg(frame_fg).bg(interior_bg);
    let stroke = |cols: usize| {
        Span::styled("─".repeat(cols), Style::default().fg(frame_fg).bg(interior_bg))
    };
    let tail_span =
        |text: String| Span::styled(text, Style::default().fg(muted_fg).bg(interior_bg));
    // Stroke columns between the two corners.
    let run = full_w - 2;
    // Inlay begins where the composer's text begins: the prefix minus the
    // corner glyph's own column.
    let lead_in = (COMPOSER_PROMPT_PREFIX_COLS - 1).min(run);
    let tail_w = tail.as_deref().map(str_len).unwrap_or(0);
    let info_w = info
        .as_ref()
        .map(|spans| spans.iter().map(|span| str_len(&span.content)).sum())
        .unwrap_or(0);
    let mut spans = vec![Span::styled(corner_l, corner_style)];
    match info {
        Some(info_spans) if run >= lead_in + info_w + 2 => {
            spans.push(stroke(lead_in));
            spans.push(stroke(1)); // breathing room between line and label
            spans.extend(info_spans);
            let remaining = run - lead_in - 1 - info_w;
            if tail_w > 0 && remaining >= tail_w + 2 {
                // [gap stroke] tail [1 stroke] — remaining = gap + tail + 1.
                let gap = remaining - tail_w - 1;
                spans.push(stroke(gap));
                spans.push(tail_span(tail.unwrap_or_default()));
                spans.push(stroke(1));
            } else {
                spans.push(stroke(remaining));
            }
        }
        _ => {
            // Info (or its room) is gone; the tail alone may still fit,
            // right-aligned: `[stroke…] tail [stroke] corner`.
            if tail_w > 0 && run >= tail_w + 2 {
                spans.push(stroke(run - tail_w - 1));
                spans.push(tail_span(tail.unwrap_or_default()));
                spans.push(stroke(1));
            } else {
                spans.push(stroke(run));
            }
        }
    }
    spans.push(Span::styled(corner_r, corner_style));
    Line::from(spans)
}

/// Render plumbing for the composer draw family: frame, target rect,
/// theme, layout map, scroll state, and selection. Bundled so the three
/// composer entry points and the shared impl take (view, text, flags)
/// instead of threading eleven positional args.
pub struct ComposerView<'a, 'f: 'a> {
    pub frame: &'a mut Frame<'f>,
    pub input_rect: Rect,
    pub theme: &'a Theme,
    pub layout_map: &'a mut LayoutMap,
    pub input_scroll: &'a mut usize,
    pub selection: &'a SelectionState,
}

/// The composer text being rendered plus its byte cursor.
pub struct ComposerText<'a> {
    pub input: &'a str,
    pub byte_cursor: usize,
}

/// Special message_idx for the live input box in the layout map, so semantic
/// selection / copy works on input text just like transcript messages.
pub const INPUT_MSG_IDX: usize = usize::MAX - 2;

/// Build the wrapped-line list the composer renders, including the synthetic
/// trailing row it appends when the caret rests past the last wrapped line
/// (e.g. just after an inserted newline). Both the height computation in
/// [`super::draw_transcript`] and the actual rendering in [`draw_composer`] go
/// through this so the box never scrolls its own prompt glyph out of view on
/// the first newline.
fn composer_wrapped(input: &str, text_width: usize, byte_cursor: usize) -> Vec<WrappedLine> {
    let mut wrapped = wrap_text(input, text_width);
    let last_end = wrapped.last().map_or(0, |w| w.end_byte);
    if byte_cursor > last_end {
        wrapped.push(WrappedLine {
            text: String::new(),
            start_byte: last_end,
            end_byte: byte_cursor.max(last_end),
        });
    }
    // Always keep at least one row so an empty input box still records a
    // layout-map region: without it a click inside the empty box can't
    // resolve to a cursor and the click handler can't clear a focused step
    // to hand typing back to the prompt.
    if wrapped.is_empty() {
        wrapped.push(WrappedLine {
            text: String::new(),
            start_byte: 0,
            end_byte: 0,
        });
    }
    wrapped
}

/// Number of text rows the composer will render for `input`, accounting for
/// the trailing caret row. Always at least 1 so an empty box still reserves a
/// prompt line.
pub(crate) fn input_row_count(input: &str, text_width: usize, byte_cursor: usize) -> usize {
    composer_wrapped(input, text_width, byte_cursor)
        .len()
        .max(1)
}

/// Display column of `byte` inside the composer's wrapped text grid: the row
/// it lands on plus the column within that row, both relative to the text
/// area (the prompt glyph is added back by the caller when it needs screen
/// coordinates). Shares [`composer_wrapped`] with the draw path so the two
/// can never disagree on where a byte sits.
///
/// A byte exactly at a wrapped-line boundary resolves to column 0 of the
/// *next* row (the position the caret would occupy), so the completion
/// popup's leading edge follows the trigger token across wrap boundaries
/// instead of sticking to the end of the previous row.
pub fn composer_wrapped_pos(input: &str, text_width: usize, byte: usize) -> (usize, usize) {
    let wrapped = composer_wrapped(input, text_width, byte);
    // A byte exactly at a wrapped-line boundary (the end of one row and the
    // start of the continuation) resolves to column 0 of the continuation —
    // the position the trigger glyph itself occupies. Otherwise the byte
    // lands on the first row whose end covers it.
    for (row, wl) in wrapped.iter().enumerate() {
        if byte >= wl.start_byte && byte < wl.end_byte {
            let local = byte.saturating_sub(wl.start_byte).min(wl.text.len());
            return (row, cursor_column(&wl.text, local));
        }
    }
    (wrapped.len().saturating_sub(1), 0)
}

/// Display width of the composer's text area inside a box of `full_width`
/// columns (the total minus the left prompt prefix and right padding).
pub fn composer_text_width(full_width: usize) -> usize {
    full_width
        .saturating_sub(COMPOSER_PROMPT_PREFIX_COLS + COMPOSER_RIGHT_PAD_COLS)
        .max(1)
}

/// Compute the caret's screen coordinates `(x, y)` for `input` at `byte_cursor`
/// laid out inside `input_rect`, updating `input_scroll` in place to keep the
/// caret within the visible window.
///
/// This is the **single source of truth** for the caret's screen position. The
/// draw path resolves it once, then the terminal commit installs that final
/// coordinate while the physical cursor is hidden.
///
/// Returns `None` when `input_rect` has no room for text rows. The caller is
/// responsible for deciding whether the caret should be shown at all (modal
/// owning the keyboard, active selection, etc.).
pub fn cursor_screen_pos(
    input_rect: Rect,
    input: &str,
    byte_cursor: usize,
    input_scroll: &mut usize,
) -> Option<(u16, u16)> {
    let full_w = input_rect.width as usize;
    if full_w == 0 || input_rect.height == 0 {
        return None;
    }
    let text_width = full_w
        .saturating_sub(COMPOSER_PROMPT_PREFIX_COLS + COMPOSER_RIGHT_PAD_COLS)
        .max(1);
    let wrapped = composer_wrapped(input, text_width, byte_cursor);

    let visible_rows = (input_rect.height as usize)
        .saturating_sub(COMPOSER_VERTICAL_CHROME_ROWS as usize)
        .max(1);

    // Map the caret's byte offset onto the wrapped grid (mirrors the draw
    // loop's scan exactly).
    let mut cursor_line = wrapped.len().saturating_sub(1);
    let mut cursor_col = 0usize;
    for (i, wl) in wrapped.iter().enumerate() {
        if byte_cursor <= wl.end_byte {
            cursor_line = i;
            let local_byte = byte_cursor.saturating_sub(wl.start_byte).min(wl.text.len());
            cursor_col = cursor_column(&wl.text, local_byte);
            break;
        }
    }

    // Clamp the scroll window the same way the draw loop does.
    let max_scroll = wrapped.len().saturating_sub(visible_rows);
    if wrapped.len() <= visible_rows {
        *input_scroll = 0;
    } else {
        if cursor_line < *input_scroll {
            *input_scroll = cursor_line;
        } else if cursor_line >= *input_scroll + visible_rows {
            *input_scroll = cursor_line.saturating_sub(visible_rows - 1);
        }
        *input_scroll = (*input_scroll).min(max_scroll);
    }

    let visible_cursor_line = cursor_line.saturating_sub(*input_scroll);
    let cursor_y = input_rect.y + COMPOSER_TEXT_ROW_OFFSET + visible_cursor_line as u16;
    let cursor_x =
        input_rect.x + COMPOSER_PROMPT_PREFIX_COLS as u16 + cursor_col.min(text_width) as u16;
    Some((cursor_x, cursor_y))
}

/// Draw the flat input box panel at the bottom of the screen.
///
/// `focused` selects the panel palette (the input's dedicated active /
/// inactive background pair). The live composer passes `true` when
/// no transcript step carries keyboard focus, and `false` when the user has
/// navigated into the transcript with Ctrl+↑/↓ — the recessed "read-only"
/// band signals that the next keypress targets the step, not the input box.
///
/// `image_count` / `paste_count` are the numbers of attachments actually
/// staged behind the input's chips (`pending_images.len()` /
/// `pending_text_pastes.len()`). Only a chip whose `#N` has a real payload
/// (`N <= count`) is painted as a colored pill; an orphan label — typed by
/// hand, or left over after the paste was undone — renders as ordinary text
/// so it never reads as an attachment that isn't there.
///
/// The elevated delegated state is no longer signalled here; it lives on the
/// state bar directly below the input, separate from composer state.
pub fn draw_composer(
    view: ComposerView<'_, '_>,
    text: ComposerText<'_>,
    focused: bool,
    show_caret: bool,
    record: bool,
    image_count: usize,
    paste_count: usize,
    hints: crate::components::composer_hints::ComposerHints,
) {
    draw_composer_impl(
        view,
        text,
        ComposerDrawOptions {
            focused,
            show_caret,
            record,
            image_count,
            paste_count,
            hints,
        },
        None,
        None,
    )
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ComposerDrawOptions {
    pub focused: bool,
    pub show_caret: bool,
    pub record: bool,
    pub image_count: usize,
    pub paste_count: usize,
    /// Owned meta-row inputs: what the buffer is and what Enter does. Built
    /// by the caller before the mutable composer borrow begins.
    pub hints: crate::components::composer_hints::ComposerHints,
}

/// The effort-ignition variant of [`draw_composer`]: `prompt_accent` carries
/// the ignition's elapsed milliseconds while the wave is live, driving a
/// color-only tint on the `›` prompt (the glyph never changes). Once the
/// animation ends the caller passes no accent and the ordinary composer
/// renders. See [`super::effort_ignition`].
pub fn draw_composer_igniting(
    view: ComposerView<'_, '_>,
    text: ComposerText<'_>,
    options: ComposerDrawOptions,
    prompt_accent: (bool, Option<u128>),
) {
    draw_composer_impl(view, text, options, None, Some(prompt_accent));
}

/// Like [`draw_composer`], but paints the `highlight_len`-byte run at the
/// start of the input in bold + the theme's accent color. Used by the shell
/// to mark a resolved `/command` token so it reads differently from plain
/// prose (and from an unmatched `/`-prefix, which stays in the normal text
/// color). The length is clamped per wrapped row so the accent never bleeds
/// into the argument text when the input wraps.
pub fn draw_composer_highlighted(
    view: ComposerView<'_, '_>,
    text: ComposerText<'_>,
    options: ComposerDrawOptions,
    highlight_len: usize,
) {
    draw_composer_impl(view, text, options, Some(highlight_len), None);
}

fn draw_composer_impl(
    view: ComposerView<'_, '_>,
    text: ComposerText<'_>,
    options: ComposerDrawOptions,
    highlight_len: Option<usize>,
    prompt_accent: Option<(bool, Option<u128>)>,
) {
    let ComposerDrawOptions {
        focused,
        show_caret,
        record,
        image_count,
        paste_count,
        hints,
    } = options;
    let ComposerView {
        frame,
        input_rect,
        theme,
        layout_map,
        input_scroll,
        selection,
    } = view;
    let ComposerText { input, byte_cursor } = text;
    // The input box is a rounded line frame drawn with stroke glyphs on the
    // plain surface — no filled panel. Each text row is preceded by the
    // frame's left `│` rail, a gap, and (on the first wrapped line) the `›`
    // prompt; continuations repeat the rail and indent to the same column so
    // the caret stays aligned. The top and bottom edges are the `╭─ … ─╮` /
    // `╰─ … ─╯` border rows, whose runs of `─` carry the meta information
    // (compose target, Enter action, char counter) inlaid into the line
    // itself — "线条穿插回补" — so the information reads as part of the frame
    // instead of as bars of tinted background above/below the text.
    //
    // `focused` drives only the stroke palette: when `false` the frame drops
    // to its near-surface inactive stroke and the prompt glyph uses
    // `text_muted`, so the box visibly recedes — while staying an
    // input-owned surface (it is deliberately *not* the sent-user-message
    // panel, so the input's two states remain a related but independent
    // pair). The live composer passes `true`. The caret is gated separately
    // by `show_caret`: it is suppressed whenever a modal owns the keyboard
    // (the full-screen modal backdrop already signals "typing lands
    // elsewhere"), so the frame never shows a live caret inside a surface
    // that no longer accepts input.
    let frame_fg = if focused {
        theme.composer_frame()
    } else {
        theme.composer_frame_inactive()
    };
    // The frame's interior sits on the plain surface — the stroke carries the
    // box's identity now, not a raised tint. Spans still pin this bg (rather
    // than relying on `Color::Reset`) so selection, chips, and the ignition
    // overlay blend against the app's own surface color.
    let interior_bg = theme.surface();
    let prompt_fg = if focused {
        theme.brand()
    } else {
        theme.muted()
    };
    // Effort-ignition tint (codex port): while the ignition wave is live the
    // `›` prompt charges toward the fire accent — a color-only accent. The
    // glyph itself stays `›`; once the wave ends the prompt returns to its
    // ordinary palette. See `effort_ignition`.
    let prompt_fg = match prompt_accent {
        Some((_, ms)) if focused => {
            super::effort_ignition::ignition_prompt_color(ms, theme).unwrap_or(prompt_fg)
        }
        _ => prompt_fg,
    };
    let full_w = input_rect.width as usize;
    // Inner text budget: the full width minus the left prefix (rail + gap +
    // `›` + gap) and the matching right pad, so text never touches either
    // rail of the frame.
    let text_width = full_w
        .saturating_sub(COMPOSER_PROMPT_PREFIX_COLS + COMPOSER_RIGHT_PAD_COLS)
        .max(1);
    let wrapped = composer_wrapped(input, text_width, byte_cursor);

    // Number of text rows that fit inside the box (top/bottom transition rows
    // consume two lines). The box is sized by draw_transcript to fit the wrapped text
    // up to half the terminal height, so when the text exceeds this height we
    // scroll to keep the cursor visible.
    let visible_rows = (input_rect.height as usize)
        .saturating_sub(COMPOSER_VERTICAL_CHROME_ROWS as usize)
        .max(1);

    // The caret position (and the scroll clamp that keeps it on screen) is the
    // single source of truth in [`cursor_screen_pos`]. The draw path reuses it
    // so the rendered caret and the terminal cursor can never disagree — which
    // is what previously let the IME composition window drift by a frame.
    let (cursor_x, cursor_y) =
        cursor_screen_pos(input_rect, input, byte_cursor, input_scroll).unwrap_or((0, 0));

    let mut lines: Vec<Line> = Vec::with_capacity(visible_rows + 2);

    // ── Top border: `╭─── as: prompt ───╮` ─────────────────────────────────
    // The compose-target clause is inlaid into the top border's run of `─`,
    // positioned after a fixed lead-in so the label always starts at the
    // same column. While unfocused the clause is dropped entirely (the frame
    // recedes and the meta text would compete with a step-focused
    // transcript), leaving a plain `╭────╮` run.
    lines.push(build_frame_border_row(
        full_w,
        true,
        focused.then(|| {
            compose_target_spans(hints.compose_target, theme, interior_bg)
        }),
        None,
        frame_fg,
        interior_bg,
        theme.muted(),
    ));

    // Text rows: every row is closed on the left by the frame's `│` rail,
    // then a gap; the first logical line adds the `›` prompt glyph plus its
    // own gap, and every wrapped continuation indents to the same column so
    // the box reads as a shell-style prompt inside a lined frame. Only the
    // visible slice is rendered so overflowing content can scroll while the
    // box stays within its terminal-sized bounds.
    let rail = Span::styled("│", Style::default().bg(interior_bg).fg(frame_fg));
    let rail_gap = Span::styled(
        " ".repeat(COMPOSER_RAIL_GAP_COLS),
        Style::default().bg(interior_bg),
    );
    let prompt_glyph = Span::styled("›", Style::default().bg(interior_bg).fg(prompt_fg));
    let prompt_gap = Span::styled(
        " ".repeat(COMPOSER_PROMPT_PREFIX_COLS - COMPOSER_RAIL_GAP_COLS - 2),
        Style::default().bg(interior_bg),
    );
    let indent = Span::styled(
        " ".repeat(COMPOSER_PROMPT_PREFIX_COLS - 1),
        Style::default().bg(interior_bg),
    );    if wrapped.is_empty() {
        let used = COMPOSER_PROMPT_PREFIX_COLS;
        // Suffix after the text: interior tail + (RIGHT_PAD-1) air + the
        // right rail. tail + RIGHT_PAD + used = full_w.
        let tail_cols = full_w.saturating_sub(used + COMPOSER_RIGHT_PAD_COLS);
        lines.push(Line::from(vec![
            rail.clone(),
            rail_gap.clone(),
            prompt_glyph.clone(),
            prompt_gap.clone(),
            Span::styled(" ".repeat(tail_cols), Style::default().bg(interior_bg)),
            Span::styled(
                " ".repeat(COMPOSER_RIGHT_PAD_COLS - 1),
                Style::default().bg(interior_bg),
            ),
            rail.clone(),
        ]));
    } else {
        let start = *input_scroll;
        let end = (*input_scroll + visible_rows).min(wrapped.len());
        // Resolve the selection byte range for the whole input box once; each
        // wrapped line intersects it to find its own highlighted slice. The
        // composer records itself as a single block at `INPUT_MSG_IDX` /
        // block 0, so a drag or triple-click inside the box resolves here.
        let sel_range = block_selection_range(selection, INPUT_MSG_IDX, 0);
        let selected_bg = theme.selected();
        let text_fg = theme.fg();
        let base_text = Style::default().bg(interior_bg).fg(text_fg);
        // Resolved `/command` token: bold + accent color, echoing the
        // completion menu's command column so the two surfaces read alike.
        let accent_text = Style::default()
            .bg(interior_bg)
            .fg(theme.brand())
            .add_modifier(Modifier::BOLD);
        // Attachment chips (`[Image #N (size)]` / `[Pasted text #N +M lines
        // (size)]`) render as tinted "pills" so a pasted block reads as a
        // distinct object inside the live input instead of ordinary prose.
        // Paste chips take the calm blue, image chips the warm amber; each is
        // a bold colored label on a tinted band derived from the current
        // interior, so the identifier is both informative and identifiable.
        let chips = iter_chips(input);
        let chip_paste_fg = theme.chip_paste_fg();
        let chip_image_fg = theme.chip_image_fg();
        let chip_paste_style = Style::default()
            .bg(theme.chip_paste_bg(interior_bg))
            .fg(chip_paste_fg)
            .add_modifier(Modifier::BOLD);
        let chip_image_style = Style::default()
            .bg(theme.chip_image_bg(interior_bg))
            .fg(chip_image_fg)
            .add_modifier(Modifier::BOLD);
        for (i, wl) in wrapped[start..end].iter().enumerate() {
            let used = COMPOSER_PROMPT_PREFIX_COLS + str_len(&wl.text);
            let mut spans = if start + i == 0 {
                vec![
                    rail.clone(),
                    rail_gap.clone(),
                    prompt_glyph.clone(),
                    prompt_gap.clone(),
                ]
            } else {
                vec![rail.clone(), indent.clone()]
            };
            let selected = line_selection(sel_range, wl);
            // A resolved `/command` token is accented from the input's first
            // byte; clamp to this wrapped row so the accent stops at the wrap
            // boundary instead of bleeding into the argument text.
            let hl_end = highlight_len
                .filter(|_| start + i == 0)
                .map(|len| len.min(wl.text.len()));
            // Clamp each chip to this wrapped row, but only when the chip has
            // a payload really staged behind it (`N <= count` for its kind).
            // A chip split across a wrap boundary paints both fragments with
            // the same pill, so a pasted block stays visually contiguous as
            // it wraps. An orphan label — typed by hand, or left over after
            // the paste was undone — has no backing payload and renders as
            // plain text, so the colored pill never lies about an attachment
            // that isn't there.
            let mut chip_ranges: Vec<(usize, usize, ChipKind)> = Vec::new();
            for chip in &chips {
                let backed = match chip.kind {
                    ChipKind::Image => chip.number <= image_count,
                    ChipKind::Paste => chip.number <= paste_count,
                };
                if !backed || chip.end_byte <= wl.start_byte || chip.start_byte >= wl.end_byte {
                    continue;
                }
                let lo = chip.start_byte.saturating_sub(wl.start_byte);
                let hi = (chip.end_byte - wl.start_byte).min(wl.text.len());
                if lo < hi {
                    chip_ranges.push((lo, hi, chip.kind));
                }
            }
            push_styled_runs(
                &mut spans,
                &wl.text,
                hl_end,
                &chip_ranges,
                selected,
                RunStyles {
                    base: base_text,
                    accent: accent_text,
                    chip_paste: chip_paste_style,
                    chip_image: chip_image_style,
                    selected_bg,
                    chip_paste_fg,
                    chip_image_fg,
                },
            );
            // Close the row: interior tail, right-pad air, then the frame's
            // right `│` rail as the row's final column.
            // used + tail + (RIGHT_PAD-1) + 1 (rail) = full_w.
            let tail_cols = full_w
                .saturating_sub(used + COMPOSER_RIGHT_PAD_COLS);
            spans.push(Span::styled(
                " ".repeat(tail_cols),
                Style::default().bg(interior_bg),
            ));
            spans.push(Span::styled(
                " ".repeat(COMPOSER_RIGHT_PAD_COLS - 1),
                Style::default().bg(interior_bg),
            ));
            spans.push(rail.clone());
            lines.push(Line::from(spans));
        }
    }

    // ── Bottom border: `╰── Enter send ── 1.2k chars ─╯` ───────────────────
    // The Enter-action keys lead (left, after the corner) and the char
    // counter closes (right, before the corner). Both ride the border's `─`
    // run instead of a tinted bar; the row still degrades by width ladder
    // before the counter is ever dropped, and the counter is the first thing
    // the frame sheds when the run gets too short.
    {
        use crate::components::composer_hints::{
            ActionDensity, format_char_count, keys_row_spans,
        };
        let keys_width = full_w
            .saturating_sub(COMPOSER_PROMPT_PREFIX_COLS + COMPOSER_RIGHT_PAD_COLS)
            .max(8);
        let density = ActionDensity::for_width(keys_width);
        let keys = if focused {
            Some(keys_row_spans(
                hints.can_retry,
                density,
                hints.compose_target,
                theme,
                interior_bg,
            ))
        } else {
            None
        };
        let count_label = format_char_count(input.chars().count());
        let tail = if focused && !count_label.is_empty() {
            Some(count_label)
        } else {
            None
        };
        lines.push(build_frame_border_row(
            full_w,
            false,
            keys,
            tail,
            frame_fg,
            interior_bg,
            theme.muted(),
        ));
    }

    frame.render_widget(Paragraph::new(lines), input_rect);

    // Record each visible text row in the layout map so mouse drag selection
    // and copy work on the live input. Skipped when the API-key modal masks
    // the display (byte offsets wouldn't match the real input).
    if record {
        let start = *input_scroll;
        let end = (*input_scroll + visible_rows).min(wrapped.len());
        for (i, wl) in wrapped[start..end].iter().enumerate() {
            let row_y = input_rect.y + COMPOSER_TEXT_ROW_OFFSET + i as u16;
            layout_map.push(BlockRegion {
                message_idx: INPUT_MSG_IDX,
                block_idx: 0,
                start_byte: wl.start_byte,
                end_byte: wl.end_byte,
                text: wl.text.clone(),
                prefix_cols: COMPOSER_PROMPT_PREFIX_COLS as u16,
                rect: Rect::new(input_rect.x, row_y, full_w as u16, 1),
                hidden_ranges: Vec::new(),
            });
        }
    }

    // Position the caret relative to the visible slice, after the `> ` /
    // indent prefix. Gated by `show_caret` rather than `focused`: the caret is
    // hidden whenever a modal takes over input or a selection is active, so it
    // never sits inside a box that doesn't accept keypresses. The coordinates
    // come from the shared [`cursor_screen_pos`] so the rendered caret and the
    // terminal cursor are always identical.
    if show_caret {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

/// Push `text` onto `spans`, splitting it at every style boundary so each
/// emitted run is uniform. Boundaries come from the optional leading
/// `/command` accent (`accent_len` bytes, relative to `text`), the selection
/// range (`selected`, relative to `text`), and the attachment-chip ranges
/// (`chip_ranges`, `(lo, hi, kind)` relative to `text`). Precedence:
///
/// 1. **Selection** wins on background — the highlighted slice stays a
///    uniform `selected_bg` — but a chip keeps its identity color, so the
///    user can still see which pasted block is selected.
/// 2. **Chip pills** paint their tinted band + bold label (`chip_paste` /
///    `chip_image`), so paste chips and image chips read as distinct blocks.
/// 3. The **command accent** (bold + brand color).
/// 4. Plain base text.
///    Style set for `push_styled_runs`: the base/accent text styles, the two
///    chip styles, and the selection/chip foreground colors.
struct RunStyles {
    base: Style,
    accent: Style,
    chip_paste: Style,
    chip_image: Style,
    selected_bg: Color,
    chip_paste_fg: Color,
    chip_image_fg: Color,
}

fn push_styled_runs(
    spans: &mut Vec<Span<'static>>,
    text: &str,
    accent_len: Option<usize>,
    chip_ranges: &[(usize, usize, ChipKind)],
    selected: Option<(usize, usize)>,
    styles: RunStyles,
) {
    if text.is_empty() {
        return;
    }
    // Every byte offset where the run style can change: the text edges, the
    // accent boundary, the selection edges, and each chip's edges.
    let mut points: Vec<usize> = Vec::with_capacity(6 + chip_ranges.len() * 2);
    points.push(0);
    points.push(text.len());
    if let Some(len) = accent_len {
        points.push(len);
    }
    if let Some((lo, hi)) = selected {
        points.push(lo);
        points.push(hi);
    }
    for &(lo, hi, _) in chip_ranges {
        points.push(lo);
        points.push(hi);
    }
    points.sort_unstable();
    points.dedup();

    let is_selected = |p: usize| matches!(selected, Some((lo, hi)) if p >= lo && p < hi);
    let chip_of = |p: usize| {
        chip_ranges
            .iter()
            .find(|&&(lo, hi, _)| p >= lo && p < hi)
            .map(|&(_, _, kind)| kind)
    };
    let in_accent = |p: usize| accent_len.map(|len| p < len).unwrap_or(false);

    let mut i = 0;
    while i + 1 < points.len() {
        let lo = points[i];
        let hi = points[i + 1];
        i += 1;
        if lo >= hi {
            continue;
        }
        let style = if is_selected(lo) {
            match chip_of(lo) {
                Some(ChipKind::Paste) => Style::default()
                    .fg(styles.chip_paste_fg)
                    .bg(styles.selected_bg),
                Some(ChipKind::Image) => Style::default()
                    .fg(styles.chip_image_fg)
                    .bg(styles.selected_bg),
                None => styles.base.bg(styles.selected_bg),
            }
        } else if let Some(kind) = chip_of(lo) {
            match kind {
                ChipKind::Paste => styles.chip_paste,
                ChipKind::Image => styles.chip_image,
            }
        } else if in_accent(lo) {
            styles.accent
        } else {
            styles.base
        };
        spans.push(Span::styled(text[lo..hi].to_string(), style));
    }
}
