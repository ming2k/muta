//! Transient overlay toast bubbles.
//!
//! # Architecture & Scope Contract
//! Toast bubbles are ephemeral top-right overlays rendered at layer 3 above other chrome.
//!
//! ### Strictly Reserved For:
//! 1. Immediate action acknowledgments ("copied to clipboard", "input cleared").
//! 2. Two-step confirmation gates ("Esc again interrupts", "press Ctrl+C again to exit").
//! 3. Slash command acknowledgments (`CommandAck`, e.g. `/delegate on`) that must not pollute the transcript.
//!
//! ### Prohibited For Toasts:
//! Diagnostic errors, missing workspace assets, and initialization failures MUST route to
//! `NoticeSurface::Inline` (in the transcript timeline) rather than `Toast`.
//!
//! ### Layout Invariants:
//! - **Compact Zero-Padding**: No leading or trailing empty rows. Screen height in TUI is scarce;
//!   every row displays content (`height = rendered_lines.len()`).
//! - **Symmetric Horizontal Inset**: Every line has uniform 1-space leading and trailing breathing space.
//! - **Shrink-to-Fit Width**: Width tightly bounds the longest wrapped line, clamped between
//!   [`MIN_TOAST_WIDTH`] and [`MAX_TOAST_WIDTH`].

use mutx_engine::{
    Block as RtBlock, Borders, Color, Frame, Modifier, Paragraph, Rect, Span, {Line, Style},
};
use unicode_width::UnicodeWidthStr;

use crate::text_layout::wrap_text;

use super::super::Theme;

pub(crate) const MIN_TOAST_WIDTH: u16 = 16;
pub(crate) const MAX_TOAST_WIDTH: u16 = 60;
pub(crate) const MAX_TOAST_ROWS: usize = 6;

pub(crate) enum ToastKind {
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

pub(crate) struct ToastBubble<'a> {
    pub message: &'a str,
    pub kind: ToastKind,
}

impl<'a> ToastBubble<'a> {
    pub(crate) fn render(self, frame: &mut Frame, theme: &Theme) {
        let size = frame.area();
        self.render_at_width(frame, theme, size.width);
    }

    pub(crate) fn render_at_width(self, frame: &mut Frame, theme: &Theme, width: u16) {
        let color = self.kind.color(theme);
        draw_toast(frame, theme, self.message, color, width);
    }
}

