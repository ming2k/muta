//! Step rendering implementation: the summary primitives, the per-tool body
//! content renderers (code, listing, matches, bash, diff), and the top-level
//! orchestrators (`draw_tool_step`, `draw_reasoning_trace`, and
//! `draw_runner_inline_step`) that compose them. Also
//! produces the sticky pinned-step summary that
//! [`super::super::draw_transcript`] overlays while a step body is scrolled
//! into view. State and color resolution live in [`super`] (re-exported from
//! [`super::state`]).

use mutx_engine::{
    Color, Frame, Modifier, Paragraph, Rect, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::{Disclosure, Interaction, summary_text_color};

use crate::model::document::{Block, CommandPhase, Inline, TranscriptMessage};
use crate::model::layout::{
    BlockRegion, COMMAND_RESULT_BLOCK_IDX, LayoutMap, THINKING_BLOCK_IDX,
};
use crate::model::selection::{CellDragInfo, SelectionState};

use crate::message_body::draw_message_body;
use crate::text_layout::{
    CodeGutterParams, RichLineParams, RichTextColors, RichTextRanges, WrappedLine,
    block_selection_range, code_gutter_line, line_selection, line_spans, line_spans_rich,
    padded_tail, wrap_text,
};
use crate::tools::{ArgLayout, DiffCache, DiffHunk, DiffOp, ResultKind, ToolStatus};
use crate::view::{
    BASH_FOLD_HEAD_ROWS, BASH_FOLD_TAIL_ROWS, CODE_BAND_GUTTER_GAP, CODE_BAND_GUTTER_MIN_WIDTH,
    REASONING_TRACE_BLOCK_GAP_ROWS, REASONING_TRACE_BODY_TOP_GAP_ROWS, STEP_MIN_WIDTH, StickyInfo,
    TOOL_STEP_BODY_INDENT_COLS, TOOL_STEP_BODY_TOP_GAP_ROWS, TOOL_STEP_CHILDREN_GAP_ROWS,
    TRANSCRIPT_BODY_LEADING_INDENT, Theme,
};

use crate::design::TURN_HEADER_BODY_GAP_ROWS;

/// Cursor + environment carried through the tool-step body renderers.
///
/// Bundles the per-frame paint state (frame, viewport rect, scroll
/// accumulators, theme, layout map) so content renderers take a single
/// `&mut RenderCtx` plus their content-specific arguments, instead of 6-8
/// positional cursor args threaded through every helper. This is the
/// extraction seam for the tool-rendering redesign (ADR-0001); higher-level
/// orchestration still constructs a `RenderCtx` at the boundary.
pub(crate) struct RenderCtx<'a, 'f: 'a> {
    pub frame: &'a mut Frame<'f>,
    pub area: Rect,
    pub full_width: usize,
    pub theme: &'a Theme,
    pub layout_map: &'a mut LayoutMap,
    pub skip_rows: &'a mut usize,
    pub y: &'a mut u16,
    pub content_lines: &'a mut usize,
}

impl<'a, 'f: 'a> RenderCtx<'a, 'f> {
    /// Assemble a render context from the raw cursor state owned by a caller.
    #[allow(clippy::too_many_arguments)]
    pub fn from_cursor(
        frame: &'a mut Frame<'f>,
        area: Rect,
        full_width: usize,
        theme: &'a Theme,
        layout_map: &'a mut LayoutMap,
        skip_rows: &'a mut usize,
        y: &'a mut u16,
        content_lines: &'a mut usize,
    ) -> Self {
        Self {
            frame,
            area,
            full_width,
            theme,
            layout_map,
            skip_rows,
            y,
            content_lines,
        }
    }

    /// Advance the cursor over `rows` unpainted blank rows, honoring
    /// scroll-skip and the viewport clip. The accounting twin of [`Self::paint`]
    /// for rows that produce no output (gaps, padding): `content_lines` still
    /// grows so the scroll height stays honest.
    pub fn advance_blank_rows(&mut self, rows: usize) {
        for _ in 0..rows {
            *self.content_lines += 1;
            if *self.skip_rows > 0 {
                *self.skip_rows = self.skip_rows.saturating_sub(1);
            } else if *self.y < self.area.y + self.area.height {
                *self.y += 1;
            }
        }
    }

    /// Paint one already-built line at the cursor, honoring scroll-skip and
    /// viewport clip. Always accounts the row in `content_lines`, so callers
    /// must iterate every logical row even once the viewport is full —
    /// short-circuiting would undercount the scroll height. This reproduces
    /// the original "bulk-count then paint until clip" accounting per-row.
    ///
    /// Returns the painted `Rect` when the row was actually drawn (so callers
    /// can record a selectable [`BlockRegion`] for it), or `None` when the row
    /// was skipped or fell outside the viewport.
    pub fn paint(&mut self, line: Line<'static>) -> Option<Rect> {
        *self.content_lines += 1;
        if *self.skip_rows > 0 {
            *self.skip_rows = self.skip_rows.saturating_sub(1);
            return None;
        }
        if *self.y >= self.area.y + self.area.height {
            return None;
        }
        let rect = Rect::new(self.area.x, *self.y, self.area.width, 1);
        self.frame.render_widget(Paragraph::new(line), rect);
        *self.y += 1;
        Some(rect)
    }

    /// Paint `line` and, when drawn, record a selectable text region anchored
    /// at `wl`'s byte range under `(mi, block_idx)`. Collapses the per-row
    /// skip/clip/paint/record boilerplate that was duplicated across every
    /// content renderer.
    pub fn paint_text_row(
        &mut self,
        line: Line<'static>,
        mi: usize,
        block_idx: usize,
        wl: &WrappedLine,
        prefix_cols: u16,
        hidden_ranges: &[(usize, usize)],
    ) {
        if let Some(rect) = self.paint(line) {
            self.layout_map.push(BlockRegion {
                message_idx: mi,
                block_idx,
                start_byte: wl.start_byte,
                end_byte: wl.end_byte,
                text: wl.text.clone(),
                prefix_cols,
                rect,
                hidden_ranges: hidden_ranges.to_vec(),
            });
        }
    }
}

/// `WrappedLine::empty()`-on-empty fallback used by every content renderer so
/// a blank logical line still occupies one rendered row (matching the
/// original inline `if wrapped.is_empty() { vec![empty] } else { wrapped }`).
fn nonempty_wrapped(wrapped: Vec<WrappedLine>) -> Vec<WrappedLine> {
    if wrapped.is_empty() {
        vec![WrappedLine {
            text: String::new(),
            start_byte: 0,
            end_byte: 0,
        }]
    } else {
        wrapped
    }
}


/// Tracked info for an expanded step, used to render a sticky summary pinned
/// under the HUD bar while the step's body is scrolled into view.
pub struct StickyStep {
    message_idx: usize,
    summary: String,
    color: Color,
    background: Option<Color>,
    summary_line: usize,
    body_end_line: usize,
}

/// Truncate `text` so its display width never exceeds `max_width` columns,
/// appending `…` when it is cut. Operates on grapheme clusters so multi-cell
/// glyphs (CJK, emoji) are not split mid-glyph, and the ellipsis only lands
/// when there is at least one column of headroom for it. Unlike the char-based
/// `truncate` in `paint::tools`, this respects terminal geometry rather than
/// a fixed character budget, so a long summary collapses to fit the band
/// instead of overflowing the right gutter.
fn truncate_to_width(text: &str, max_width: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;

    if text.width() <= max_width && !text.contains(['\n', '\r']) {
        return text.to_string();
    }
    // Reserve one column for the ellipsis; if there is no room even for it,
    // cut as many graphemes as fit in `max_width` with no suffix.
    let budget = max_width.saturating_sub(1);
    let mut out = String::new();
    for g in text.graphemes(true) {
        if g == "\n" || g == "\r" || g == "\r\n" {
            break;
        }
        let w = g.width();
        if out.width() + w > budget {
            break;
        }
        out.push_str(g);
    }
    out.push('…');
    out
}

/// Build the summary line for a tool/runner step: an optional expand marker
/// followed by the summary text, padded to `full_width`.
///
/// The focus affordance is carried entirely by color (resolved upstream through
/// `summary_text_color` / `summary_weight`, which maps a focused step to the
/// hover tone), so this builder needs no focus flag of its own.
///
/// The summary text is display-width-clamped to the remaining columns after
/// the expand marker, so a long header can never overflow the band (and thus
/// never eat the right gutter). This is the render-time guard: the content is
/// also pre-truncated to a char budget at generation time, but that budget is
/// fixed and ignores terminal width, so it alone cannot hold the right edge.
fn tool_summary_line(
    expand: &str,
    summary: &str,
    fg: Color,
    bg: Color,
    full_width: usize,
) -> Line<'static> {
    let base = Style::default().bg(bg);
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(3);
    let mut used = 0usize;

    if !expand.is_empty() {
        let s = format!("{} ", expand);
        used += s.width();
        spans.push(Span::styled(s, base.fg(fg).add_modifier(Modifier::BOLD)));
    }

    // Clamp the summary to the columns that remain inside the band so the
    // trailing `padded_tail` has at least its right gutter to fill; without
    // this a header wider than `full_width` drives `padded_tail` to zero and
    // the text spills past the right edge.
    let summary_budget = full_width.saturating_sub(used);
    let clamped = truncate_to_width(summary, summary_budget);
    used += clamped.width();
    spans.push(Span::styled(
        clamped,
        base.fg(fg).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(padded_tail(full_width, used), base));
    Line::from(spans)
}

/// Render the shared summary of an expandable step and record its rect in the
/// layout map so clicks / `Enter` on it can toggle the step. Returns the
/// content-line index of the summary (used for sticky-pin tracking).
///
/// `block_idx` is the sentinel recorded in [`BlockRegion`] so the click handler
/// can tell step/trace kinds apart: `usize::MAX` for tool steps and
/// `usize::MAX - 1` for reasoning traces.
fn draw_step_summary(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    expanded: bool,
    summary: &str,
    summary_color: Color,
    bg: Color,
) -> usize {
    let expand = if expanded {
        MARKER_EXPANDED
    } else {
        MARKER_COLLAPSED
    };
    let summary_line_idx = *ctx.content_lines;

    let line = tool_summary_line(expand, summary, summary_color, bg, ctx.full_width);
    if let Some(rect) = ctx.paint(line) {
        ctx.layout_map.push(BlockRegion {
            message_idx: mi,
            block_idx,
            start_byte: 0,
            end_byte: 0,
            text: String::new(),
            prefix_cols: 0,
            rect,
            hidden_ranges: Vec::new(),
        });
    }

    summary_line_idx
}

