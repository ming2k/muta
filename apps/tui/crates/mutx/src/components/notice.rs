//! Transcript notice component (System / Infrastructure notices as top-level transcript entries).

use mutx_engine::{
    Color, Frame, Modifier, Paragraph, Rect, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::design::{TRANSCRIPT_BODY_LEADING_INDENT, TURN_HEADER_BODY_GAP_ROWS};
use crate::model::document::{MessageKind, NoticeSeverity, TranscriptMessage};
use crate::model::layout::{BlockRegion, LayoutMap, NOTICE_BLOCK_IDX};
use crate::text_layout::wrap_text;

use super::super::Theme;

pub(crate) struct NoticeView<'a> {
    pub message: &'a TranscriptMessage,
}

impl<'a> NoticeView<'a> {
    fn severity(&self) -> Option<NoticeSeverity> {
        match &self.message.kind {
            MessageKind::Notice { severity, .. } => Some(*severity),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeContent {
    /// E.g. "OpenAI HTTP 429 Too Many Requests", "Anthropic HTTP 529 Overloaded", or "System Notice".
    pub header: String,
    /// Optional pretty-printed JSON body or multiline diagnostic detail.
    pub detail: Option<String>,
}

/// Parse raw notice text: extracts the canonical header (`[Message/Provider] StatusCode StatusReason`)
/// and pretty-prints any JSON response body into `detail`. Strips out mechanical "Gave up after..." boilerplate.
pub fn parse_notice_content(raw: &str) -> NoticeContent {
    // 1. Strip mechanical boilerplate text if present
    let clean = if let Some(pos) = raw.find("Gave up after") {
        &raw[..pos]
    } else {
        raw
    }
    .trim();

    // 2. Check for JSON payload
    if let Some(pos) = clean.find('{') {
        let prefix = clean[..pos].trim_end_matches([':', ' ', '\n', '\t', '\r']);
        let json_candidate = clean[pos..].trim();
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(json_candidate) {
            let pretty_json =
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| json_candidate.to_string());
            let header = if prefix.is_empty() {
                "Provider HTTP Error".to_string()
            } else {
                prefix.to_string()
            };
            return NoticeContent {
                header,
                detail: Some(pretty_json),
            };
        }
    }

    // 3. Check for multi-line non-JSON detail
    if let Some((first_line, rest)) = clean.split_once('\n') {
        let rest_trimmed = rest.trim();
        if !rest_trimmed.is_empty() {
            return NoticeContent {
                header: first_line.trim().to_string(),
                detail: Some(rest_trimmed.to_string()),
            };
        }
    }

    // 4. Single-line plain message
    NoticeContent {
        header: clean.to_string(),
        detail: None,
    }
}

/// Build the one-row header of a notification entry: label left, time right.
/// The lead glyph (`! `, `▲ `, `ℹ `) and `notification` label are rendered in the
/// severity indicator tone (BOLD), followed by an optional right-aligned time.
fn notice_header_line(
    lead_symbol: &str,
    severity_tone: Color,
    time_label: Option<&str>,
    muted: Color,
    full_width: usize,
) -> Line<'static> {
    let mut spans = Vec::with_capacity(4);
    let mut used = 0usize;

    // Indicator tag: `! notification`, `▲ notification`, `ℹ notification` in severity_tone + BOLD.
    let tag = format!("{lead_symbol}notification");
    used += tag.width();
    spans.push(Span::styled(
        tag,
        Style::default()
            .fg(severity_tone)
            .add_modifier(Modifier::BOLD),
    ));

    // Trailing timestamp: right-aligned in muted color.
    if let Some(time) = time_label {
        let time_width = time.width();
        if used + 2 + time_width <= full_width {
            spans.push(Span::styled(
                " ".repeat(full_width - used - time_width),
                Style::default(),
            ));
            spans.push(Span::styled(time.to_string(), Style::default().fg(muted)));
        }
    }

    Line::from(spans)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_notice_view(
    frame: &mut Frame,
    area: Rect,
    notice: NoticeView<'_>,
    mi: usize,
    layout_map: &mut LayoutMap,
    skip_rows: &mut usize,
    current_y: &mut u16,
    content_lines: &mut usize,
    theme: &Theme,
    _hovered: bool,
    _focused: bool,
) {
    let Some(severity) = notice.severity() else {
        return;
    };
    let (lead_symbol, tag_color) = match severity {
        NoticeSeverity::Error => ("! ", theme.err()),
        NoticeSeverity::Warning => ("▲ ", theme.warn()),
        NoticeSeverity::Info => ("ℹ ", theme.info()),
    };
    let parsed = parse_notice_content(&notice.message.raw);
    let full_width = area.width as usize;
    let time_label = notice.message.sent_at_ms.map(crate::time::sent_time_label);

    // 1. Notification Entry Header
    let header_line = notice_header_line(
        lead_symbol,
        tag_color,
        time_label.as_deref(),
        theme.muted(),
        full_width,
    );

    *content_lines += 1;
    if *skip_rows > 0 {
        *skip_rows -= 1;
    } else if *current_y < area.y + area.height {
        let line_rect = Rect::new(area.x, *current_y, area.width, 1);
        frame.render_widget(Paragraph::new(header_line), line_rect);

        layout_map.push(BlockRegion {
            message_idx: mi,
            block_idx: NOTICE_BLOCK_IDX,
            start_byte: 0,
            end_byte: 0,
            text: String::new(),
            prefix_cols: 0,
            rect: line_rect,
            hidden_ranges: Vec::new(),
        });
        *current_y += 1;
    }

    // 2. 1-row blank gap between entry header and body
    for _ in 0..TURN_HEADER_BODY_GAP_ROWS {
        *content_lines += 1;
        if *skip_rows > 0 {
            *skip_rows -= 1;
        } else if *current_y < area.y + area.height {
            *current_y += 1;
        }
    }

    // 3. Notice body: header text
    let body_wrap_width = full_width
        .saturating_sub(TRANSCRIPT_BODY_LEADING_INDENT as usize)
        .max(1);
    let body_lines = wrap_text(&parsed.header, body_wrap_width);

    for wl in body_lines {
        *content_lines += 1;
        if *skip_rows > 0 {
            *skip_rows -= 1;
            continue;
        }
        if *current_y >= area.y + area.height {
            break;
        }

        let line_rect = Rect::new(area.x, *current_y, area.width, 1);
        let spans = vec![Span::styled(
            wl.text.clone(),
            Style::default().fg(theme.fg()),
        )];
        frame.render_widget(Paragraph::new(Line::from(spans)), line_rect);

        layout_map.push(BlockRegion {
            message_idx: mi,
            block_idx: NOTICE_BLOCK_IDX,
            start_byte: 0,
            end_byte: wl.text.len(),
            text: wl.text,
            prefix_cols: 0,
            rect: line_rect,
            hidden_ranges: Vec::new(),
        });
        *current_y += 1;
    }

    // 4. Detail body: formatted detail (unfolded direct rendering)
    if let Some(detail) = parsed.detail.as_ref() {
        let detail_indent = "  ";
        let detail_wrap_width = full_width.saturating_sub(detail_indent.width() + 2).max(1);

        for line_str in detail.lines() {
            let wrapped_detail = wrap_text(line_str, detail_wrap_width);
            for dwl in wrapped_detail {
                *content_lines += 1;
                if *skip_rows > 0 {
                    *skip_rows -= 1;
                    continue;
                }
                if *current_y >= area.y + area.height {
                    break;
                }

                let line_rect = Rect::new(area.x, *current_y, area.width, 1);
                let spans = vec![
                    Span::styled(detail_indent, Style::default()),
                    Span::styled(dwl.text.clone(), Style::default().fg(theme.muted())),
                ];
                frame.render_widget(Paragraph::new(Line::from(spans)), line_rect);

                layout_map.push(BlockRegion {
                    message_idx: mi,
                    block_idx: NOTICE_BLOCK_IDX,
                    start_byte: 0,
                    end_byte: dwl.text.len(),
                    text: dwl.text,
                    prefix_cols: detail_indent.width() as u16,
                    rect: line_rect,
                    hidden_ranges: Vec::new(),
                });
                *current_y += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_error_into_header_and_pretty_detail() {
        let raw = r#"OpenAI HTTP 429 Too Many Requests: {"error":{"code":"1305","message":"该模型当前访问量过大，请您稍后再试"}}
Gave up after 6 attempt(s); the upstream service appears overloaded. Resend the message to try again, or raise `provider_retry_max_attempts` for more attempts."#;
        let parsed = parse_notice_content(raw);
        assert_eq!(parsed.header, "OpenAI HTTP 429 Too Many Requests");
        assert!(parsed.detail.is_some());
        let detail = parsed.detail.unwrap();
        assert!(detail.contains("\"code\": \"1305\""));
        assert!(detail.contains("该模型当前访问量过大，请您稍后再试"));
        assert!(!detail.contains("Gave up after"));
    }

    #[test]
    fn parses_retry_exhausted_json_error() {
        let raw = r#"Exhausted 30 retry attempts — Google HTTP 429 Too Many Requests: {"error":{"code":429,"message":"Resource has been exhausted","status":"RESOURCE_EXHAUSTED"}}"#;
        let parsed = parse_notice_content(raw);
        assert_eq!(
            parsed.header,
            "Exhausted 30 retry attempts — Google HTTP 429 Too Many Requests"
        );
        assert!(parsed.detail.is_some());
        let detail = parsed.detail.unwrap();
        assert!(detail.contains("\"code\": 429"));
        assert!(detail.contains("\"status\": \"RESOURCE_EXHAUSTED\""));
    }

    #[test]
    fn parses_plain_http_error_without_json() {
        let raw = "OpenAI HTTP 503 Service Unavailable";
        let parsed = parse_notice_content(raw);
        assert_eq!(parsed.header, "OpenAI HTTP 503 Service Unavailable");
        assert_eq!(parsed.detail, None);
    }

    #[test]
    fn parses_plain_message_without_detail() {
        let raw = "Failed to connect to host";
        let parsed = parse_notice_content(raw);
        assert_eq!(parsed.header, "Failed to connect to host");
        assert_eq!(parsed.detail, None);
    }

    #[test]
    fn parses_multiline_text_into_first_line_and_rest() {
        let raw = "Claude HTTP 500 Server Error\nDetails:\nhost unreachable";
        let parsed = parse_notice_content(raw);
        assert_eq!(parsed.header, "Claude HTTP 500 Server Error");
        assert_eq!(
            parsed.detail,
            Some("Details:\nhost unreachable".to_string())
        );
    }

    #[test]
    fn draw_notice_view_renders_as_entry_with_header_gap_and_body() {
        let theme = Theme::default();
        let mut grid = mutx_engine::Grid::new(60, 10);
        let mut frame = Frame::new(&mut grid);
        let area = Rect::new(0, 0, 60, 10);
        let msg = TranscriptMessage::notice(NoticeSeverity::Error, "Connection refused");
        let notice = NoticeView { message: &msg };
        let mut layout_map = LayoutMap::default();
        let mut skip_rows = 0;
        let mut current_y = 0;
        let mut content_lines = 0;

        draw_notice_view(
            &mut frame,
            area,
            notice,
            0,
            &mut layout_map,
            &mut skip_rows,
            &mut current_y,
            &mut content_lines,
            &theme,
            false,
            false,
        );

        // Header (row 0) + 1-row gap (row 1) + Body (row 2) = 3 content lines
        assert_eq!(content_lines, 3);
        assert_eq!(current_y, 3);

        // Verify row 0 contains "! notification"
        let buf = frame.buffer_mut();
        let mut row0 = String::new();
        for x in 0..20 {
            row0.push_str(buf[(x, 0)].symbol());
        }
        assert!(row0.starts_with("! notification"));

        // Verify row 2 contains "Connection refused"
        let mut row2 = String::new();
        for x in 0..25 {
            row2.push_str(buf[(x, 2)].symbol());
        }
        assert!(row2.contains("Connection refused"));
    }
}