pub(crate) fn draw_toast(
    frame: &mut Frame,
    theme: &Theme,
    message: &str,
    color: Color,
    width: u16,
) {
    let clean = message.trim();
    if clean.is_empty() {
        return;
    }

    // Usable width on the terminal: reserve at least 2 columns on the right
    // margin and 2 columns on the left margin.
    let max_toast_w = (width.saturating_sub(4) as usize)
        .min(MAX_TOAST_WIDTH as usize)
        .max(MIN_TOAST_WIDTH as usize);

    // Text budget per row: subtract 2 columns for borders (┃ on left, ┃ on right)
    // and 2 columns for horizontal padding (1 space left, 1 space right).
    let text_budget = max_toast_w.saturating_sub(4).max(1);

    let logical_lines: Vec<&str> = clean.lines().collect();
    let is_multiline = logical_lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .count()
        > 1;

    let mut rendered_lines: Vec<(String, Style)> = Vec::new();
    let mut truncated = false;

    for (idx, line_str) in logical_lines.iter().enumerate() {
        let trimmed = line_str.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Title (first non-empty logical line) is rendered bold in foreground color.
        // Detail / subsequent lines use muted text for clear visual hierarchy.
        let style = if idx == 0 || !is_multiline {
            Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted())
        };

        let wrapped = wrap_text(trimmed, text_budget);
        for wl in wrapped {
            if rendered_lines.len() >= MAX_TOAST_ROWS {
                truncated = true;
                break;
            }
            rendered_lines.push((wl.text, style));
        }
        if truncated {
            break;
        }
    }

    if rendered_lines.is_empty() {
        return;
    }

    if truncated && let Some((last_text, _)) = rendered_lines.last_mut() {
        if last_text.width() < text_budget {
            last_text.push('…');
        } else {
            while last_text.width() + 1 > text_budget && !last_text.is_empty() {
                last_text.pop();
            }
            last_text.push('…');
        }
    }

    let max_content_w = rendered_lines
        .iter()
        .map(|(text, _)| text.width())
        .max()
        .unwrap_or(0);

    // Dynamic width: content width + 2 border columns + 2 padding columns,
    // clamped between MIN_TOAST_WIDTH and max_toast_w.
    let toast_width = (max_content_w as u16 + 4).clamp(MIN_TOAST_WIDTH, max_toast_w as u16);
    let x = width.saturating_sub(toast_width).saturating_sub(2).max(1);
    let toast_height = rendered_lines.len() as u16;
    let area = Rect::new(x, 1, toast_width, toast_height);

    let block = RtBlock::default()
        .borders(Borders::LEFT | Borders::RIGHT)
        .border_type(mutx_engine::BorderType::Thick)
        .border_style(Style::default().fg(color))
        .style(Style::default().bg(theme.panel()));

    let lines: Vec<Line> = rendered_lines
        .into_iter()
        .map(|(text, style)| Line::from(vec![Span::raw(" "), Span::styled(text, style)]))
        .collect();

    let para = Paragraph::new(lines);
    frame.render_widget(para.block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use mutx_engine::TestTerminal;

    fn grid_row(terminal: &TestTerminal, y: u16) -> String {
        let buffer = terminal.buffer();
        let width = buffer.area().width;
        (0..width).map(|x| buffer[(x, y)].symbol()).collect()
    }

    #[test]
    fn single_line_toast_renders_compactly_without_empty_rows() {
        let theme = Theme::default();
        let mut terminal = TestTerminal::new(80, 10);
        terminal.draw(|f| {
            draw_toast(f, &theme, "copied to clipboard", theme.ok(), 80);
        });

        // y=0: outside toast (no border)
        let row_0 = grid_row(&terminal, 0);
        assert!(!row_0.contains('┃'), "row 0 should not contain border");

        // y=1: single content row, compact zero-padding
        let row_1 = grid_row(&terminal, 1);
        assert!(row_1.contains('┃'), "row 1 should contain border: {row_1}");
        assert!(
            row_1.contains(" copied to clipboard "),
            "row 1 has content with symmetric 1-space padding: {row_1}"
        );

        // y=2: outside toast (zero trailing blank row!)
        let row_2 = grid_row(&terminal, 2);
        assert!(
            !row_2.contains('┃'),
            "row 2 should not contain border: {row_2}"
        );
    }

    #[test]
    fn multiline_toast_renders_compactly_with_consistent_padding() {
        let theme = Theme::default();
        let mut terminal = TestTerminal::new(80, 10);
        let msg =
            "Some additional workspace roots could not be loaded\nSkipped roots: `../opencode`";
        terminal.draw(|f| {
            draw_toast(f, &theme, msg, theme.warn(), 80);
        });

        // y=0: empty
        let row_0 = grid_row(&terminal, 0);
        assert!(!row_0.contains('┃'), "row 0 must not be a stray blank row");

        // y=1: Title line with 1-space leading pad (no blank row on top!)
        let row_1 = grid_row(&terminal, 1);
        assert!(row_1.contains('┃'), "row 1 should have border: {row_1}");
        assert!(
            row_1.contains(" Some additional workspace roots could not be loaded "),
            "row 1 must have symmetric padding: {row_1}"
        );

        // y=2: Detail line with matching 1-space leading pad (not glued to border!)
        let row_2 = grid_row(&terminal, 2);
        assert!(row_2.contains('┃'), "row 2 should have border: {row_2}");
        assert!(
            row_2.contains(" Skipped roots: `../opencode`"),
            "row 2 must have leading space padding: {row_2}"
        );

        // y=3: outside toast (no trailing blank row!)
        let row_3 = grid_row(&terminal, 3);
        assert!(
            !row_3.contains('┃'),
            "row 3 should not contain border: {row_3}"
        );
    }

    #[test]
    fn long_line_toast_wraps_within_max_bounds() {
        let theme = Theme::default();
        let mut terminal = TestTerminal::new(80, 10);
        let long_msg = "This is a very long notification message that definitely exceeds the text budget and must wrap cleanly across multiple lines without overflowing or getting cut off abruptly.";
        terminal.draw(|f| {
            draw_toast(f, &theme, long_msg, theme.info(), 80);
        });

        // Content rows start immediately at y=1 (compact)
        let row_1 = grid_row(&terminal, 1);
        let row_2 = grid_row(&terminal, 2);
        assert!(row_1.contains('┃'), "row 1 should have border");
        assert!(row_2.contains('┃'), "row 2 should have border");
        // Each wrapped line must have the 1-space indent
        assert!(
            row_1.contains(" This is a very long"),
            "row 1 starts with space: {row_1}"
        );
        assert!(row_2.contains('┃'), "row 2 enclosed in border: {row_2}");
    }
}