/// Draw blank rows padded to `full_width` with `style`'s background. The row
/// count is supplied by component spacing tokens in `design.rs`.
fn draw_blank_rows(ctx: &mut RenderCtx<'_, '_>, style: Style, rows: usize) {
    for _ in 0..rows {
        let _ = ctx.paint(Line::from(Span::styled(
            padded_tail(ctx.full_width, 0),
            style,
        )));
    }
}

/// Render text content as a code block with a line-number gutter on
/// `code_surface`. Used for `read_text` / `edit_file` results and as the
/// fallback for unrecognized tools. The gutter starts at column `indent`
/// so the code aligns with the rest of the step body.
///
/// When `language` is `Some`, a subtle language tag is drawn on its own dim
/// line above the gutter — matching the markdown `Block::Code` band, so a
/// code block reads identically whether it sits in assistant prose or inside
/// an expanded tool step (the block-level design contract).
///
/// `start_line` is the 1-based file line of the first row of `content`
/// (carried by `ToolOutput::Code::start_line`). `0` means "unknown" — the
/// renderer then numbers the slice 1, 2, 3… The gutter width is derived from
/// the *highest* displayed line number (not the line *count*) so an offset
/// snippet like 100..104 still gets a 3-wide column instead of overflowing.
#[allow(clippy::too_many_arguments)]
fn draw_code_content(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    content: &str,
    start_line: usize,
    language: Option<&str>,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
) {
    let code_bg = ctx.theme.code_surface();
    let mut logical_lines: Vec<(usize, &str)> = Vec::new();
    let mut offset = 0usize;
    for line in content.split('\n') {
        logical_lines.push((offset, line));
        offset += line.len() + 1;
    }
    // `0` (unknown) is indistinguishable from `1` for gutter purposes: both
    // render the first row as line 1. Normalize once so the math below is
    // uniform.
    let first_line = start_line.max(1);
    let last_line = first_line.saturating_add(logical_lines.len().saturating_sub(1));
    let gutter_width = last_line.to_string().len().max(CODE_BAND_GUTTER_MIN_WIDTH);
    let left_indent = indent;
    let gutter_gap = CODE_BAND_GUTTER_GAP;
    let gutter_indent = left_indent + 1 /* space */ + gutter_width + gutter_gap;
    let wrap_width = inner_w.saturating_sub(1 + gutter_width + gutter_gap);
    let sel_range = block_selection_range(selection, mi, block_idx);

    // Subtle language tag on its own dim line above the gutter — mirrors the
    // markdown `Block::Code` band so both block origins share one code-block
    // design.
    if let Some(lang) = language.filter(|l| !l.is_empty()) {
        let pad = Style::default().bg(code_bg);
        let used = left_indent + 1 + lang.len();
        let line = Line::from(vec![
            Span::styled(" ".repeat(left_indent), pad),
            Span::styled(" ", pad),
            Span::styled(
                lang.to_string(),
                Style::default().bg(code_bg).fg(ctx.theme.dim()),
            ),
            Span::styled(padded_tail(ctx.full_width, used), pad),
        ]);
        ctx.paint(line);
    }

    for (line_idx, (line_start_byte, logical_line)) in logical_lines.iter().enumerate() {
        let wrapped = nonempty_wrapped(wrap_text(logical_line, wrap_width));
        for (wrap_idx, wl) in wrapped.iter().enumerate() {
            let gutter = if wrap_idx == 0 {
                format!("{:>width$}", first_line + line_idx, width = gutter_width)
            } else {
                " ".repeat(gutter_width)
            };

            let block_wl = WrappedLine {
                text: wl.text.clone(),
                start_byte: line_start_byte + wl.start_byte,
                end_byte: line_start_byte + wl.end_byte,
            };

            let line = code_gutter_line(CodeGutterParams {
                left_bar: Color::Reset,
                left_indent,
                gutter: &gutter,
                gutter_gap,
                code_bg,
                gutter_fg: ctx.theme.dim(),
                text: &wl.text,
                selected: line_selection(sel_range, &block_wl),
                code_fg: ctx.theme.code_text(),
                selected_bg: ctx.theme.selected(),
                full_width: ctx.full_width,
            });
            ctx.paint_text_row(line, mi, block_idx, &block_wl, gutter_indent as u16, &[]);
        }
    }
}

/// Render a `list_dir` / `find_files` result: one entry per row on `code_bg`,
/// directories (entries ending in `/`) in `info`, files in `code_fg`. No
/// line-number gutter since listing rows have no meaningful line index.
fn draw_listing_content(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    content: &str,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
) {
    let code_bg = ctx.theme.code_surface();
    let pad = Style::default().bg(code_bg);
    let dir_fg = ctx.theme.info();
    let file_fg = ctx.theme.code_text();
    let sel_range = block_selection_range(selection, mi, block_idx);
    let wrap_w = inner_w.max(1);

    let mut logical_lines: Vec<(usize, &str)> = Vec::new();
    let mut offset = 0usize;
    for line in content.split('\n') {
        logical_lines.push((offset, line));
        offset += line.len() + 1;
    }

    for (line_start_byte, logical_line) in logical_lines.iter() {
        let is_dir = logical_line.ends_with('/');
        let fg = if is_dir { dir_fg } else { file_fg };
        let base = Style::default().bg(code_bg).fg(fg);
        let wrapped = nonempty_wrapped(wrap_text(logical_line, wrap_w));
        for wl in &wrapped {
            let block_wl = WrappedLine {
                text: wl.text.clone(),
                start_byte: line_start_byte + wl.start_byte,
                end_byte: line_start_byte + wl.end_byte,
            };
            let mut line = line_spans(
                &" ".repeat(indent),
                pad,
                &wl.text,
                line_selection(sel_range, &block_wl),
                base,
                ctx.theme.selected(),
            );
            let used = indent + wl.text.width();
            line.spans
                .push(Span::styled(padded_tail(ctx.full_width, used), pad));
            ctx.paint_text_row(line, mi, block_idx, &block_wl, indent as u16, &[]);
        }
    }
}

/// Render a `write_todos` / checklist result: structured task list with status glyphs
/// [✓] completed, [•] in_progress, [☐] pending, [✕] cancelled on `code_bg`.
#[allow(clippy::too_many_arguments)]
fn draw_checklist_content(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    output: &str,
    arguments: &str,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
) {
    let code_bg = ctx.theme.code_surface();
    let pad = Style::default().bg(code_bg);
    let sel_range = block_selection_range(selection, mi, block_idx);
    let wrap_w = inner_w.max(1);

    #[derive(serde::Deserialize)]
    struct RawItem {
        #[serde(default)]
        content: String,
        #[serde(default)]
        status: String,
    }

    #[derive(serde::Deserialize)]
    struct RawList {
        #[serde(default)]
        items: Vec<RawItem>,
    }

    let parsed_items: Vec<RawItem> = serde_json::from_str::<Vec<RawItem>>(output)
        .or_else(|_| serde_json::from_str::<RawList>(output).map(|l| l.items))
        .or_else(|_| serde_json::from_str::<RawList>(arguments).map(|l| l.items))
        .or_else(|_| {
            let v: Result<serde_json::Value, _> = serde_json::from_str(arguments);
            if let Ok(v) = v
                && let Some(arr) = v.get("items").and_then(|v| v.as_array())
            {
                let items = arr
                    .iter()
                    .map(|item| RawItem {
                        content: item
                            .get("content")
                            .and_then(|s| s.as_str())
                            .unwrap_or("")
                            .to_string(),
                        status: item
                            .get("status")
                            .and_then(|s| s.as_str())
                            .unwrap_or("pending")
                            .to_string(),
                    })
                    .collect();
                return Ok(items);
            }
            Err(())
        })
        .unwrap_or_default();

    if parsed_items.is_empty() {
        draw_listing_content(ctx, mi, block_idx, output, selection, indent, inner_w);
        return;
    }

    let mut offset = 0usize;
    for item in &parsed_items {
        let (glyph, glyph_style, text_style) = match item.status.as_str() {
            "completed" | "done" => (
                "✓ ",
                Style::default()
                    .bg(code_bg)
                    .fg(ctx.theme.ok())
                    .add_modifier(Modifier::BOLD),
                Style::default().bg(code_bg).fg(ctx.theme.muted()),
            ),
            "in_progress" => (
                "• ",
                Style::default()
                    .bg(code_bg)
                    .fg(ctx.theme.info())
                    .add_modifier(Modifier::BOLD),
                Style::default()
                    .bg(code_bg)
                    .fg(ctx.theme.code_text())
                    .add_modifier(Modifier::BOLD),
            ),
            "cancelled" => (
                "✕ ",
                Style::default().bg(code_bg).fg(ctx.theme.err()),
                Style::default()
                    .bg(code_bg)
                    .fg(ctx.theme.dim())
                    .add_modifier(Modifier::STRIKETHROUGH),
            ),
            _ => (
                "☐ ",
                Style::default().bg(code_bg).fg(ctx.theme.dim()),
                Style::default().bg(code_bg).fg(ctx.theme.code_text()),
            ),
        };

        let logical_line = format!("{}{}", glyph, item.content);
        let wrapped = nonempty_wrapped(wrap_text(&logical_line, wrap_w));

        for (idx, wl) in wrapped.iter().enumerate() {
            let block_wl = WrappedLine {
                text: wl.text.clone(),
                start_byte: offset + wl.start_byte,
                end_byte: offset + wl.end_byte,
            };

            let mut line = if idx == 0 && wl.text.starts_with(glyph) {
                let rest_text = &wl.text[glyph.len()..];
                let mut spans = vec![
                    Span::styled(" ".repeat(indent), pad),
                    Span::styled(glyph, glyph_style),
                ];
                let rest_spans = line_spans(
                    "",
                    pad,
                    rest_text,
                    line_selection(sel_range, &block_wl),
                    text_style,
                    ctx.theme.selected(),
                );
                spans.extend(rest_spans.spans);
                Line::from(spans)
            } else {
                line_spans(
                    &" ".repeat(indent + 2),
                    pad,
                    &wl.text,
                    line_selection(sel_range, &block_wl),
                    text_style,
                    ctx.theme.selected(),
                )
            };

            let used = indent + wl.text.width();
            line.spans
                .push(Span::styled(padded_tail(ctx.full_width, used), pad));
            ctx.paint_text_row(line, mi, block_idx, &block_wl, indent as u16, &[]);
        }
        offset += logical_line.len() + 1;
    }
}

/// A single logical line parsed out of `search_text`'s `path:linenum:content` format.
struct MatchLine<'a> {
    path: &'a str,
    lineno: &'a str,
    content: &'a str,
    /// Byte offset of `content` within the original ripgrep output line.
    content_offset: usize,
}

