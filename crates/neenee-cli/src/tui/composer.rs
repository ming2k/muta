//! The live editable prompt box at the bottom of the screen: in-box text
//! wrapping with vertical scroll to keep the caret visible, and per-row
//! layout-map recording for semantic selection / copy.

use neenee_tui_engine::text::{cursor_column, str_len};
use neenee_tui_engine::{
    Frame, Modifier, Paragraph, Rect, Style, {Line, Span},
};

use crate::tui::model::layout::{BlockRegion, LayoutMap};
use crate::tui::model::selection::SelectionState;

use super::Theme;
use super::design::{
    COMPOSER_PROMPT_PREFIX_COLS, COMPOSER_RIGHT_PAD_COLS, COMPOSER_TEXT_ROW_OFFSET,
    COMPOSER_VERTICAL_CHROME_ROWS,
};
use super::text_layout::{
    WrappedLine, block_selection_range, line_selection, padded_tail, wrap_text,
};

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
/// This is the **single source of truth** for the caret's screen position.
/// Both the per-frame draw path ([`draw_composer`]) and the input-driven
/// immediate flush (which syncs the terminal cursor to the IME *before* the
/// next draw, eliminating the one-frame lag that mis-anchors IME composition
/// windows) resolve through this function, so the two paths can never diverge.
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
/// `focused` selects the panel palette. The live composer passes `true` when
/// no transcript step carries keyboard focus, and `false` when the user has
/// navigated into the transcript with Ctrl+↑/↓ — the dimmer "read-only"
/// palette signals that the next keypress targets the step, not the input box.
///
/// The elevated autopilot state is no longer signalled here; it lives on the
/// state bar directly below the input, separate from composer state.
#[allow(clippy::too_many_arguments)]
pub fn draw_composer(
    frame: &mut Frame,
    input_rect: Rect,
    input: &str,
    byte_cursor: usize,
    focused: bool,
    show_caret: bool,
    theme: &Theme,
    layout_map: &mut LayoutMap,
    record: bool,
    input_scroll: &mut usize,
    selection: &SelectionState,
) {
    draw_composer_impl(
        frame,
        input_rect,
        input,
        byte_cursor,
        focused,
        show_caret,
        theme,
        layout_map,
        record,
        input_scroll,
        selection,
        None,
    )
}

/// Like [`draw_composer`], but paints the `highlight_len`-byte run at the
/// start of the input in bold + the theme's accent color. Used by the shell
/// to mark a resolved `/command` token so it reads differently from plain
/// prose (and from an unmatched `/`-prefix, which stays in the normal text
/// color). The length is clamped per wrapped row so the accent never bleeds
/// into the argument text when the input wraps.
#[allow(clippy::too_many_arguments)]
pub fn draw_composer_highlighted(
    frame: &mut Frame,
    input_rect: Rect,
    input: &str,
    byte_cursor: usize,
    focused: bool,
    show_caret: bool,
    theme: &Theme,
    layout_map: &mut LayoutMap,
    record: bool,
    input_scroll: &mut usize,
    selection: &SelectionState,
    highlight_len: usize,
) {
    draw_composer_impl(
        frame,
        input_rect,
        input,
        byte_cursor,
        focused,
        show_caret,
        theme,
        layout_map,
        record,
        input_scroll,
        selection,
        Some(highlight_len),
    )
}

