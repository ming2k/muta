//! Per-tool body content renderers (bash output, grep, find, diff, file write, code blocks).

use mutx_engine::{
    Color, Modifier, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::base::{
    MARKER_COLLAPSED, MARKER_EXPANDED, RenderCtx, nonempty_wrapped, truncate_to_width,
};
use crate::model::layout::BlockRegion;
use crate::model::selection::SelectionState;
use crate::text_layout::{
    CodeGutterParams, WrappedLine, block_selection_range, clamp_selection_range, code_gutter_line,
    line_selection, line_spans, padded_tail, wrap_text,
};
use crate::tools::{DiffCache, DiffHunk, DiffOp, ResultKind};
use crate::view::{
    BASH_FOLD_HEAD_ROWS, BASH_FOLD_TAIL_ROWS, CODE_BAND_GUTTER_GAP, CODE_BAND_GUTTER_MIN_WIDTH,
    Theme,
};

/// Build the summary line for a tool/runner step: an optional expand marker
/// followed by the summary text, padded to `full_width`.
pub(crate) fn tool_summary_line(
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
pub(crate) fn draw_step_summary(
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
pub(crate) fn draw_blank_rows(ctx: &mut RenderCtx<'_, '_>, style: Style, rows: usize) {
    for _ in 0..rows {
        let _ = ctx.paint(Line::from(Span::styled(
            padded_tail(ctx.full_width, 0),
            style,
        )));
    }
}

/// Render text content as a code block with a line-number gutter on
/// `code_surface`. Used for `read_text` / `edit_text` results and as the
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
pub(crate) fn draw_code_content(
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
pub(crate) fn draw_listing_content(
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
pub(crate) fn draw_checklist_content(
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

            let prefix_cols = if idx == 0 { indent } else { indent + 2 };
            let mut line = if idx == 0 && wl.text.starts_with(glyph) {
                let rest_text = &wl.text[glyph.len()..];
                let glyph_len = glyph.len();
                let (glyph_bg_style, rest_sel) = match line_selection(sel_range, &block_wl) {
                    Some((lo, hi)) => {
                        let g_style = if lo < glyph_len {
                            glyph_style.bg(ctx.theme.selected())
                        } else {
                            glyph_style
                        };
                        let r_sel = if hi > glyph_len {
                            let r_lo = lo.saturating_sub(glyph_len);
                            let r_hi = hi - glyph_len;
                            (r_lo < r_hi).then_some((r_lo, r_hi))
                        } else {
                            None
                        };
                        (g_style, r_sel)
                    }
                    None => (glyph_style, None),
                };
                let mut spans = vec![
                    Span::styled(" ".repeat(indent), pad),
                    Span::styled(glyph, glyph_bg_style),
                ];
                let rest_spans = line_spans(
                    "",
                    pad,
                    rest_text,
                    rest_sel,
                    text_style,
                    ctx.theme.selected(),
                );
                spans.extend(rest_spans.spans.into_iter().filter(|s| !s.content.is_empty()));
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

            let used = prefix_cols + wl.text.width();
            line.spans
                .push(Span::styled(padded_tail(ctx.full_width, used), pad));
            ctx.paint_text_row(line, mi, block_idx, &block_wl, prefix_cols as u16, &[]);
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
pub(crate) fn draw_matches_content(
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
                    let selected = clamp_selection_range(
                        line_selection(sel_range, &block_wl),
                        &wl.text,
                    );
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
/// prompt line first, then stdout / stderr / an exit or truncation footer.
/// Output rows have no line-number gutter. Legacy section markers (`Exit N`,
/// `STDOUT:`, …) are highlighted in `warning` for sessions restored without a
/// structured payload. The command line is not selectable (it's derived from
/// the call, not the output stream); output rows are.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_command_content(
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
        let output_rows = command_structured_lines(lines, stdout, stderr, base);
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
/// them sequentially). Output lines are styled using terminal text style `base`.
///
/// Prefers the arrival-ordered `lines` (the TUI-authoritative interleaved
/// view), falling back to the all-stdout-then-all-stderr flat strings for the
/// legacy / live-seed / restored-session path. Empty bands contribute nothing.
fn command_structured_lines(
    lines: &[muta_contracts::tool_output::ShellLine],
    stdout: &str,
    stderr: &str,
    base: Style,
) -> Vec<(String, Style)> {
    let mut out: Vec<(String, Style)> = Vec::new();
    if !lines.is_empty() {
        for l in lines {
            // `emit_command_lines` normalizes CR/BS itself, so pass the raw text.
            out.push((l.text.clone(), base));
        }
        return out;
    }
    // Legacy / live-seed fallback: all-stdout band then all-stderr band.
    for text in [stdout, stderr] {
        let text = text.trim_end_matches(&['\r', '\n'][..]);
        if text.is_empty() {
            continue;
        }
        for line in text.split('\n') {
            out.push((line.to_string(), base));
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
pub(crate) fn draw_tool_result(
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
pub(crate) fn draw_tool_error(
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
pub(crate) fn draw_diff_content(
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
