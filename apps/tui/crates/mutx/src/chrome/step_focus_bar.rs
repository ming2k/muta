//! The clean focus bar shown when the Transcript region is focused in Session View.

use mutx_engine::{Frame, Line, Modifier, Paragraph, Rect, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::components::keycap::keycap_span;
use crate::keymap::{Key, keyvocab};
use crate::view::Theme;

/// Draw the single-row focus bar directly above the composer:
/// `TRANSCRIPT   ↑↓ move   Enter open   Esc compose`
pub fn draw_step_focus_bar(frame: &mut Frame, rect: Rect, theme: &Theme) -> Rect {
    if rect.height == 0 || rect.width < 20 {
        return rect;
    }

    let full_w = rect.width as usize;
    let brand_bold = Style::default()
        .fg(theme.brand())
        .add_modifier(Modifier::BOLD);
    let label_style = theme.keycap_label_style();

    let spans: Vec<Span<'static>> = vec![
        Span::styled("TRANSCRIPT", brand_bold),
        Span::raw("   "),
        keycap_span(theme, keyvocab::ARROWS_UD),
        Span::styled(" move", label_style),
        Span::raw("   "),
        keycap_span(theme, Key::ENTER.display()),
        Span::styled(" open", label_style),
        Span::raw("   "),
        keycap_span(theme, Key::ESC.display()),
        Span::styled(" compose", label_style),
    ];

    let total_w: usize = spans.iter().map(|s| s.content.width()).sum();

    let line_spans = if full_w >= total_w {
        spans
    } else {
        vec![
            Span::styled("TRANSCRIPT", brand_bold),
            Span::raw(" "),
            keycap_span(theme, Key::ESC.display()),
            Span::styled(" compose", label_style),
        ]
    };

    let p = Paragraph::new(Line::from(line_spans))
        .style(Style::default().bg(theme.panel()));
    frame.render_widget(p, rect);
    rect
}