#[allow(clippy::too_many_arguments)]
fn draw_composer_impl(
    frame: &mut Frame,
    input_rect: Rect,
    input: &str,
    byte_cursor: usize,
    focused: bool,
    show_caret: bool,
    theme: &Theme,
    layout_map: &mut LayoutMap,
    record: bool,
    input_scroll: &mut usize,
    selection: &SelectionState,
    highlight_len: Option<usize>,
) {
    // The input box is a flat panel: each text row carries panel_bg and is
    // prefixed with `› ` on the first wrapped line / a two-space indent on
    // continuations. The top and bottom edges are half-block rows so the panel
    // floats a half row off the app background.
    //
    // `focused` drives only the palette: when `false` the panel drops to the
    // dimmer `user_panel_bg` and the prompt glyph uses `text_muted`, matching
    // the already-sent user-message styling so the box visibly recedes. The
    // live composer passes `true`. The caret is gated separately by `show_caret`:
    // it is suppressed whenever a modal owns the keyboard (the full-screen
    // modal backdrop already signals "typing lands elsewhere"), so the panel
    // never shows a live caret inside a surface that no longer accepts input.
    let panel_bg = if focused {
        theme.input_surface()
    } else {
        theme.user_surface()
    };
    let prompt_fg = if focused {
        theme.brand()
    } else {
        theme.muted()
    };
    let app_bg = theme.surface();
    let full_w = input_rect.width as usize;
    // Reserve the left prompt prefix (`› `) and a matching right pad so text
    // never touches either edge of the input panel — the box reads as a
    // balanced solid band like the header.
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
    let top_edge = Span::styled("▄".repeat(full_w), Style::default().fg(panel_bg).bg(app_bg));
    let bottom_edge = Span::styled("▀".repeat(full_w), Style::default().fg(panel_bg).bg(app_bg));

    // Top edge: lower-half blocks so only the bottom half of the row carries
    // panel_bg, meeting the full-height text rows below it.
    lines.push(Line::from(top_edge.clone()));

    // Text rows: `› ` marks the first logical line and a two-space indent
    // marks every wrapped continuation, so the box reads as a shell-style
    // prompt. Only the visible slice is rendered so overflowing content can
    // scroll while the box stays within its terminal-sized bounds.
    let prompt_glyph = Span::styled("›", Style::default().bg(panel_bg).fg(prompt_fg));
    let prompt_gap = Span::styled(" ", Style::default().bg(panel_bg));
    let indent = Span::styled(
        " ".repeat(COMPOSER_PROMPT_PREFIX_COLS),
        Style::default().bg(panel_bg),
    );
    if wrapped.is_empty() {
        let used = COMPOSER_PROMPT_PREFIX_COLS;
        lines.push(Line::from(vec![
            prompt_glyph.clone(),
            prompt_gap.clone(),
            Span::styled(padded_tail(full_w, used), Style::default().bg(panel_bg)),
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
        let base_text = Style::default().bg(panel_bg).fg(text_fg);
        // Resolved `/command` token: bold + accent color, echoing the
        // completion menu's command column so the two surfaces read alike.
        let accent_text = Style::default()
            .bg(panel_bg)
            .fg(theme.brand())
            .add_modifier(Modifier::BOLD);
        for (i, wl) in wrapped[start..end].iter().enumerate() {
            let used = COMPOSER_PROMPT_PREFIX_COLS + str_len(&wl.text);
            let mut spans = if start + i == 0 {
                vec![prompt_glyph.clone(), prompt_gap.clone()]
            } else {
                vec![indent.clone()]
            };
            let selected = line_selection(sel_range, wl);
            // A resolved `/command` token is accented from the input's first
            // byte; clamp to this wrapped row so the accent stops at the wrap
            // boundary instead of bleeding into the argument text.
            let hl_end = highlight_len
                .filter(|_| start + i == 0)
                .map(|len| len.min(wl.text.len()));
            match (selected, hl_end) {
                (None, hl) => push_accented(&mut spans, &wl.text, hl, base_text, accent_text),
                (Some((lo, hi)), None) => {
                    if lo > 0 {
                        spans.push(Span::styled(wl.text[..lo].to_string(), base_text));
                    }
                    spans.push(Span::styled(
                        wl.text[lo..hi].to_string(),
                        base_text.bg(selected_bg),
                    ));
                    if hi < wl.text.len() {
                        spans.push(Span::styled(wl.text[hi..].to_string(), base_text));
                    }
                }
                // Selection wins over the command accent inside its range so
                // the highlighted slice stays uniformly selected.
                (Some((lo, hi)), Some(end)) => {
                    if lo > 0 {
                        push_accented(
                            &mut spans,
                            &wl.text[..lo],
                            Some(end.min(lo)),
                            base_text,
                            accent_text,
                        );
                    }
                    spans.push(Span::styled(
                        wl.text[lo..hi].to_string(),
                        base_text.bg(selected_bg),
                    ));
                    if hi < wl.text.len() {
                        push_accented(
                            &mut spans,
                            &wl.text[hi..],
                            Some(end.saturating_sub(hi)).filter(|e| *e > 0),
                            base_text,
                            accent_text,
                        );
                    }
                }
            }
            spans.push(Span::styled(
                padded_tail(full_w, used),
                Style::default().bg(panel_bg),
            ));
            lines.push(Line::from(spans));
        }
    }

    // Bottom edge: upper-half blocks so only the top half of the row carries
    // panel_bg, meeting the full-height text rows above it.
    lines.push(Line::from(bottom_edge));

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

/// Push `text` onto `spans`, styling the first `accent_len` bytes with
/// `accent` and the remainder with `base`. `None` (or a zero length) renders
/// the whole slice in `base`. The length is expected to land on a char
/// boundary — the composer clamps it against wrapped-line text, which always
/// breaks on grapheme boundaries.
fn push_accented(
    spans: &mut Vec<Span<'static>>,
    text: &str,
    accent_len: Option<usize>,
    base: Style,
    accent: Style,
) {
    match accent_len.filter(|len| *len > 0 && *len <= text.len()) {
        None => spans.push(Span::styled(text.to_string(), base)),
        Some(len) => {
            spans.push(Span::styled(text[..len].to_string(), accent));
            if len < text.len() {
                spans.push(Span::styled(text[len..].to_string(), base));
            }
        }
    }
}
