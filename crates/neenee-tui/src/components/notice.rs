//! Transcript notice component (System / Infrastructure notices & error cards).

use neenee_tui_engine::{
    Color, Frame, Modifier, Paragraph, Rect, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::model::document::{MessageKind, NoticeSeverity, TranscriptMessage};
use crate::model::layout::{BlockRegion, LayoutMap, NOTICE_BLOCK_IDX};
use crate::text_layout::{padded_tail, wrap_text};

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

    fn expanded(&self) -> bool {
        match &self.message.kind {
            MessageKind::Notice { expanded, .. } => *expanded,
            _ => false,
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
            let pretty_json = serde_json::to_string_pretty(&value)
                .unwrap_or_else(|_| json_candidate.to_string());
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

fn notice_colors(
    severity: NoticeSeverity,
    theme: &Theme,
    hovered: bool,
    focused: bool,
) -> (Color, Color) {
    match severity {
        NoticeSeverity::Error => {
            let bg = if hovered || focused {
                theme.diff_del_hl
            } else {
                theme.diff_del_bg
            };
            (theme.err(), bg)
        }
        NoticeSeverity::Warning => {
            let bg = if hovered || focused {
                theme.input_bg_active
            } else {
                theme.element_bg
            };
            (theme.warn(), bg)
        }
        NoticeSeverity::Info => {
            let bg = if hovered || focused {
                theme.input_bg_active
            } else {
                theme.element_bg
            };
            (theme.info(), bg)
        }
    }
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
    hovered: bool,
    focused: bool,
) {
    let Some(severity) = notice.severity() else {
        return;
    };
    let (tag_color, card_bg) = notice_colors(severity, theme, hovered, focused);
    let parsed = parse_notice_content(&notice.message.raw);
    let full_width = area.width as usize;

    let header_style = Style::default()
        .bg(card_bg)
        .fg(tag_color)
        .add_modifier(Modifier::BOLD);

    let left_pad = "  ";
    let header_wrap_width = full_width.saturating_sub(left_pad.width() + 2).max(1);
    let header_lines = wrap_text(&parsed.header, header_wrap_width);

    *content_lines += header_lines.len().max(1);

    for (idx, wl) in header_lines.iter().enumerate() {
        if *skip_rows > 0 {
            *skip_rows = skip_rows.saturating_sub(1);
            continue;
        }
        if *current_y >= area.y + area.height {
            break;
        }

        let mut spans = Vec::new();
        let mut used = 0;

        let pad = if idx == 0 { "  " } else { "    " };
        spans.push(Span::styled(pad, Style::default().bg(card_bg)));
        used += pad.width();

        spans.push(Span::styled(wl.text.clone(), header_style));
        used += wl.text.width();

        spans.push(Span::styled(
            padded_tail(full_width, used),
            Style::default().bg(card_bg),
        ));

        let line_rect = Rect::new(area.x, *current_y, area.width, 1);
        frame.render_widget(Paragraph::new(Line::from(spans)), line_rect);

        layout_map.push(BlockRegion {
            message_idx: mi,
            block_idx: NOTICE_BLOCK_IDX,
            start_byte: 0,
            end_byte: parsed.header.len(),
            text: parsed.header.clone(),
            prefix_cols: pad.width() as u16,
            rect: line_rect,
            hidden_ranges: Vec::new(),
        });
        *current_y += 1;
    }

    if notice.expanded()
        && let Some(detail) = parsed.detail.as_ref()
    {
        let detail_indent = "    ";
        let detail_wrap_width = full_width.saturating_sub(detail_indent.width() + 2).max(1);

        for line_str in detail.lines() {
            let wrapped_detail = wrap_text(line_str, detail_wrap_width);
            *content_lines += wrapped_detail.len().max(1);

            for dwl in wrapped_detail {
                if *skip_rows > 0 {
                    *skip_rows = skip_rows.saturating_sub(1);
                    continue;
                }
                if *current_y >= area.y + area.height {
                    break;
                }

                let used = detail_indent.width() + dwl.text.width();
                let spans = vec![
                    Span::styled(detail_indent, Style::default().bg(card_bg)),
                    Span::styled(
                        dwl.text.clone(),
                        Style::default().bg(card_bg).fg(theme.text),
                    ),
                    Span::styled(
                        padded_tail(full_width, used),
                        Style::default().bg(card_bg),
                    ),
                ];

                let line_rect = Rect::new(area.x, *current_y, area.width, 1);
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
        assert_eq!(parsed.detail, Some("Details:\nhost unreachable".to_string()));
    }
}
