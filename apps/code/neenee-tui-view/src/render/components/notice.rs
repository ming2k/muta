//! Transcript notice component.

use neenee_tui::{
    Color, Frame, Paragraph, Rect, Style, {Line, Span},
};

use crate::document::{MessageKind, NoticeSeverity, TranscriptMessage};

use super::super::Theme;
use super::super::text_layout::wrap_text;

pub(in crate::render) struct NoticeView<'a> {
    pub message: &'a TranscriptMessage,
}

impl<'a> NoticeView<'a> {
    fn severity(&self) -> Option<NoticeSeverity> {
        match &self.message.kind {
            MessageKind::Notice { severity } => Some(*severity),
            _ => None,
        }
    }
}

fn severity_presentation(severity: NoticeSeverity, theme: &Theme) -> (&'static str, Color) {
    match severity {
        NoticeSeverity::Error => ("✖", theme.err()),
        NoticeSeverity::Warning => ("!", theme.warn()),
        NoticeSeverity::Info => ("ℹ", theme.info()),
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::render) fn draw_notice_view(
    frame: &mut Frame,
    area: Rect,
    notice: NoticeView<'_>,
    skip_rows: &mut usize,
    current_y: &mut u16,
    content_lines: &mut usize,
    theme: &Theme,
) {
    let Some(severity) = notice.severity() else {
        return;
    };
    let (glyph, color) = severity_presentation(severity, theme);

    let glyph_segment = format!("{glyph} ");
    let indent_cols = 2usize;
    let body_wrap_width = area.width.saturating_sub(indent_cols as u16).max(1) as usize;

    let lines = wrap_text(&notice.message.raw, body_wrap_width);
    *content_lines += lines.len();

    let base = Style::default().fg(color);
    let glyph_style = Style::default().fg(color);
    for (idx, wl) in lines.iter().enumerate() {
        if *skip_rows > 0 {
            *skip_rows = skip_rows.saturating_sub(1);
            continue;
        }
        if *current_y >= area.y + area.height {
            break;
        }

        let line = if idx == 0 {
            Line::from(vec![
                Span::styled(glyph_segment.clone(), glyph_style),
                Span::styled(wl.text.clone(), base),
            ])
        } else {
            Line::from(vec![
                Span::styled(" ".repeat(indent_cols), Style::default()),
                Span::styled(wl.text.clone(), base),
            ])
        };
        let line_rect = Rect::new(area.x, *current_y, area.width, 1);
        frame.render_widget(Paragraph::new(line), line_rect);
        *current_y += 1;
    }
}
