//! Help / keybindings modal.

use neenee_tui::{
    Frame, Modifier, Span, {Line, Style},
};

use crate::render::Theme;
use crate::render::components::modal::{ModalHeader, ModalPage, ModalPageSize, draw_modal_page};
use crate::render::components::scroll::ScrollBody;
use crate::render::primitives::{FixedModalSpec, FooterHint};

pub fn draw_help_modal(frame: &mut Frame, scroll: &mut usize, theme: &Theme) -> neenee_tui::Rect {
    let key = |k: &str| {
        Span::styled(
            format!("{:<10}", k),
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
        )
    };
    let desc = |d: &str| Span::styled(d.to_string(), Style::default().fg(theme.muted()));
    let section = |title: &str| {
        Span::styled(
            title.to_string(),
            Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
        )
    };
    let row = |k: &str, d: &str| Line::from(vec![key(k), desc(d)]);

    let body = vec![
        Line::from(section("General")),
        row("enter", "send message"),
        row("alt+enter", "insert newline (ctrl+j)"),
        row("esc", "interrupt (×2) / close"),
        row("ctrl+c", "copy · clear input · quit (×2)"),
        Line::from(""),
        Line::from(section("Line editing")),
        row("ctrl+a / ctrl+e", "caret to line start / end"),
        row("ctrl+b", "move back one char (←)"),
        row("home / end", "caret to line start / end"),
        row("ctrl+u / ctrl+k", "delete to line start / end"),
        row("ctrl+w", "delete previous word"),
        row("alt+backspace", "delete previous word"),
        row("alt+d", "delete next word"),
        row("ctrl+← / ctrl+→", "move word back / forward"),
        row("alt+b / alt+f", "move word back / forward"),
        Line::from(""),
        Line::from(section("Transcript focus")),
        Line::from(desc(
            "No modes: typing always lands in the prompt. Ctrl+↑/↓ highlights",
        )),
        Line::from(desc(
            "a step; the highlight tells you which keys act on it.",
        )),
        row("ctrl+↑ / ctrl+↓", "focus a step (nearest first)"),
        row("↑ / ↓", "while focused: cycle steps"),
        row("enter", "open the focused step"),
        row("esc", "clear the focus"),
        Line::from(""),
        Line::from(section("Views & tools")),
        row("? / f1 / ctrl+h", "this help"),
        row("/tools", "manage tools"),
        row("/skills", "browse skills"),
        row("/permissions", "manage permissions"),
        row("/config", "configuration"),
        row("ctrl+m", "switch model"),
        row("ctrl+r", "search history"),
        row("ctrl+t", "toggle tool steps"),
        row("/", "slash commands"),
        Line::from(""),
        Line::from(section("Modes")),
        row("/pursue", "pursue a pursuit until it is met"),
        Line::from(""),
        Line::from(desc("Drag to select · Ctrl+C or Ctrl+Shift+C to copy.")),
    ];
    draw_modal_page(
        frame,
        ModalPage {
            size: ModalPageSize::Fixed(FixedModalSpec::HELP),
            header: ModalHeader::title("Help"),
            body: ScrollBody {
                lines: body,
                scroll,
                follow: None,
                edge_margin: 0,
                wrap: true,
            },
            footer_hints: &[
                FooterHint::navigation("↑↓", "scroll"),
                FooterHint::always("Esc", "close"),
            ],
            extra_footer_hints: &[],
            keymap_open: false,
            // Help is already a keybindings surface — no recursive `? more`.
            show_more: false,
        },
        theme,
    )
}
