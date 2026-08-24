//! Renderer for round-interrupt markers (C11): one warning entry per round
//! stopped before completing — user interrupt (Esc Esc), superseded by newer
//! input, or killed with the process.
//!
//! Structured exactly like a notice entry (ADR-0111's universal Entry shape)
//! but with its own glyph + category so the marker reads as a distinct
//! *event* in the stream, not as harness chatter:
//!
//! ```text
//! ▲ interrupted · 21:39          ← header: warn tone + BOLD, muted time tail
//!                                ← 1 blank row (TURN_HEADER_BODY_GAP_ROWS)
//!   round 3 · Esc Esc            ← body, indent TRANSCRIPT_BODY_LEADING_INDENT
//! ```
//!
//! The severity tone is `theme.warn()` — the same user-intervention tone the
//! tool-step renderer uses for `ToolStatus::Interrupted` (`tools/mod.rs`),
//! so every "a human (or the host) stopped this work" surface agrees.

use mutx_engine::{Frame, Modifier, Rect, Span, Style};

use crate::design::{TRANSCRIPT_BODY_LEADING_INDENT, TURN_HEADER_BODY_GAP_ROWS};
use crate::model::document::TranscriptMessage;
use crate::model::layout::LayoutMap;
use crate::text_layout::wrap_text;
use unicode_width::UnicodeWidthStr;

use super::Theme;

/// Build the one-row header of a round-interrupt entry:
/// `▲ interrupted · 21:39`. The glyph + label render in the warn tone
/// (BOLD), followed by the muted trailing timestamp ` · HH:MM` — the same
/// three-part skeleton as `notice_header_line` / `command_header_line`.
fn round_interrupt_header_line(
    warn: mutx_engine::Color,
    time_label: Option<&str>,
    muted: mutx_engine::Color,
    full_width: usize,
) -> mutx_engine::Line<'static> {
    let mut spans = Vec::with_capacity(4);
    let mut used = 0usize;

    // Indicator tag: `▲ interrupted` in the warn tone + BOLD.
    let tag = "▲ interrupted";
    used += tag.width();
    spans.push(Span::styled(
        tag,
        Style::default().fg(warn).add_modifier(Modifier::BOLD),
    ));

    // Trailing timestamp: ` · HH:MM` in muted color.
    if let Some(time) = time_label {
        let time_span = format!(" · {time}");
        let budget = full_width.saturating_sub(used);
        if time_span.width() <= budget {
            spans.push(Span::styled(time_span, Style::default().fg(muted)));
        }
    }

    mutx_engine::Line::from(spans)
}

/// Draw a round-interrupt marker as a top-level **Entry** (C11, following the
/// ADR-0111 universal shape): header row, one-row gap, unfolded body. The
/// body is the `TranscriptMessage::round_interrupted` raw text
/// (`round N · <reason>`), wrapped at the body width in the foreground tone.
#[allow(clippy::too_many_arguments)]
pub fn draw_round_interrupt(
    frame: &mut Frame,
    area: Rect,
    msg: &TranscriptMessage,
    mi: usize,
    layout_map: &mut LayoutMap,
    skip_rows: &mut usize,
    current_y: &mut u16,
    content_lines: &mut usize,
    theme: &Theme,
) {
    let full_width = area.width as usize;
    if full_width < (TRANSCRIPT_BODY_LEADING_INDENT as usize + 1) {
        // Degenerate width: skip rendering entirely but keep the row count
        // honest so scroll math never desyncs.
        *content_lines += 1;
        if *skip_rows > 0 {
            *skip_rows -= 1;
        }
        return;
    }
    let time_label = msg.sent_at_ms.map(crate::time::sent_time_label);

    // 1. Entry header.
    let header_line = round_interrupt_header_line(
        theme.warn(),
        time_label.as_deref(),
        theme.muted(),
        full_width,
    );
    *content_lines += 1;
    if *skip_rows > 0 {
        *skip_rows -= 1;
    } else if *current_y < area.y + area.height {
        let line_rect = Rect::new(area.x, *current_y, area.width, 1);
        frame.render_widget(mutx_engine::Paragraph::new(header_line), line_rect);
        layout_map.push(crate::model::layout::BlockRegion {
            message_idx: mi,
            block_idx: 0,
            start_byte: 0,
            end_byte: 0,
            text: String::new(),
            prefix_cols: 0,
            rect: line_rect,
            hidden_ranges: Vec::new(),
        });
        *current_y += 1;
    }

    // 2. 1-row blank gap between entry header and body.
    for _ in 0..TURN_HEADER_BODY_GAP_ROWS {
        *content_lines += 1;
        if *skip_rows > 0 {
            *skip_rows -= 1;
        } else if *current_y < area.y + area.height {
            *current_y += 1;
        }
    }

    // 3. Body: the reason line, indented.
    let body_wrap_width = full_width
        .saturating_sub(TRANSCRIPT_BODY_LEADING_INDENT as usize)
        .max(1);
    let body_lines = wrap_text(&msg.raw, body_wrap_width);
    for wl in body_lines {
        *content_lines += 1;
        if *skip_rows > 0 {
            *skip_rows -= 1;
            continue;
        }
        if *current_y >= area.y + area.height {
            break;
        }
        let line_rect = Rect::new(
            area.x + TRANSCRIPT_BODY_LEADING_INDENT,
            *current_y,
            area.width.saturating_sub(TRANSCRIPT_BODY_LEADING_INDENT),
            1,
        );
        frame.render_widget(
            mutx_engine::Paragraph::new(mutx_engine::Line::from(vec![Span::styled(
                wl.text,
                Style::default().fg(theme.fg()),
            )])),
            line_rect,
        );
        *current_y += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_is_glyph_label_and_optional_time() {
        let line = round_interrupt_header_line(
            mutx_engine::Color::Yellow,
            Some("21:39"),
            mutx_engine::Color::DarkGray,
            40,
        );
        let text: String = line.spans.iter().map(|s| s.content.clone()).collect();
        assert_eq!(text, "▲ interrupted · 21:39");
    }

    #[test]
    fn header_omits_time_when_absent() {
        let line = round_interrupt_header_line(
            mutx_engine::Color::Yellow,
            None,
            mutx_engine::Color::DarkGray,
            40,
        );
        let text: String = line.spans.iter().map(|s| s.content.clone()).collect();
        assert_eq!(text, "▲ interrupted");
    }

    #[test]
    fn header_drops_time_that_does_not_fit() {
        let line = round_interrupt_header_line(
            mutx_engine::Color::Yellow,
            Some("21:39"),
            mutx_engine::Color::DarkGray,
            5, // narrower than " · 21:39"
        );
        let text: String = line.spans.iter().map(|s| s.content.clone()).collect();
        assert_eq!(text, "▲ interrupted");
    }
}