/// Parse `path:linenum:content` (ripgrep's default with `-n`). Paths may
/// contain `:` (e.g. Windows `C:\foo`), so the scan accepts the first colon
/// that is followed by an all-digit run and another colon as the
/// line-number separator. Returns `None` for blank separators or any line
/// that doesn't match the ripgrep shape.
fn parse_match_line(line: &str) -> Option<MatchLine<'_>> {
    for (idx, ch) in line.char_indices() {
        if ch != ':' {
            continue;
        }
        let after = &line[idx + 1..];
        let digits_end = after
            .char_indices()
            .take_while(|(_, c)| c.is_ascii_digit())
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        if digits_end > 0 && after.as_bytes().get(digits_end) == Some(&b':') {
            let path = &line[..idx];
            if path.is_empty() {
                continue;
            }
            let lineno = &after[..digits_end];
            let content = &after[digits_end + 1..];
            let content_offset = idx + 1 + digits_end + 1;
            return Some(MatchLine {
                path,
                lineno,
                content,
                content_offset,
            });
        }
    }
    None
}

/// Emit `text` as one or more wrapped rows at column `indent`, all styled
/// with `style` on `pad`'s background, recording a selectable [`BlockRegion`]
/// per row whose byte range is anchored at `abs_start` within the tool
/// output. Used for search path headers, ripgrep separator rows, and any
/// other "simple" result row that doesn't need a line-number gutter.
#[allow(clippy::too_many_arguments)]
fn emit_simple_rows(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    indent: usize,
    text: &str,
    abs_start: usize,
    pad: Style,
    style: Style,
    sel_range: Option<(usize, Option<usize>)>,
) {
    let wrap_w = ctx.full_width.saturating_sub(indent).max(1);
    let wrapped = nonempty_wrapped(wrap_text(text, wrap_w));
    for wl in &wrapped {
        let block_wl = WrappedLine {
            text: wl.text.clone(),
            start_byte: abs_start + wl.start_byte,
            end_byte: abs_start + wl.end_byte,
        };
        let mut line = line_spans(
            &" ".repeat(indent),
            pad,
            &wl.text,
            line_selection(sel_range, &block_wl),
            style,
            ctx.theme.selected(),
        );
        let used = indent + wl.text.width();
        line.spans
            .push(Span::styled(padded_tail(ctx.full_width, used), pad));
        ctx.paint_text_row(line, mi, block_idx, &block_wl, indent as u16, &[]);
    }
}

/// Render a `search_text` result by grouping matches under their file path. Each
/// new path is printed once as a bold `heading_fg` header row; each match
/// is shown as `{lineno}  {content}` with the line number dimmed and the
/// line-number column aligned across the whole result. Non-match lines
/// (ripgrep block separators, etc.) fall back to a dimmed plain row.
/// Selection byte ranges are anchored in the original tool output so
/// copy/cut works across the visible match content.
fn draw_matches_content(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    content: &str,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
) {
    let code_bg = ctx.theme.code_surface();
    let pad = Style::default().bg(code_bg);
    let header_style = Style::default()
        .bg(code_bg)
        .fg(ctx.theme.heading())
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().bg(code_bg).fg(ctx.theme.dim());
    let match_style = Style::default().bg(code_bg).fg(ctx.theme.code_text());
    let sel_range = block_selection_range(selection, mi, block_idx);

    // Walk logical lines with their byte offsets in `content`.
    let mut logical: Vec<(usize, &str)> = Vec::new();
    let mut offset = 0usize;
    for line in content.split('\n') {
        logical.push((offset, line));
        offset += line.len() + 1;
    }

    // Width of the line-number column: the widest lineno across all matches,
    // so the content column stays aligned within and across files.
    let mut lineno_width = 1usize;
    for (_, line) in &logical {
        if let Some(p) = parse_match_line(line) {
            lineno_width = lineno_width.max(p.lineno.len());
        }
    }
    let gap = 2usize;
    let content_cols = indent + lineno_width + gap;
    let content_wrap_w = inner_w.saturating_sub(lineno_width + gap).max(1);

    let mut current_path: Option<&str> = None;

    for (line_start_byte, logical_line) in &logical {
        match parse_match_line(logical_line) {
            Some(parsed) => {
                if current_path != Some(parsed.path) {
                    current_path = Some(parsed.path);
                    emit_simple_rows(
                        ctx,
                        mi,
                        block_idx,
                        indent,
                        parsed.path,
                        *line_start_byte,
                        pad,
                        header_style,
                        sel_range,
                    );
                }
                // Absolute byte offset of `content` within the tool output.
                let content_abs = line_start_byte + parsed.content_offset;
                let wrapped = nonempty_wrapped(wrap_text(parsed.content, content_wrap_w));
                for (wrap_idx, wl) in wrapped.iter().enumerate() {
                    let lineno_span = if wrap_idx == 0 {
                        let lpad = lineno_width.saturating_sub(parsed.lineno.len());
                        Span::styled(format!("{}{}", " ".repeat(lpad), parsed.lineno), dim)
                    } else {
                        Span::styled(" ".repeat(lineno_width), dim)
                    };
                    let block_wl = WrappedLine {
                        text: wl.text.clone(),
                        start_byte: content_abs + wl.start_byte,
                        end_byte: content_abs + wl.end_byte,
                    };
                    let selected = line_selection(sel_range, &block_wl);
                    let mut spans = vec![
                        Span::styled(" ".repeat(indent), pad),
                        lineno_span,
                        Span::styled(" ".repeat(gap), pad),
                    ];
                    match selected {
                        None => spans.push(Span::styled(wl.text.clone(), match_style)),
                        Some((lo, hi)) => {
                            if lo > 0 {
                                spans.push(Span::styled(wl.text[..lo].to_string(), match_style));
                            }
                            spans.push(Span::styled(
                                wl.text[lo..hi].to_string(),
                                match_style.bg(ctx.theme.selected()),
                            ));
                            if hi < wl.text.len() {
                                spans.push(Span::styled(wl.text[hi..].to_string(), match_style));
                            }
                        }
                    }
                    let used = content_cols + wl.text.width();
                    spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
                    ctx.paint_text_row(
                        Line::from(spans),
                        mi,
                        block_idx,
                        &block_wl,
                        content_cols as u16,
                        &[],
                    );
                }
            }
            None => {
                emit_simple_rows(
                    ctx,
                    mi,
                    block_idx,
                    indent,
                    logical_line,
                    *line_start_byte,
                    pad,
                    dim,
                    sel_range,
                );
            }
        }
    }
}

/// Resolve a shell step's `ShellTermination` into a themed footer
/// `(text, style)`, or `None` for a healthy `Exited` run (which carries no
/// extra marker beyond its optional `exit N` line). The footer explains *why*
/// the command ended and — for the blocked variants — how to retry
/// non-interactively, closing the loop the agent can't close itself. Colors
/// reuse the block-level design contract: `warn()` for the blocked/timeout
/// family, `err()` for cancellation, all on the code surface.
fn termination_footer(
    term: muta_contracts::tool_output::ShellTermination,
    theme: &Theme,
) -> Option<(String, Style)> {
    use muta_contracts::tool_output::ShellTermination as T;
    let bg = theme.code_surface();
    let warn_style = Style::default()
        .bg(bg)
        .fg(theme.warn())
        .add_modifier(Modifier::BOLD);
    let err_style = Style::default()
        .bg(bg)
        .fg(theme.err())
        .add_modifier(Modifier::BOLD);
    match term {
        T::Exited => None,
        T::IdleBlocked => Some((
            "killed by harness: no output within the idle-guard window — likely \
             compiling, pipe-buffered output, or a stdin prompt."
                .to_string(),
            warn_style,
        )),
        T::InteractiveBlocked => Some((
            "interactive command not executed in autonomous mode — \
             pass the credential via a flag or env var and retry."
                .to_string(),
            warn_style,
        )),
        T::Timeout => Some((
            "killed by harness: overall timeout reached (command was still \
             producing output)."
                .to_string(),
            warn_style,
        )),
        T::Cancelled => Some(("command cancelled (interrupted).".to_string(), err_style)),
    }
}

