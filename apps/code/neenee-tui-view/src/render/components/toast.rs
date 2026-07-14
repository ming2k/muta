//! Transient overlay toast bubbles.

use neenee_tui::{
    Block as RtBlock, Borders, Color, Frame, Modifier, Paragraph, Rect, Span, {Line, Style},
};
use unicode_width::UnicodeWidthStr;

use super::super::Theme;

pub(in crate::render) enum ToastKind {
    CopyOk,
    CopyFailed,
    Armed,
    Custom(Color),
}

impl ToastKind {
    fn color(&self, theme: &Theme) -> Color {
        match *self {
            ToastKind::CopyOk => theme.ok(),
            ToastKind::CopyFailed => theme.err(),
            ToastKind::Armed => theme.warn(),
            ToastKind::Custom(color) => color,
        }
    }
}

pub(in crate::render) struct ToastBubble<'a> {
    pub message: &'a str,
    pub kind: ToastKind,
}

impl<'a> ToastBubble<'a> {
    pub(in crate::render) fn render(self, frame: &mut Frame, theme: &Theme) {
        let size = frame.area();
        self.render_at_width(frame, theme, size.width);
    }

    pub(in crate::render) fn render_at_width(self, frame: &mut Frame, theme: &Theme, width: u16) {
        let color = self.kind.color(theme);
        draw_toast(frame, theme, self.message, color, width);
    }
}

pub(in crate::render) fn draw_toast(
    frame: &mut Frame,
    theme: &Theme,
    message: &str,
    color: Color,
    width: u16,
) {
    let text = format!(" {} ", message.trim());
    let inner_w = text.width() as u16;
    let toast_width = inner_w.min(58) + 2;
    let x = width.saturating_sub(toast_width).saturating_sub(2).max(1);
    let area = Rect::new(x, 1, toast_width, 3);

    let block = RtBlock::default()
        .borders(Borders::LEFT | Borders::RIGHT)
        .border_type(neenee_tui::BorderType::Thick)
        .border_style(Style::default().fg(color))
        .style(Style::default().bg(theme.panel()));

    let line = Line::from(Span::styled(
        text,
        Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
    ));
    let para = Paragraph::new(vec![Line::from(""), line]);
    frame.render_widget(para.block(block), area);
}
