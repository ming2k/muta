//! Color-scheme picker reached from Settings › Appearance.

use neenee_core::ColorSchemeConfig;
use neenee_tui_engine::{
    Frame, Modifier, Style, {Line, Span},
};

use crate::tui::components::modal::{
    ModalHeader, ModalPage, ModalPageSize, draw_modal_page, modal_body_width,
};
use crate::tui::components::options::{ChoiceTone, choice_style};
use crate::tui::components::scroll::ScrollBody;
use crate::tui::primitives::{ContentModalSpec, FooterHint, HeaderPart, SCROLL_EDGE_MARGIN};
use crate::tui::view::{COLOR_SCHEMES, Theme};

pub const ROW_COUNT: usize = COLOR_SCHEMES.len();

pub fn scheme_id_at(index: usize) -> Option<&'static str> {
    COLOR_SCHEMES.get(index).map(|scheme| scheme.id)
}

pub fn draw_config_theme_modal(
    frame: &mut Frame,
    current: &str,
    custom: &ColorSchemeConfig,
    modal_index: usize,
    scroll: &mut usize,
    keymap_open: bool,
    theme: &Theme,
) -> neenee_tui_engine::Rect {
    let body_width = modal_body_width(frame, ContentModalSpec::CONFIG_THEME);
    let mut lines: Vec<Line<'static>> = vec![
        Line::from(Span::styled(
            "Choose a palette. Presets apply instantly; Custom opens the editor.",
            Style::default().fg(theme.muted()),
        )),
        Line::from(""),
    ];
    let current = Theme::normalize_color_scheme(current);
    let mut selected_line = None;

    for (index, scheme) in COLOR_SCHEMES.iter().enumerate() {
        let highlighted = index == modal_index;
        let active = scheme.id == current;
        let style = choice_style(ChoiceTone::Flat, highlighted, theme);
        if highlighted {
            selected_line = Some(lines.len());
        }
        let cursor = if highlighted { "›" } else { " " };
        let active_mark = if active { "●" } else { "○" };
        let colors = Theme::preview_colors(scheme.id, custom);
        let description_width = body_width.saturating_sub(3 + 2 + 12 + colors.len()).max(1);
        let description = if scheme.description.len() > description_width {
            &scheme.description[..description_width.saturating_sub(1)]
        } else {
            scheme.description
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {cursor} "),
                Style::default().fg(if highlighted {
                    theme.brand()
                } else {
                    theme.dim()
                }),
            ),
            Span::styled(
                format!("{active_mark} "),
                Style::default().fg(if active { theme.ok() } else { theme.dim() }),
            ),
            Span::styled(
                format!("{:<12}", scheme.label),
                Style::default().fg(style.fg).add_modifier(if highlighted {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
            ),
            Span::styled(
                format!("{description:<description_width$}"),
                Style::default().fg(style.dim),
            ),
            Span::styled("■", Style::default().fg(colors[0])),
            Span::styled("■", Style::default().fg(colors[1])),
            Span::styled("■", Style::default().fg(colors[2])),
            Span::styled("■", Style::default().fg(colors[3])),
            Span::styled("■", Style::default().fg(colors[4])),
        ]));
    }

    let header = [
        HeaderPart::Text {
            text: "Settings  /  ",
            accent: false,
        },
        HeaderPart::title("Appearance"),
    ];
    draw_modal_page(
        frame,
        ModalPage {
            size: ModalPageSize::Content(ContentModalSpec::CONFIG_THEME),
            header: ModalHeader::parts(&header),
            body: ScrollBody {
                lines,
                scroll,
                follow: selected_line,
                edge_margin: SCROLL_EDGE_MARGIN,
                wrap: false,
            },
            footer_hints: &[
                FooterHint::navigation("↑↓", "select"),
                FooterHint::primary("Enter/Space", "apply or edit"),
                FooterHint::always("Esc", "back"),
            ],
            extra_footer_hints: &[],
            keymap_open,
            show_more: true,
        },
        theme,
    )
}