/// Render a `bash` step as a terminal-like `code_bg` block: a `$ command`
/// prompt line first, then stdout / stderr (in `error_fg`) / an exit or
/// truncation footer. Output rows have no line-number gutter. Legacy section
/// markers (`Exit N`, `STDOUT:`, …) are highlighted in `warning` for sessions
/// restored without a structured payload. The command line is not selectable
/// (it's derived from the call, not the output stream); output rows are.
#[allow(clippy::too_many_arguments)]
fn draw_command_content(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    content: &str,
    structured: Option<&muta_contracts::ToolOutput>,
    command: &str,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
) {
    let result_bg = ctx.theme.code_surface();
    let pad = Style::default().bg(result_bg);
    let base = Style::default().bg(result_bg).fg(ctx.theme.code_text());
    let stderr_style = Style::default().bg(result_bg).fg(ctx.theme.err());
    let marker_style = Style::default()
        .bg(result_bg)
        .fg(ctx.theme.warn())
        .add_modifier(Modifier::BOLD);
    let sel_range = block_selection_range(selection, mi, block_idx);
    let wrap_w = inner_w.max(1);

    // `$ command` prompt line(s) — the command may span multiple lines; only
    // the first rendered row carries the `$ ` prompt.
    if !command.is_empty() {
        let cmd_style = Style::default().bg(result_bg).fg(ctx.theme.fg());
        let mut rows = command.split('\n');
        if let Some(first) = rows.next() {
            let prompt = format!("$ {}", first);
            for wl in nonempty_wrapped(wrap_text(&prompt, wrap_w)) {
                let used = indent + wl.text.width();
                let line = Line::from(vec![
                    Span::styled(" ".repeat(indent), pad),
                    Span::styled(wl.text.clone(), cmd_style),
                    Span::styled(padded_tail(ctx.full_width, used), pad),
                ]);
                let _ = ctx.paint(line);
            }
        }
        for cont in rows {
            for wl in nonempty_wrapped(wrap_text(cont, wrap_w)) {
                let used = indent + wl.text.width();
                let line = Line::from(vec![
                    Span::styled(" ".repeat(indent), pad),
                    Span::styled(wl.text.clone(), cmd_style),
                    Span::styled(padded_tail(ctx.full_width, used), pad),
                ]);
                let _ = ctx.paint(line);
            }
        }
    }

    if let Some(muta_contracts::ToolOutput::Shell {
        stdout,
        stderr,
        lines,
        exit,
        truncated,
        termination,
        ..
    }) = structured
    {
        // Materialize the output stream once, then emit it through the folding
        // emitter: a head of leading context, a `⋯ N lines hidden` row for the
        // verbose middle, and a tail of trailing context. Only output goes
        // through the fold — the exit / truncated / termination footers below
        // stay outside it, so the trailing "events" are always visible even for
        // a huge log. Short output (≤ HEAD + TAIL + 1 lines) renders verbatim;
        // `emit_command_lines_folded` is a no-op on an empty stream.
        let mut byte_offset = 0usize;
        let output_rows = command_structured_lines(lines, stdout, stderr, base, stderr_style);
        if !output_rows.is_empty() {
            byte_offset = emit_command_lines_folded(
                ctx,
                mi,
                block_idx,
                indent,
                wrap_w,
                pad,
                sel_range,
                &output_rows,
                byte_offset,
            );
        }
        if *truncated {
            byte_offset = emit_command_lines(
                ctx,
                mi,
                block_idx,
                indent,
                wrap_w,
                pad,
                sel_range,
                "[output truncated]",
                marker_style,
                byte_offset,
            );
        }
        // Exit-code footer: always painted when the code is known, so an
        // expanded step closes with a diagnostic fact even on success
        // ("did it actually exit 0?"). A clean `exit 0` is dimmed to stay
        // quiet; any non-zero code keeps the loud warn marker.
        if let Some(code) = exit {
            let m = format!("exit {}", code);
            let style = if *code == 0 {
                Style::default().bg(result_bg).fg(ctx.theme.dim())
            } else {
                marker_style
            };
            let _ = emit_command_lines(
                ctx,
                mi,
                block_idx,
                indent,
                wrap_w,
                pad,
                sel_range,
                &m,
                style,
                byte_offset,
            );
        }

        // ── Themed termination footer (L6) ──
        // Every non-trivial termination renders a themed footer so the user
        // and the model see *why* the command ended, not just that it did.
        // A healthy `Exited` run is silent here (its exit code is above);
        // every other variant paints a coloured marker + a
        // remediation hint. All colors flow through the shared theme tokens,
        // reusing the block-level design contract (diff tokens, warn/err)
        // so the footer reads as part of the same surface language.
        if let Some(footer) = termination_footer(*termination, ctx.theme) {
            let (text, style) = footer;
            let _ = emit_command_lines(
                ctx,
                mi,
                block_idx,
                indent,
                wrap_w,
                pad,
                sel_range,
                &text,
                style,
                byte_offset,
            );
        }
        return;
    }

    // Legacy fallback for non-Shell results (e.g. restored sessions whose
    // structured payload was not persisted): render the composed `content`
    // string, highlighting the conventional section markers. This path does
    // *not* middle-fold: the legacy `content` string inlines its markers
    // (`Exit N`, `STDOUT:`, `[Output truncated` …) at arbitrary positions, so a
    // head/tail window could hide a trailing event marker. Restored sessions
    // are also the rare case — the live structured `Shell` path is what every
    // fresh bash call takes, and it folds above. Folding here is low value and
    // high risk, so the full content is rendered verbatim.
    let content = content.trim_end_matches(&['\r', '\n'][..]);
    if content.is_empty() {
        return;
    }
    let mut logical_lines: Vec<(usize, &str)> = Vec::new();
    let mut offset = 0usize;
    for line in content.split('\n') {
        logical_lines.push((offset, line));
        offset += line.len() + 1;
    }
    for (line_start_byte, logical_line) in logical_lines.iter() {
        let trimmed = logical_line.trim_end();
        let is_marker = trimmed.starts_with("Exit ")
            || trimmed == "STDOUT:"
            || trimmed == "STDERR:"
            || trimmed.starts_with("(success, stderr):")
            || trimmed.starts_with("[Output truncated")
            || trimmed.starts_with("[Output was large")
            || trimmed.starts_with("[killed by harness")
            || trimmed.starts_with("[not executed");
        let style = if is_marker { marker_style } else { base };
        let _ = emit_command_lines(
            ctx,
            mi,
            block_idx,
            indent,
            wrap_w,
            pad,
            sel_range,
            logical_line,
            style,
            *line_start_byte,
        );
    }
}

/// Emit a (possibly multi-line) bash body section at `indent`, wrapping to
/// `wrap_w`, all rows in `style`, anchoring selection byte ranges at
/// `*byte_offset` (advanced past the section). Shared by the structured and
/// legacy bash renderers.
#[allow(clippy::too_many_arguments)]
fn emit_command_lines(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    indent: usize,
    wrap_w: usize,
    pad: Style,
    sel_range: Option<(usize, Option<usize>)>,
    text: &str,
    style: Style,
    mut byte_offset: usize,
) -> usize {
    // Shell capture appends a `\n` after every emitted line, so a payload like
    // `date`'s stdout (`"Fri … 2026\n"`) would otherwise split into
    // `["Fri … 2026", ""]` and paint a phantom trailing blank row (padded with
    // spaces). Trim trailing newlines first; internal blank lines are
    // preserved. This is a no-op for the single-line marker/legacy callers,
    // whose strings never carry a trailing newline.
    let text = text.trim_end_matches(&['\r', '\n'][..]);
    for logical_line in text.split('\n') {
        // Carriage-return / backspace normalization: capture already resolves
        // these (a `\r`-refreshed progress bar collapses to its final frame),
        // but the legacy / restored-session flat-string path can still carry
        // raw `\r`s, so resolve them here too — with the *same* function the
        // capture layer uses, so both paths agree instead of the renderer
        // doing a cruder "keep only the last segment" approximation.
        let logical_line = muta_contracts::tool_output::normalize_carriage_returns(logical_line);
        let wrapped = nonempty_wrapped(wrap_text(&logical_line, wrap_w));
        for wl in &wrapped {
            let block_wl = WrappedLine {
                text: wl.text.clone(),
                start_byte: byte_offset + wl.start_byte,
                end_byte: byte_offset + wl.end_byte,
            };
            let mut line = line_spans(
                &" ".repeat(indent),
                pad,
                &wl.text,
                line_selection(sel_range, &block_wl),
                style,
                ctx.theme.selected(),
            );
            let used = indent + wl.text.width();
            line.spans
                .push(Span::styled(padded_tail(ctx.full_width, used), pad));
            ctx.paint_text_row(line, mi, block_idx, &block_wl, indent as u16, &[]);
        }
        byte_offset += logical_line.len() + 1;
    }
    byte_offset
}

/// Materialize a structured `Shell` result's output stream into an ordered
/// list of `(text, style)` logical lines, in the same byte-offset layout
/// [`emit_command_lines`] uses (one logical line per entry; the caller anchors
/// them sequentially). Each entry carries its per-stream style so a folded
/// view still colours stdout/stderr correctly even when the middle is dropped.
///
/// Prefers the arrival-ordered `lines` (the TUI-authoritative interleaved
/// view), falling back to the all-stdout-then-all-stderr flat strings for the
/// legacy / live-seed / restored-session path. Empty bands contribute nothing.
fn command_structured_lines(
    lines: &[muta_contracts::tool_output::ShellLine],
    stdout: &str,
    stderr: &str,
    base: Style,
    stderr_style: Style,
) -> Vec<(String, Style)> {
    use muta_contracts::tool_output::ShellStream;
    let mut out: Vec<(String, Style)> = Vec::new();
    if !lines.is_empty() {
        for l in lines {
            let style = if l.stream == ShellStream::Err {
                stderr_style
            } else {
                base
            };
            // `emit_command_lines` normalizes CR/BS itself, so pass the raw text.
            out.push((l.text.clone(), style));
        }
        return out;
    }
    // Legacy / live-seed fallback: all-stdout band then all-stderr band.
    for (text, style) in [(stdout, base), (stderr, stderr_style)] {
        let text = text.trim_end_matches(&['\r', '\n'][..]);
        if text.is_empty() {
            continue;
        }
        for line in text.split('\n') {
            out.push((line.to_string(), style));
        }
    }
    out
}

/// Render `rows` (logical `(text, style)` lines) at `indent`, folding the
/// verbose middle into a single dim `⋯ N lines hidden` row when there are
/// more than `BASH_FOLD_HEAD_ROWS + BASH_FOLD_TAIL_ROWS + 1` lines. Visible
/// rows are registered for selection at their true `output`-space byte
/// offsets: `byte_offset` advances past *every* logical line (including the
/// hidden ones) so the tail rows anchor correctly, exactly as the unfolded
/// path would. The synthesized ellipsis row is not selectable (it is a
/// summary, not real content).
///
/// This preserves the contract that a selection spanning the fold copies the
/// visible head and tail text only — the hidden middle is neither painted nor
/// selectable, matching "you can't select what isn't on screen."
#[allow(clippy::too_many_arguments)]
fn emit_command_lines_folded(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    indent: usize,
    wrap_w: usize,
    pad: Style,
    sel_range: Option<(usize, Option<usize>)>,
    rows: &[(String, Style)],
    mut byte_offset: usize,
) -> usize {
    let total = rows.len();
    let fold_after = BASH_FOLD_HEAD_ROWS + BASH_FOLD_TAIL_ROWS + 1;
    if total <= fold_after {
        for (text, style) in rows {
            byte_offset = emit_command_lines(
                ctx,
                mi,
                block_idx,
                indent,
                wrap_w,
                pad,
                sel_range,
                text,
                *style,
                byte_offset,
            );
        }
        return byte_offset;
    }

    // Window: first HEAD rows, one ellipsis row, last TAIL rows.
    let head_end = BASH_FOLD_HEAD_ROWS;
    let tail_start = total - BASH_FOLD_TAIL_ROWS;
    let hidden = tail_start - head_end;

    // ── head ──
    for (text, style) in rows[..head_end].iter() {
        byte_offset = emit_command_lines(
            ctx,
            mi,
            block_idx,
            indent,
            wrap_w,
            pad,
            sel_range,
            text,
            *style,
            byte_offset,
        );
    }

    // ── ellipsis ──
    // Advance `byte_offset` past every hidden logical line so the tail rows
    // anchor at their true `output`-space positions. Each hidden line occupies
    // `text.len() + 1` bytes in the flat stream (the `+1` is the `\n`
    // separator `emit_command_lines` counts). `normalize_carriage_returns` can
    // only shrink a line, so this upper bound keeps offsets monotonic; the
    // tail offsets stay past the head, which is all selection anchoring needs.
    for (text, _style) in rows[head_end..tail_start].iter() {
        byte_offset += text.len() + 1;
    }
    let ellipsis_text = format!("⋯ {} lines hidden", hidden);
    let used = indent + ellipsis_text.width();
    let ellipsis_line = Line::from(vec![
        Span::styled(" ".repeat(indent), pad),
        Span::styled(ellipsis_text, pad.fg(ctx.theme.dim())),
        Span::styled(padded_tail(ctx.full_width, used), pad),
    ]);
    let _ = ctx.paint(ellipsis_line); // summary row: not registered for selection

    // ── tail ──
    for (text, style) in rows[tail_start..].iter() {
        byte_offset = emit_command_lines(
            ctx,
            mi,
            block_idx,
            indent,
            wrap_w,
            pad,
            sel_range,
            text,
            *style,
            byte_offset,
        );
    }

    byte_offset
}

