//! Floating Which-Key / Leader Chord guide overlay card component.
//!
//! Renders a floating, bounded overlay card in the bottom-right corner of the
//! screen when a two-stroke leader chord (`Ctrl+X` or `Ctrl+C`) is armed.
//! Completely decoupled from View layouts, with zero layout shift.

use mutx_engine::{Block as RtBlock, Borders, Clear, Frame, Line, Paragraph, Rect, Span, Style};

use super::super::Theme;
use super::super::app::LeaderChord;
use super::super::keymap::Key;
use super::keycap::keycap_span;

/// A single entry in the which-key chord overlay.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WhichKeyItem {
    pub key: &'static str,
    pub desc: &'static str,
    pub is_primary: bool,
}

impl WhichKeyItem {
    pub const fn primary(key: &'static str, desc: &'static str) -> Self {
        Self {
            key,
            desc,
            is_primary: true,
        }
    }

    #[allow(dead_code)]
    pub const fn secondary(key: &'static str, desc: &'static str) -> Self {
        Self {
            key,
            desc,
            is_primary: false,
        }
    }

    pub const fn from_key(key: Key, desc: &'static str, is_primary: bool) -> Self {
        Self {
            key: key.display(),
            desc,
            is_primary,
        }
    }
}

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

    let (title, items): (&'static str, Vec<WhichKeyItem>) = match chord {
        LeaderChord::CtrlX => (
            "Ctrl+X (View / Window)",
            vec![
                WhichKeyItem::primary("b", "switch view / buffer"),
                WhichKeyItem::primary("k", "close current view"),
                WhichKeyItem::primary("o", "other pane / focus"),
                WhichKeyItem::from_key(Key::CTRL_C, "quit mutx", true),
                WhichKeyItem::from_key(Key::CTRL_G, "cancel", false),
            ],
        ),
        LeaderChord::CtrlC => (
            "Ctrl+C (Agent / Mode)",
            vec![
                WhichKeyItem::primary("c", "interrupt round"),
                WhichKeyItem::primary("p", "permissions modal"),
                WhichKeyItem::primary("t", "todos task list"),
                WhichKeyItem::primary("m", "models picker"),
                WhichKeyItem::primary("d", "performance report"),
                WhichKeyItem::from_key(Key::CTRL_G, "cancel", false),
            ],
        ),
        LeaderChord::None => return,
    };

    let card_width: u16 = 38;
    // 2 border rows + 1 top blank line + 1 title line + items + 1 bottom blank line
    let card_height: u16 = (items.len() as u16) + 5;

    // Position at bottom-right, 1 row above the terminal bottom
    let x = viewport.width.saturating_sub(card_width + 2).max(1);
    let y = viewport.height.saturating_sub(card_height + 1).max(1);

    let area = Rect::new(
        x,
        y,
        card_width.min(viewport.width.saturating_sub(x)),
        card_height,
    );

    // 1. Wipe underlying text cleanly with Clear widget
    frame.render_widget(Clear, area);

    // 2. Build card block
    let block = RtBlock::default()
        .borders(Borders::LEFT | Borders::RIGHT | Borders::TOP | Borders::BOTTOM)
        .border_type(mutx_engine::BorderType::Thick)
        .border_style(Style::default().fg(theme.brand()))
        .style(Style::default().bg(theme.panel()));

    // 3. Build action lines with top and bottom spacing
    let mut lines = Vec::with_capacity(items.len() + 4);
    lines.push(Line::default()); // Top empty line
    lines.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            title,
            Style::default()
                .fg(theme.brand())
                .add_modifier(mutx_engine::Modifier::BOLD),
        ),
    ]));
    for item in items {
        let key_span = keycap_span(theme, item.key);
        let pad = match item.key.len() {
            1 => "   ",
            2 => "  ",
            3 => " ",
            _ => " ",
        };
        let desc_style = if item.is_primary {
            theme.keycap_label_style()
        } else {
            Style::default().fg(theme.muted())
        };
        lines.push(Line::from(vec![
            Span::raw(" "),
            key_span,
            Span::raw(pad),
            Span::styled(item.desc, desc_style),
        ]));
    }
    lines.push(Line::default()); // Bottom empty line

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
        assert!(content.contains("Ctrl+X (View / Window)"));
        assert!(content.contains("switch view"));
        assert!(content.contains("cancel"));

        // Bottom-most row (row 23) should be empty (1 row margin from bottom)
        let row_23: String = terminal.buffer().content[23 * 80..24 * 80]
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(row_23.trim().is_empty());
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
        assert!(content.contains("Ctrl+C (Agent / Mode)"));
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
        assert!(!content.contains("Ctrl+X"));
        assert!(!content.contains("Ctrl+C"));
    }
}
