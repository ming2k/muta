//! Floating Which-Key / Leader Chord guide overlay card component.
//!
//! Renders a floating, bounded overlay card in the bottom-right corner of the
//! screen when a two-stroke leader chord (`Ctrl+X` or `Ctrl+C`) is armed.
//! Completely decoupled from View layouts, with zero layout shift.

use mutx_engine::{
    Block as RtBlock, Borders, Clear, Frame, Line, Paragraph, Rect, Span, Style,
};

use super::super::Theme;
use super::super::app::LeaderChord;
use super::keycap::keycap_span;

/// Render the floating leader chord guide if a chord is active.
pub(crate) fn draw_which_key_overlay(
    frame: &mut Frame,
    theme: &Theme,
    chord: LeaderChord,
    viewport: Rect,
) {
    if chord == LeaderChord::None || viewport.width < 40 || viewport.height < 10 {
        return;
    }

    let (title, items): (&'static str, Vec<(&'static str, &'static str, bool)>) = match chord {
        LeaderChord::CtrlX => (
            "C-x (View / Window)",
            vec![
                ("b", "switch view / buffer", true),
                ("k", "close current view", true),
                ("o", "other pane / focus", true),
                ("C-c", "quit mutx", true),
                ("C-g", "cancel", false),
            ],
        ),
        LeaderChord::CtrlC => (
            "C-c (Agent / Mode)",
            vec![
                ("c", "interrupt round", true),
                ("p", "permissions modal", true),
                ("t", "todos task list", true),
                ("m", "models picker", true),
                ("d", "performance report", true),
                ("C-g", "cancel", false),
            ],
        ),
        LeaderChord::None => return,
    };

    let card_width: u16 = 36;
    let card_height: u16 = (items.len() as u16) + 3; // title + items + padding

    // Position at bottom-right, 3 rows above the terminal bottom (above composer)
    let x = viewport
        .width
        .saturating_sub(card_width + 2)
        .max(1);
    let y = viewport
        .height
        .saturating_sub(card_height + 3)
        .max(1);

    let area = Rect::new(x, y, card_width.min(viewport.width.saturating_sub(x)), card_height);

    // 1. Wipe underlying text cleanly with Clear widget
    frame.render_widget(Clear, area);

    // 2. Build card block
    let block = RtBlock::default()
        .borders(Borders::LEFT | Borders::RIGHT | Borders::TOP | Borders::BOTTOM)
        .border_type(mutx_engine::BorderType::Thick)
        .border_style(Style::default().fg(theme.brand()))
        .style(Style::default().bg(theme.panel()));

    // 3. Build action lines
    let mut lines = Vec::with_capacity(items.len() + 2);
    lines.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            title,
            Style::default().fg(theme.brand()).add_modifier(mutx_engine::Modifier::BOLD),
        ),
    ]));
    for (key, desc, is_primary) in items {
        let key_span = keycap_span(theme, key);
        let pad = match key.len() {
            1 => "   ",
            2 => "  ",
            3 => " ",
            _ => " ",
        };
        let desc_style = if is_primary {
            Style::default().fg(theme.fg())
        } else {
            Style::default().fg(theme.dim())
        };
        lines.push(Line::from(vec![
            Span::raw(" "),
            key_span,
            Span::raw(pad),
            Span::styled(desc, desc_style),
        ]));
    }

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_key_overlay_renders_for_ctrl_x() {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(80, 24);
        terminal.draw(|f| {
            draw_which_key_overlay(f, &theme, LeaderChord::CtrlX, f.area());
        });
        let content: String = terminal
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(content.contains("C-x (View / Window)"));
        assert!(content.contains("switch view"));
        assert!(content.contains("cancel"));
    }

    #[test]
    fn which_key_overlay_renders_for_ctrl_c() {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(80, 24);
        terminal.draw(|f| {
            draw_which_key_overlay(f, &theme, LeaderChord::CtrlC, f.area());
        });
        let content: String = terminal
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(content.contains("C-c (Agent / Mode)"));
        assert!(content.contains("interrupt round"));
        assert!(content.contains("permissions"));
    }

    #[test]
    fn which_key_overlay_silent_when_none() {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(80, 24);
        terminal.draw(|f| {
            draw_which_key_overlay(f, &theme, LeaderChord::None, f.area());
        });
        let content: String = terminal
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(!content.contains("C-x"));
        assert!(!content.contains("C-c"));
    }
}