/// Render an expanded tool step's content — no `Result`/`Diff` label, no
/// separator; just the tool-specific block dispatched by `result_kind`. Known
/// tools with structured output get a specialized renderer; everything else
/// falls back to a line-numbered code block via [`draw_code_content`]. `bash`
/// additionally prefixes the block with a `$ command` line so the whole step
/// reads like a terminal session.
#[allow(clippy::too_many_arguments)]
fn draw_tool_result(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    message_id: u64,
    name: &str,
    arguments: &str,
    output: &str,
    structured: Option<&muta_contracts::ToolOutput>,
    diff_cache: &mut DiffCache,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
) {
    let block_idx = 1usize;
    // Explicit tool errors are terminal results, not the successful shape the
    // presenter normally renders. In particular, a failed edit has no Patch;
    // deriving a diff from its call arguments would show an intended change
    // that never reached disk and hide the actionable failure message.
    if matches!(
        structured,
        Some(
            muta_contracts::ToolOutput::Error { .. }
                | muta_contracts::ToolOutput::PermissionDenied { .. }
        )
    ) {
        draw_tool_error(ctx, mi, block_idx, output, selection, indent, inner_w);
        return;
    }

    let kind = crate::tools::presenter_for(name).result_kind();
    match kind {
        ResultKind::Listing => {
            draw_listing_content(ctx, mi, block_idx, output, selection, indent, inner_w)
        }
        ResultKind::Matches => {
            draw_matches_content(ctx, mi, block_idx, output, selection, indent, inner_w)
        }
        ResultKind::Command => {
            let command = command_for(structured, arguments);
            draw_command_content(
                ctx, mi, block_idx, output, structured, &command, selection, indent, inner_w,
            );
        }
        ResultKind::Code => {
            // Prefer the structured payload: `Code::text` is pure file content
            // (the model-facing `prefix`/`suffix` framing is ignored here) and
            // `start_line` carries the read `offset` so an offset snippet
            // numbers from its true file line. `lang` is surfaced as a
            // language-tag line so the block matches the markdown code band.
            // `Patch::new` handles the `write_file` case: a full-file write
            // rendered as a simple code block with line numbers (no diff
            // gutter — there is no "old" side).
            // Legacy/restored steps without a payload fall back to the
            // flattened `output` string with `start_line = 0` (slice-relative
            // 1-based numbering).
            let (content, start_line, lang) = match structured {
                Some(muta_contracts::ToolOutput::Code {
                    text,
                    start_line,
                    lang,
                    ..
                }) => (text.as_str(), *start_line, lang.as_deref()),
                Some(muta_contracts::ToolOutput::Patch {
                    new, start_line, ..
                }) => (new.as_str(), *start_line, None),
                _ => (output, 0, None),
            };
            draw_code_content(
                ctx, mi, block_idx, content, start_line, lang, selection, indent, inner_w,
            )
        }
        ResultKind::Diff => {
            // Prefer the structured Patch payload (old/new from the result);
            // fall back to argument-derived rows only for legacy/restored
            // completed steps. Both paths are cached by stable message id and
            // exact source, so animation frames never repeat Myers/word diffing.
            let hunks = match structured {
                Some(muta_contracts::ToolOutput::Patch {
                    old,
                    new,
                    start_line,
                    ..
                }) => diff_cache.patch(message_id, old, new, *start_line),
                _ => diff_cache.legacy_arguments(message_id, name, arguments),
            };
            draw_diff_content(ctx, hunks.as_ref(), indent, inner_w);
        }
        ResultKind::Checklist => {
            draw_checklist_content(
                ctx, mi, block_idx, output, arguments, selection, indent, inner_w,
            );
        }
    }
}

/// Render an explicit tool failure as error text on the shared code surface.
/// It deliberately has no line-number gutter: these rows describe the failed
/// operation and are not source-file content.
fn draw_tool_error(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    block_idx: usize,
    output: &str,
    selection: &SelectionState,
    indent: usize,
    inner_w: usize,
) {
    let bg = ctx.theme.code_surface();
    let pad = Style::default().bg(bg);
    let error = Style::default()
        .bg(bg)
        .fg(ctx.theme.err())
        .add_modifier(Modifier::BOLD);
    let selection = block_selection_range(selection, mi, block_idx);
    let _ = emit_command_lines(
        ctx,
        mi,
        block_idx,
        indent,
        inner_w.max(1),
        pad,
        selection,
        output,
        error,
        0,
    );
}

/// Resolve the shell command for a `bash` step: prefer the structured
/// [`ToolOutput::Shell`](muta_contracts::ToolOutput) payload (set as soon as the
/// call starts, so it is available even while streaming), falling back to
/// parsing the JSON arguments for legacy / restored sessions without a
/// structured payload.
fn command_for(structured: Option<&muta_contracts::ToolOutput>, arguments: &str) -> String {
    if let Some(muta_contracts::ToolOutput::Shell { command, .. }) = structured
        && !command.is_empty()
    {
        return command.clone();
    }
    crate::model::document::parse_arguments_kv(arguments)
        .iter()
        .find(|(k, _)| k == "command")
        .map(|(_, v)| v.clone())
        .unwrap_or_default()
}

/// Render explicit Git-style hunks inside an expanded edit step. Every hunk
/// starts with its authoritative `@@ -N,M +P,Q @@` header, followed by rows
/// with dual old/new line-number gutters and colored change signs. Hunk
/// grouping and ranges are derived before rendering; this function only
/// paints them and never infers source semantics from presentation rows.
///
/// Every logical row goes through [`RenderCtx::paint`], which counts it in
/// `content_lines` even when scroll-skip or viewport clip keeps it off
/// screen. Undercounting here would make the measured step height (and thus
/// `max_scroll`) depend on the scroll position, feeding back as jumpy scroll
/// and flicker while the transcript animates.
fn draw_diff_content(
    ctx: &mut RenderCtx<'_, '_>,
    hunks: &[DiffHunk],
    indent: usize,
    inner_w: usize,
) {
    if hunks.is_empty() {
        return;
    }
    let code_bg = ctx.theme.code_surface();
    let gutter_fg = ctx.theme.muted();
    // Each number column is at least 2 chars wide so single-digit files
    // align cleanly (GitHub-style: right-aligned old_no | new_no).
    let max_no = hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter_map(|line| line.old_no.or(line.new_no))
        .max()
        .unwrap_or(0);
    let gutter_w = max_no.to_string().len().max(2);
    let sign_w = 2usize; // "+ " / "- " / "  "
    // Dual gutter: old_no(right, gutter_w) + " " + new_no(right, gutter_w).
    let gutter_cols = 2 * gutter_w + 1;
    let text_w = inner_w.saturating_sub(gutter_cols + sign_w).max(1);
    // opencode-style banding: the whole row carries a low-chroma tint so
    // added/removed blocks read at a glance, and the exact edited word sits
    // on a brighter tint on top of the row band. Context rows stay on the
    // neutral code surface so they recede. All four tints are first-class
    // theme tokens (the block-level design contract) — no inline literals —
    // so retuning the palette here retunes every block-level surface.
    let add_row_bg = ctx.theme.diff_add_bg();
    let del_row_bg = ctx.theme.diff_del_bg();
    let add_hi_bg = ctx.theme.diff_add_hl();
    let del_hi_bg = ctx.theme.diff_del_hl();
    let info_fg = ctx.theme.info();

    for hunk in hunks {
        let hunk_header = hunk.header();
        {
            let pad = Style::default().bg(code_bg);
            let hh_len = hunk_header.len();
            let mut spans: Vec<Span<'static>> = vec![
                Span::styled(" ".repeat(indent), pad),
                Span::styled(" ".repeat(gutter_cols), pad),
                Span::styled("  ", Style::default().bg(code_bg)),
                Span::styled(hunk_header, Style::default().bg(code_bg).fg(info_fg)),
            ];
            let used = indent + gutter_cols + sign_w + hh_len;
            spans.push(Span::styled(
                padded_tail(ctx.full_width, used),
                Style::default().bg(code_bg),
            ));

            ctx.paint(Line::from(spans));
        }

        for line in &hunk.lines {
            let (sign, row_bg, base_fg, hi_bg) = match line.op {
                DiffOp::Add => ('+', add_row_bg, ctx.theme.ok(), add_hi_bg),
                DiffOp::Remove => ('-', del_row_bg, ctx.theme.err(), del_hi_bg),
                DiffOp::Context => (' ', code_bg, ctx.theme.muted(), code_bg),
            };
            let pad = Style::default().bg(row_bg);

            let full = line.text();
            let wrapped = nonempty_wrapped(wrap_text(&full, text_w));
            let highlight_frags = wrapped.len() <= 1;

            let (first_old, first_new) = match line.op {
                DiffOp::Context => (fmt_no(line.old_no, gutter_w), fmt_no(line.new_no, gutter_w)),
                DiffOp::Remove => (fmt_no(line.old_no, gutter_w), fmt_no(None, gutter_w)),
                DiffOp::Add => (fmt_no(None, gutter_w), fmt_no(line.new_no, gutter_w)),
            };
            let blank_col = fmt_no(None, gutter_w);

            for (i, wl) in wrapped.iter().enumerate() {
                let is_cont = i > 0;
                let (old_col, new_col) = if is_cont {
                    (&blank_col, &blank_col)
                } else {
                    (&first_old, &first_new)
                };
                let sign_text = if is_cont {
                    "  "
                } else {
                    match sign {
                        '+' => "+ ",
                        '-' => "- ",
                        _ => "  ",
                    }
                };
                let mut spans: Vec<Span<'static>> = vec![
                    Span::styled(" ".repeat(indent), pad),
                    Span::styled(old_col.clone(), Style::default().bg(row_bg).fg(gutter_fg)),
                    Span::styled(" ", Style::default().bg(row_bg)),
                    Span::styled(new_col.clone(), Style::default().bg(row_bg).fg(gutter_fg)),
                    Span::styled(
                        sign_text,
                        Style::default()
                            .bg(row_bg)
                            .fg(base_fg)
                            .add_modifier(Modifier::BOLD),
                    ),
                ];
                if highlight_frags && !is_cont {
                    for frag in &line.frags {
                        let style = if frag.changed {
                            Style::default()
                                .bg(hi_bg)
                                .fg(base_fg)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().bg(row_bg).fg(base_fg)
                        };
                        let frag_text = frag.text.trim_end_matches('\n');
                        spans.push(Span::styled(frag_text.to_string(), style));
                    }
                } else {
                    spans.push(Span::styled(
                        wl.text.clone(),
                        Style::default().bg(row_bg).fg(base_fg),
                    ));
                }
                let used = indent + gutter_cols + sign_w + wl.text.width();
                spans.push(Span::styled(padded_tail(ctx.full_width, used), pad));
                ctx.paint(Line::from(spans));
            }
        }
    }
}

