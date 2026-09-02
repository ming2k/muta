//! The transient one-row step focus bar shown when a transcript step is keyboard-focused.

use mutx_engine::{Frame, Line, Modifier, Paragraph, Rect, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::components::keycap::keycap_span;
use crate::keymap::{Key, keyvocab};
use crate::view::Theme;

/// Draw the single-row step focus inspector bar directly above the composer.
pub fn draw_step_focus_bar(frame: &mut Frame, rect: Rect, theme: &Theme) -> Rect {
    if rect.height == 0 || rect.width < 20 {
        return rect;
    }

    let full_w = rect.width as usize;
    let brand_bold = Style::default()
        .fg(theme.brand())
        .add_modifier(Modifier::BOLD);
    let label_style = theme.keycap_label_style();

    let left: Vec<Span<'static>> = vec![
        Span::styled("◈ STEP FOCUS", brand_bold),
        Span::raw("   "),
        keycap_span(theme, keyvocab::ARROWS_UD),
        Span::styled(" step", label_style),
        Span::raw("   "),
        keycap_span(theme, "Esc / Ctrl+X o"),
        Span::styled(" input", label_style),
    ];

    let right: Vec<Span<'static>> = vec![
        keycap_span(theme, Key::ENTER.display()),
        Span::styled(
            " expand/toggle",
            Style::default().fg(theme.brand()).add_modifier(Modifier::BOLD),
        ),
    ];

    let left_w: usize = left.iter().map(|s| s.content.width()).sum();
    let right_w: usize = right.iter().map(|s| s.content.width()).sum();

    let mut line_spans: Vec<Span<'static>> = Vec::new();
    if full_w >= left_w + right_w + 2 {
        line_spans.extend(left);
        let gap = full_w - left_w - right_w;
        line_spans.push(Span::raw(" ".repeat(gap)));
        line_spans.extend(right);
    } else if full_w >= left_w {
        line_spans.extend(left);
    } else {
        // Narrow fallback
        line_spans.push(Span::styled("◈ STEP FOCUS", brand_bold));
        line_spans.push(Span::raw("   "));
        line_spans.push(keycap_span(theme, Key::ESC.display()));
        line_spans.push(Span::styled(" input", label_style));
    }

    let p = Paragraph::new(Line::from(line_spans))
        .style(Style::default().bg(theme.panel()));
    frame.render_widget(p, rect);

    rect
}