/// Format an optional line number as a right-aligned, `width`-wide string.
/// `None` yields `width` spaces.
fn fmt_no(no: Option<usize>, width: usize) -> String {
    match no {
        Some(n) => format!("{:>width$}", n, width = width),
        None => format!("{:>width$}", "", width = width),
    }
}

/// Render an runner `task` tool step as a compact, non-expandable step.
/// Activating it (click / Enter) navigates into a dedicated runner view
/// rather than expanding a body inline. The step is exactly two rows: a
/// summary line carrying the `[profile]` role badge + task description, and
/// a single `└`-edged second row that shows the live "peek" (current
/// activity + elapsed) while running and is replaced in place by the
/// one-line conclusion when the runner terminates.
pub fn draw_runner_inline_step(
    ctx: &mut RenderCtx<'_, '_>,
    msg: &TranscriptMessage,
    mi: usize,
    hovered: bool,
    focused: bool,
) {
    let _theme = ctx.theme;
    let _transcript_area = ctx.area;

    let Some(summary) = msg.tool_step_summary() else {
        return;
    };

    let status = msg
        .tool_step_status()
        .map(ToolStatus::from_status)
        .unwrap_or(ToolStatus::Running);

    // `ctx.area` arrives already inset by `draw_transcript`, so no
    // re-clip is needed here.
    let full_width = ctx.area.width as usize;
    if full_width < STEP_MIN_WIDTH {
        return;
    }

    let bg = ctx.theme.surface();

    // Summary color flows through the shared state machine exactly like a
    // tool step (steady lifecycle accent while non-terminal, weight ladder
    // once completed); the badge borrows the brand hue so the role reads as
    // identity rather than as run state.
    let status_color = status.color(ctx.theme);
    let accent = match status {
        ToolStatus::Ok => None,
        _ => Some(status_color),
    };
    let summary_color = summary_text_color(
        accent,
        Disclosure::Collapsed,
        Interaction::from_hover_focused(hovered, focused),
        ctx.theme,
    );

    // Row 1: `[badge]  summary`. The badge is a plain bracketed token in the
    // brand color (no inverse pill) so it sits quietly next to the summary
    // and survives narrow terminals. Two plain spaces separate badge and
    // summary (R2 same-rank peers on the join ladder).
    let badge = msg
        .runner_profile()
        .map(|role| format!("[{}]", role.to_uppercase()))
        .unwrap_or_else(|| "[RUNNER]".to_string());
    let summary_row = {
        let base = Style::default().bg(bg);
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(4);
        let mut used = 0usize;
        let badge_text = format!("  {}", badge);
        used += badge_text.width();
        spans.push(Span::styled(badge_text, base.fg((*ctx.theme).brand())));
        let sep = "  ";
        used += sep.len();
        spans.push(Span::styled(sep, base));
        let summary_budget = full_width.saturating_sub(used);
        let clamped = truncate_to_width(&summary, summary_budget);
        used += clamped.width();
        spans.push(Span::styled(
            clamped,
            base.fg(summary_color).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(padded_tail(full_width, used), base));
        Line::from(spans)
    };
    if let Some(rect) = ctx.paint(summary_row) {
        ctx.layout_map.push(BlockRegion {
            message_idx: mi,
            block_idx: usize::MAX,
            start_byte: 0,
            end_byte: 0,
            text: String::new(),
            prefix_cols: 0,
            rect,
            hidden_ranges: Vec::new(),
        });
    }

    // Row 2: the `└`-edged live row. While running it carries the peek
    // (current activity + elapsed); once the runner terminates it is replaced
    // in place by the one-line conclusion. The `└` is constant — the step is
    // always exactly two rows, so the second row is always the leaf — and the
    // running/terminated distinction is carried by color and content, not by
    // the glyph.
    let (row_text, row_color) = if let Some(peek) = msg.runner_status_line() {
        (peek, (*ctx.theme).info())
    } else if let Some(outcome) = msg.runner_outcome_line() {
        (outcome, ctx.theme.muted())
    } else {
        (String::new(), ctx.theme.muted())
    };
    if !row_text.is_empty() {
        let bg_style = Style::default().bg(bg);
        let inner_width = ctx.full_width.saturating_sub(4);
        let wrapped = wrap_text(&row_text, inner_width.max(1));
        for (i, wl) in wrapped.iter().enumerate() {
            // The `└` edge marks the first wrapped row; continuation rows fall
            // back to a plain indent so the tree edge reads as a single branch.
            let (prefix, prefix_w) = if i == 0 { ("  └ ", 4) } else { ("    ", 4) };
            let used = prefix_w + wl.text.width();
            let line = Line::from(vec![
                Span::styled(prefix, bg_style.fg(ctx.theme.muted())),
                Span::styled(wl.text.clone(), bg_style.fg(row_color)),
                Span::styled(padded_tail(ctx.full_width, used), bg_style),
            ]);
            // Make the whole second row part of the same clickable summary so
            // clicking anywhere on the step enters the runner view.
            if let Some(rect) = ctx.paint(line) {
                ctx.layout_map.push(BlockRegion {
                    message_idx: mi,
                    block_idx: usize::MAX,
                    start_byte: 0,
                    end_byte: 0,
                    text: String::new(),
                    prefix_cols: 0,
                    rect,
                    hidden_ranges: Vec::new(),
                });
            }
        }
    }
}

/// Render a tool-step message as an expandable step with a summary line,
/// a body, and per-line scroll handling so tall steps scroll like
/// normal messages.
pub fn draw_tool_step(
    ctx: &mut RenderCtx<'_, '_>,
    msg: &TranscriptMessage,
    mi: usize,
    selection: &SelectionState,
    cell_selection: Option<&CellDragInfo>,
    diff_cache: &mut DiffCache,
    sticky_steps: &mut Vec<StickyStep>,
    hovered: bool,
    focused: bool,
) {
    let _theme = ctx.theme;
    let _transcript_area = ctx.area;

    let Some(summary) = msg.tool_step_summary() else {
        return;
    };
    let expanded = msg.tool_step_expanded() == Some(true);

    // Run state is conveyed by color alone: muted while running, red on
    // failure, dim when cancelled, and weight-only on success.
    // There is no status glyph or per-tool icon in the summary. The summary
    // text color is resolved through the shared state machine: a non-completed
    // lifecycle supplies an accent that supplies the hue while the disclosure ×
    // interaction weight channel modulates its brightness; the completed case
    // yields no accent and falls fully through to the weight ladder so a
    // finished call reads as calm when idle — bright (primary foreground) while
    // its body is open, the hover tone while focused or under the pointer, and
    // muted when collapsed and idle.
    //
    // The activity bar is the single breathing anchor (ADR 0008); per-step
    // liveness rides on hue alone so a transcript full of running steps does
    // not flash in unison and steal attention from the content the user is
    // reading.
    let status = msg
        .tool_step_status()
        .map(ToolStatus::from_status)
        .unwrap_or(ToolStatus::Running);
    // Tool steps render flat on the app background (no band) — like
    // reasoning traces, only the optional content block carries a `code_bg`.
    let summary_bg = ctx.theme.surface();
    let status_color = status.color(ctx.theme);
    let accent = match status {
        ToolStatus::Ok => None,
        _ => Some(status_color),
    };
    let summary_color = summary_text_color(
        accent,
        Disclosure::from_expanded(expanded),
        Interaction::from_hover_focused(hovered, focused),
        ctx.theme,
    );

    // `ctx.area` arrives already inset by `draw_transcript` (the
    // uniform horizontal gutters are applied once at the stream entry point),
    // so all helpers below read `ctx.area.x` / `.width` directly.
    let full_width = ctx.area.width as usize;
    if full_width < STEP_MIN_WIDTH {
        // Too narrow to draw; fall back to plain block rendering.
        draw_message_body(
            &mut *ctx.frame,
            ctx.area,
            msg,
            mi,
            selection,
            cell_selection,
            ctx.theme,
            &mut *ctx.layout_map,
            &mut *ctx.skip_rows,
            ctx.y,
            &mut *ctx.content_lines,
            true,
        );
        return;
    }

    let inner_width = ctx.area.width as usize;
    let summary_line_idx = {
        draw_step_summary(
            ctx,
            mi,
            usize::MAX,
            expanded,
            &summary,
            summary_color,
            summary_bg,
        )
    };

    // Body region (only when expanded). Tool steps are flat — no band, no
    // Tool/Arguments/Result labels — so an expanded step reads like a log entry:
    // the tool-specific content directly under the summary (bash → `$ cmd` +
    // output; list/search → entries; edit/write → diff; read → code), indented to
    // align with prose. Only content blocks carry a `code_bg`; everything else
    // sits on the app background.
    if expanded {
        let surface = ctx.theme.surface();
        let pad = Style::default().bg(surface);
        let indent = TOOL_STEP_BODY_INDENT_COLS;
        let inner_w = inner_width.saturating_sub(indent);

        {
            draw_blank_rows(ctx, pad, TOOL_STEP_BODY_TOP_GAP_ROWS);

            if let crate::model::document::MessageKind::ToolStep {
                name,
                arguments,
                output,
                structured,
                ..
            } = &msg.kind
            {
                // Unknown / MCP tools spell out their arguments as `key: value`
                // rows (the summary only carries the primary one). No label — the
                // key names are self-describing, and the result block below
                // carries its own `code_bg` so the two stay visually distinct.
                if matches!(
                    crate::tools::presenter_for(name).arg_layout(),
                    ArgLayout::KeyValue
                ) {
                    let kv = crate::model::document::parse_arguments_kv(arguments);
                    if !kv.is_empty() {
                        let kv_style = Style::default().bg(surface).fg(ctx.theme.muted());
                        let wrap_w = inner_w.max(1);
                        for (k, v) in &kv {
                            let row = format!("{}: {}", k, v);
                            for wl in nonempty_wrapped(wrap_text(&row, wrap_w)) {
                                let used = indent + wl.text.width();
                                let line = Line::from(vec![
                                    Span::styled(" ".repeat(indent), pad),
                                    Span::styled(wl.text.clone(), kv_style),
                                    Span::styled(padded_tail(ctx.full_width, used), pad),
                                ]);
                                let _ = ctx.paint(line);
                            }
                        }
                    }
                }

                // Tool-specific content (label-free). bash renders `$ cmd` +
                // output; others their block. A streaming or freshly-spawned command
                // step renders its `$ cmd` and live streaming output.
                let has_output = output.as_deref().is_some_and(|s| !s.is_empty());
                let is_command =
                    matches!(name.as_str(), "run_command" | "execute_command" | "bash");
                let has_structured = structured.is_some();
                if has_output || is_command || has_structured {
                    draw_tool_result(
                        ctx,
                        mi,
                        msg.id,
                        name,
                        arguments,
                        output.as_deref().unwrap_or(""),
                        structured.as_deref(),
                        diff_cache,
                        selection,
                        indent,
                        inner_w,
                    );
                }
            }
        }

        // ── Nested runner children ──.
        if let crate::model::document::MessageKind::ToolStep { children, .. } = &msg.kind {
            if !children.is_empty() {
                draw_blank_rows(ctx, pad, TOOL_STEP_CHILDREN_GAP_ROWS);
            }
            for child in children {
                if child.is_tool_step() {
                    draw_child_tool_step(ctx, child, status_color);
                } else {
                    let remaining_height = ctx
                        .area
                        .y
                        .saturating_add(ctx.area.height)
                        .saturating_sub(*ctx.y);
                    let child_area = Rect::new(
                        ctx.area.x + 6,
                        *ctx.y,
                        ctx.area.width.saturating_sub(12),
                        remaining_height,
                    );
                    draw_message_body(
                        &mut *ctx.frame,
                        child_area,
                        child,
                        usize::MAX,
                        selection,
                        cell_selection,
                        ctx.theme,
                        &mut *ctx.layout_map,
                        &mut *ctx.skip_rows,
                        ctx.y,
                        &mut *ctx.content_lines,
                        false,
                    );
                }
            }
        }

        // No trailing bottom gap here: the layout resolves the semantic
        // boundary to the next component. Same-turn tool siblings use zero
        // rows; every other segment uses one, independent of disclosure state.
    }

    if expanded {
        sticky_steps.push(StickyStep {
            message_idx: mi,
            summary,
            color: status_color,
            background: Some(ctx.theme.surface()),
            summary_line: summary_line_idx,
            body_end_line: *ctx.content_lines,
        });
    }
}

/// Render a nested child tool step as a compact summary line plus its output.
fn draw_child_tool_step(
    ctx: &mut RenderCtx<'_, '_>,
    child: &TranscriptMessage,
    status_color: Color,
) {
    let Some(summary) = child.tool_step_summary() else {
        return;
    };
    let surface = ctx.theme.surface();
    let full_width = ctx.full_width;
    let indent = 6usize;
    let bg_style = Style::default().bg(surface);

    let summary_text = summary.to_string();
    let summary_lines = wrap_text(&summary_text, full_width.saturating_sub(indent));
    for wl in &summary_lines {
        let used = indent + wl.text.width();
        let line = Line::from(vec![
            Span::styled(" ".repeat(indent), bg_style),
            Span::styled(wl.text.clone(), bg_style.fg(status_color)),
            Span::styled(padded_tail(full_width, used), bg_style),
        ]);
        let _ = ctx.paint(line);
    }

    if let crate::model::document::MessageKind::ToolStep {
        output: Some(output),
        ..
    } = &child.kind
    {
        let output_lines = wrap_text(output, full_width.saturating_sub(indent + 1));
        for wl in &output_lines {
            let used = indent + wl.text.width();
            let line = Line::from(vec![
                Span::styled(" ".repeat(indent), bg_style),
                Span::styled(wl.text.clone(), bg_style.fg(ctx.theme.fg())),
                Span::styled(padded_tail(full_width, used), bg_style),
            ]);
            let _ = ctx.paint(line);
        }
    }
}

fn advance_plain_blank_rows(
    transcript_area: Rect,
    rows: usize,
    skip_rows: &mut usize,
    current_y: &mut u16,
    content_lines: &mut usize,
) {
    for _ in 0..rows {
        *content_lines += 1;
        if *skip_rows > 0 {
            *skip_rows = skip_rows.saturating_sub(1);
        } else if *current_y < transcript_area.y + transcript_area.height {
            *current_y += 1;
        }
    }
}

fn reasoning_summary_line(
    marker: &str,
    summary: &str,
    marker_color: Color,
    summary_color: Color,
    full_width: usize,
) -> Line<'static> {
    // The focus affordance is carried entirely by `summary_color` (resolved
    // upstream through `summary_text_color` / `summary_weight`, which maps a
    // focused step to the hover tone), so this builder needs no focus flag of
    // its own.
    //
    // No marker prefix: the horizontal gutter is applied once at the stream
    // entry point, so the marker starts at the area's left edge.
    let marker_text = format!("{} ", marker);
    let mut used = marker_text.width();
    let budget = full_width.saturating_sub(used);
    let summary_text = truncate_to_width(summary, budget);
    used += summary_text.width();
    Line::from(vec![
        Span::styled(
            marker_text,
            Style::default()
                .fg(marker_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            summary_text,
            Style::default()
                .fg(summary_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(padded_tail(full_width, used), Style::default()),
    ])
}

#[allow(clippy::too_many_arguments)]
fn draw_reasoning_summary(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    expanded: bool,
    marker_override: Option<&str>,
    summary: &str,
    hovered: bool,
    focused: bool,
    block_idx: usize,
) -> usize {
    let marker = marker_override.unwrap_or(if expanded {
        MARKER_EXPANDED
    } else {
        MARKER_COLLAPSED
    });
    let summary_line_idx = *ctx.content_lines;
    // A reasoning trace's lifecycle is carried by the summary text (duration
    // omitted while streaming), never by the marker, which is always the
    // disclosure `+`/`-`. So no accent is supplied and the summary color is
    // the pure disclosure × interaction weight from the shared state machine:
    // expanded → primary foreground; collapsed + hovered/focused →
    // intermediate hover tone; collapsed + idle → muted.
    //
    // The marker shares that same color so the disclosure affordance reads as
    // one visual unit with the summary text — matching how tool steps render
    // their marker (a single `fg` for marker + text). Previously the marker
    // was pinned to a fixed `info` hue, which made it read as a detached blue
    // glyph that ignored disclosure/focus state.
    let summary_color = summary_text_color(
        None,
        Disclosure::from_expanded(expanded),
        Interaction::from_hover_focused(hovered, focused),
        ctx.theme,
    );

    let line = reasoning_summary_line(
        marker,
        summary,
        summary_color,
        summary_color,
        ctx.full_width,
    );
    if let Some(rect) = ctx.paint(line) {
        ctx.layout_map.push(BlockRegion {
            message_idx: mi,
            block_idx,
            start_byte: 0,
            end_byte: 0,
            text: String::new(),
            prefix_cols: 0,
            rect,
            hidden_ranges: Vec::new(),
        });
    }

    summary_line_idx
}

/// Render a reasoning trace as expandable prose. It keeps the thinking
/// message model for stream semantics, but presents it as body-aligned text
/// instead of a colored step.
pub fn draw_reasoning_trace(
    ctx: &mut RenderCtx<'_, '_>,
    msg: &TranscriptMessage,
    mi: usize,
    selection: &SelectionState,
    cell_selection: Option<&CellDragInfo>,
    sticky_steps: &mut Vec<StickyStep>,
    hovered: bool,
    focused: bool,
) {
    let _theme = ctx.theme;
    let _transcript_area = ctx.area;

    let Some(summary) = msg.thinking_summary() else {
        return;
    };
    let expanded = msg.thinking_expanded() == Some(true);
    let full_width = ctx.area.width as usize;

    if full_width < (TRANSCRIPT_BODY_LEADING_INDENT as usize + 1) {
        draw_message_body(
            &mut *ctx.frame,
            ctx.area,
            msg,
            mi,
            selection,
            cell_selection,
            ctx.theme,
            &mut *ctx.layout_map,
            &mut *ctx.skip_rows,
            ctx.y,
            &mut *ctx.content_lines,
            true,
        );
        return;
    }

    let summary_line_idx = {
        draw_reasoning_summary(
            ctx,
            mi,
            expanded,
            // Always use the disclosure marker (`+`/`-`), never a streaming
            // `●`. With the activity bar as the single breathing anchor
            // (ADR 0008), nothing about the marker needs to change between
            // streaming and finished — the lifecycle reads from the summary
            // text (duration omitted while streaming) and the steady hue
            // alone. The marker color now follows the disclosure ×
            // interaction weight, so it tracks the highlight like the
            // summary text and like tool-step markers (no fixed hue).
            None,
            &summary,
            hovered,
            focused,
            THINKING_BLOCK_IDX,
        )
    };

    if expanded {
        // The leading indent is all that remains now that the horizontal
        // gutter is applied once at the stream entry point.
        let body_prefix = " ".repeat(TRANSCRIPT_BODY_LEADING_INDENT as usize);
        let body_wrap_width = ctx
            .area
            .width
            .saturating_sub(TRANSCRIPT_BODY_LEADING_INDENT) as usize;

        advance_plain_blank_rows(
            ctx.area,
            REASONING_TRACE_BODY_TOP_GAP_ROWS,
            &mut *ctx.skip_rows,
            ctx.y,
            &mut *ctx.content_lines,
        );
        let mut emitted_any_block = false;
        for (bi, block) in msg.blocks.iter().enumerate() {
            if let Block::Text(inline) = block {
                let Inline {
                    content,
                    code_ranges,
                    bold_ranges,
                    math_ranges,
                    link_ranges,
                } = inline;
                if emitted_any_block {
                    advance_plain_blank_rows(
                        ctx.area,
                        REASONING_TRACE_BLOCK_GAP_ROWS,
                        &mut *ctx.skip_rows,
                        ctx.y,
                        &mut *ctx.content_lines,
                    );
                }
                emitted_any_block = true;
                let lines = wrap_text(content, body_wrap_width);
                let sel_range = block_selection_range(selection, mi, bi);
                for wl in &lines {
                    let block_wl = WrappedLine {
                        text: wl.text.clone(),
                        start_byte: wl.start_byte,
                        end_byte: wl.end_byte,
                    };
                    let line = line_spans_rich(RichLineParams {
                        prefix: &body_prefix,
                        prefix_style: Style::default(),
                        text: &wl.text,
                        line_start_byte: wl.start_byte,
                        selected: line_selection(sel_range, &block_wl),
                        ranges: RichTextRanges {
                            code: code_ranges,
                            bold: bold_ranges,
                            math: math_ranges,
                            links: link_ranges,
                        },
                        base: Style::default().fg(ctx.theme.muted()),
                        colors: RichTextColors::from_theme(ctx.theme),
                    });
                    let used = TRANSCRIPT_BODY_LEADING_INDENT as usize + wl.text.width();
                    let mut line = line;
                    line.spans.push(Span::styled(
                        padded_tail(ctx.full_width, used),
                        Style::default(),
                    ));
                    ctx.paint_text_row(
                        line,
                        mi,
                        bi,
                        &block_wl,
                        TRANSCRIPT_BODY_LEADING_INDENT,
                        &[],
                    );
                }
            }
        }
        // No trailing bottom gap here: the layout resolves the semantic
        // boundary to the next component segment.
    }

    if expanded {
        sticky_steps.push(StickyStep {
            message_idx: mi,
            summary,
            color: ctx.theme.muted(),
            background: None,
            summary_line: summary_line_idx,
            body_end_line: *ctx.content_lines,
        });
    }
}

/// The disclosure marker pair: `+` collapsed, `-` expanded.
pub(crate) const MARKER_COLLAPSED: &str = "+";
pub(crate) const MARKER_EXPANDED: &str = "-";

/// Build the one-row header of a command entry: `⌘ command          21:39`.
/// The `⌘` glyph and `command` label are rendered in the same
/// indicator tone (BOLD), with the muted timestamp pinned to the right edge.
fn command_header_line(
    category_label: &str,
    family_tone: Color,
    time_label: Option<&str>,
    muted: Color,
    full_width: usize,
) -> Line<'static> {
    let mut spans = Vec::with_capacity(4);
    let mut used = 0usize;

    // Indicator tag: `⌘ command` in family_tone + BOLD.
    let tag = format!("⌘ {category_label}");
    used += tag.width();
    spans.push(Span::styled(
        tag,
        Style::default()
            .fg(family_tone)
            .add_modifier(Modifier::BOLD),
    ));

    // Trailing timestamp: whitespace-aligned to the right edge.
    if let Some(time) = time_label {
        let time_width = time.width();
        if used + 1 + time_width <= full_width {
            spans.push(Span::styled(
                " ".repeat(full_width - used - time_width),
                Style::default(),
            ));
            spans.push(Span::styled(time.to_string(), Style::default().fg(muted)));
        }
    }

    Line::from(spans)
}

/// Draw a harness command as a top-level **Entry** owning its input
/// and output (ADR-0111, revising ADR-0109/0108/0106).
///
/// Every command entry is structured identically to a turn entry:
/// - **Header**: `⌘ command` plus a right-aligned `HH:MM` timestamp
/// - **Gap**: 1 blank row separating header and content body (`TURN_HEADER_BODY_GAP_ROWS`)
/// - **Body**: The concrete invocation (e.g. `/delegate on`) followed by result output blocks.
pub fn draw_command_result(
    ctx: &mut RenderCtx<'_, '_>,
    msg: &TranscriptMessage,
    mi: usize,
    selection: &SelectionState,
    cell_selection: Option<&CellDragInfo>,
    _hovered: bool,
    _focused: bool,
) {
    let _theme = ctx.theme;
    let _transcript_area = ctx.area;

    let Some(invocation) = msg.command_result_summary() else {
        return;
    };
    let phase = msg
        .command_result_phase()
        .unwrap_or(CommandPhase::Completed);
    let full_width = ctx.area.width as usize;

    if full_width < (TRANSCRIPT_BODY_LEADING_INDENT as usize + 1) {
        draw_message_body(
            &mut *ctx.frame,
            ctx.area,
            msg,
            mi,
            selection,
            cell_selection,
            ctx.theme,
            &mut *ctx.layout_map,
            &mut *ctx.skip_rows,
            ctx.y,
            &mut *ctx.content_lines,
            true,
        );
        return;
    }

    let time_label = msg.sent_at_ms.map(crate::time::sent_time_label);

    let header_line = command_header_line(
        "command",
        (*ctx.theme).info(),
        time_label.as_deref(),
        ctx.theme.muted(),
        full_width,
    );
    {
        if let Some(rect) = ctx.paint(header_line) {
            ctx.layout_map.push(BlockRegion {
                message_idx: mi,
                block_idx: COMMAND_RESULT_BLOCK_IDX,
                start_byte: 0,
                end_byte: 0,
                text: String::new(),
                prefix_cols: 0,
                rect,
                hidden_ranges: Vec::new(),
            });
        }
    }

    // 1-row blank gap between entry header and body
    advance_plain_blank_rows(
        ctx.area,
        TURN_HEADER_BODY_GAP_ROWS,
        &mut *ctx.skip_rows,
        ctx.y,
        &mut *ctx.content_lines,
    );

    // Concrete command invocation inside the entry body. It is indented by
    // `TRANSCRIPT_BODY_LEADING_INDENT` so the invocation's first column lines
    // up with the result body drawn below it by `draw_message_body` — the
    // entry reads as one aligned block instead of a hanging head.
    let body_indent = " ".repeat(TRANSCRIPT_BODY_LEADING_INDENT as usize);
    let invocation_style = if phase == CommandPhase::Pending {
        Style::default()
            .fg(ctx.theme.muted())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg((*ctx.theme).fg())
            .add_modifier(Modifier::BOLD)
    };

    let invocation_line = Line::from(vec![
        Span::styled(body_indent, invocation_style),
        Span::styled(invocation, invocation_style),
    ]);
    {
        ctx.paint(invocation_line);
    }

    // Result body blocks
    if phase == CommandPhase::Completed && msg.command_result_text().is_some() {
        ctx.advance_blank_rows(1);
        // An ack with detail renders in its own two-tone scheme (ADR-0106):
        // the headline in the entry's foreground tone — the part the eye
        // should land on first — and each explanation line muted below it.
        // The generic body path stays for every other result shape.
        if let Some((title, detail)) = msg.command_ack_split() {
            draw_ack_body(ctx, mi, title, detail, (*ctx.theme).info());
        } else {
            draw_message_body(
                &mut *ctx.frame,
                ctx.area,
                msg,
                mi,
                selection,
                cell_selection,
                ctx.theme,
                &mut *ctx.layout_map,
                &mut *ctx.skip_rows,
                ctx.y,
                &mut *ctx.content_lines,
                true,
            );
        }
    }
}

/// Draw an ack reply body: headline first in the foreground tone, then each
/// detail line muted — never one `•`-joined row (the layout bug this fixes).
pub fn draw_ack_body(
    ctx: &mut RenderCtx<'_, '_>,
    mi: usize,
    title: &str,
    detail: &[String],
    family_tone: Color,
) {
    let _theme = ctx.theme;
    let _transcript_area = ctx.area;

    let body_indent = " ".repeat(TRANSCRIPT_BODY_LEADING_INDENT as usize);
    let wrap_width = (ctx.area.width as usize)
        .saturating_sub(TRANSCRIPT_BODY_LEADING_INDENT as usize)
        .max(1);

    // Headline: prominent (the command family tone, bold) so "Delegated mode ON"
    // reads as the outcome, not as detail noise.
    let title_style = Style::default()
        .fg(family_tone)
        .add_modifier(Modifier::BOLD);
    let mut regions: Vec<(String, Style)> = vec![(title.to_string(), title_style)];
    // Detail: muted prose, one line each, wrapping preserved.
    let muted_style = Style::default().fg(ctx.theme.muted());
    for line in detail {
        regions.push((line.clone(), muted_style));
    }

    for (text, style) in regions {
        for wl in wrap_text(&text, wrap_width) {
            *ctx.content_lines += 1;
            if *ctx.skip_rows > 0 {
                *ctx.skip_rows -= 1;
                continue;
            }
            if *ctx.y >= ctx.area.y + ctx.area.height {
                return;
            }
            let line = Line::from(vec![
                Span::styled(body_indent.clone(), style),
                Span::styled(wl.text.clone(), style),
            ]);
            let rect = Rect::new(ctx.area.x, *ctx.y, ctx.area.width, 1);
            (*ctx.frame).render_widget(Paragraph::new(line), rect);
            (*ctx.layout_map).push(BlockRegion {
                message_idx: mi,
                block_idx: COMMAND_RESULT_BLOCK_IDX,
                start_byte: 0,
                end_byte: text.len(),
                text: wl.text.clone(),
                prefix_cols: TRANSCRIPT_BODY_LEADING_INDENT,
                rect,
                hidden_ranges: Vec::new(),
            });
            *ctx.y += 1;
        }
    }
}

/// If any expanded step's body covers the top of the viewport, render its
/// summary pinned there as a sticky overlay and return its layout info so the
/// app can route clicks to it. Returns `None` when no sticky summary is
/// needed.
///
/// A sticky summary only exists for an *expanded* step (its body is what is
/// scrolled into view), so it always renders in the shared ladder's expanded
/// state — the primary foreground — matching the inline summary of an open
/// step.
pub fn draw_sticky_summary_if_needed(
    frame: &mut Frame,
    transcript_area: Rect,
    sticky_steps: &[StickyStep],
    scroll: u16,
    theme: &Theme,
) -> Option<StickyInfo> {
    let first_visible = scroll as usize;
    let step = sticky_steps
        .iter()
        .find(|c| c.summary_line < first_visible && c.body_end_line > first_visible)?;
    // Sticky steps are always expanded → the summary reads in its active tone
    // (the primary foreground), matching the inline summary of an open step.
    let summary_color = theme.fg();
    // `transcript_area` arrives already inset by `draw_transcript`, so both
    // branches pin directly inside it — no re-clip needed.
    let line_rect = if let Some(bg) = step.background {
        let line_rect = Rect::new(
            transcript_area.x,
            transcript_area.y,
            transcript_area.width,
            1,
        );
        frame.render_widget(
            Paragraph::new(tool_summary_line(
                MARKER_EXPANDED,
                &step.summary,
                summary_color,
                bg,
                transcript_area.width as usize,
            )),
            line_rect,
        );
        line_rect
    } else {
        let line_rect = Rect::new(
            transcript_area.x,
            transcript_area.y,
            transcript_area.width,
            1,
        );
        frame.render_widget(
            Paragraph::new(reasoning_summary_line(
                MARKER_EXPANDED,
                &step.summary,
                step.color,
                summary_color,
                transcript_area.width as usize,
            )),
            line_rect,
        );
        line_rect
    };
    Some(StickyInfo {
        message_idx: step.message_idx,
        rect: line_rect,
        summary_line: step.summary_line,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mutx_engine::Color;

    #[test]
    fn truncate_to_width_stops_at_newline() {
        assert_eq!(
            truncate_to_width("Run python3 -c\ns=open(...)", 50),
            "Run python3 -c…"
        );
        assert_eq!(truncate_to_width("abc\r\ndef", 50), "abc…");
        assert_eq!(truncate_to_width("single line", 50), "single line");
        assert_eq!(
            truncate_to_width("very long single line that exceeds width", 10),
            "very long…"
        );
    }

    #[test]
    fn tool_summary_line_produces_single_row_span_without_newline() {
        let line = tool_summary_line(
            "+",
            "Run python3 -c\ns=open(...)",
            Color::White,
            Color::Black,
            40,
        );
        for span in &line.spans {
            assert!(
                !span.content.contains('\n'),
                "span must never contain newline"
            );
            assert!(
                !span.content.contains('\r'),
                "span must never contain carriage return"
            );
        }
    }
}
