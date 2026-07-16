//! Inline hex editor for the custom color scheme.

use neenee_core::ColorSchemeConfig;
use neenee_tui::{
    Frame, Modifier, Style, {Line, Span},
};

use crate::render::components::footer::render_modal_footer;
use crate::render::primitives::{
    ContentModalSpec, FooterHint, HeaderPart, SCROLL_EDGE_MARGIN, content_modal_area,
    modal_chrome_rows, modal_frame, modal_header_parts, render_body,
};
use crate::render::{CUSTOM_COLOR_FIELDS, Theme};

pub const ROW_COUNT: usize = CUSTOM_COLOR_FIELDS.len();

pub fn draw_config_theme_custom_modal(
    frame: &mut Frame,
    draft: &ColorSchemeConfig,
    field_index: usize,
    input: &str,
    cursor_position: usize,
    scroll: &mut usize,
    theme: &Theme,
) -> neenee_tui::Rect {
    let field_index = field_index.min(ROW_COUNT.saturating_sub(1));
    let valid_input = Theme::color_from_hex(input).is_some();
    let preview = Theme::from_color_scheme("custom", draft);
    let mut lines: Vec<Line<'static>> = vec![
        Line::from(Span::styled(
            "Enter colors as #RRGGBB.",
            Style::default().fg(theme.muted()),
        )),
        Line::from(Span::styled(
            "Valid colors preview across the interface as you type.",
            Style::default().fg(theme.muted()),
        )),
        Line::from(""),
        preview_line(&preview),
        Line::from(""),
    ];
    let field_start = lines.len();

    for (index, field) in CUSTOM_COLOR_FIELDS.iter().enumerate() {
        let selected = index == field_index;
        let stored = Theme::custom_color_value(draft, index).unwrap_or("#000000");
        let shown = if selected { input } else { stored };
        let swatch = Theme::color_from_hex(shown).unwrap_or(theme.panel());
        let cursor = if selected { "›" } else { " " };
        let value_style = if selected && !valid_input {
            Style::default()
                .fg(theme.err())
                .add_modifier(Modifier::BOLD)
        } else if selected {
            Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted())
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {cursor} "),
                Style::default().fg(if selected { theme.brand() } else { theme.dim() }),
            ),
            Span::styled(
                format!("{:<13}", field.label),
                Style::default().fg(if selected { theme.brand() } else { theme.fg() }),
            ),
            Span::styled("  ", Style::default().bg(swatch)),
            Span::raw("  "),
            Span::styled(format!("{:<10}", shown), value_style),
            Span::styled(field.hint.to_string(), Style::default().fg(theme.dim())),
        ]));
    }

    if !valid_input {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  Enter six hex digits, for example #7aa2f7.",
            Style::default().fg(theme.err()),
        )));
    }

    let spec = ContentModalSpec::CONFIG_THEME_CUSTOM;
    let desired = lines.len() as u16 + modal_chrome_rows(spec.modal_spec());
    let area = content_modal_area(frame, spec, desired);
    let modal = modal_frame(frame, area, theme.panel(), true, true);
    let header = [
        HeaderPart::Text {
            text: "Appearance  /  ",
            accent: false,
        },
        HeaderPart::title("Custom palette"),
    ];
    modal_header_parts(frame, modal.header, &header, theme);

    let selected_line = field_start + field_index;
    render_body(
        frame,
        modal.body,
        lines,
        scroll,
        Some(selected_line),
        SCROLL_EDGE_MARGIN,
        false,
        theme,
    );
    if let Some(footer) = modal.footer {
        render_modal_footer(
            frame,
            footer,
            &[
                FooterHint::secondary("Tab/↑↓", "field"),
                FooterHint::primary("Enter", "save"),
                FooterHint::always("Esc", "cancel"),
            ],
            theme,
        );
    }

    // The selected input is short by contract, but clamp defensively while it
    // is temporarily invalid so arbitrary pasted text cannot escape the panel.
    let visible_row = selected_line.saturating_sub(*scroll) as u16;
    if visible_row < modal.body.height {
        const VALUE_COLUMN: u16 = 20;
        let x = (modal.body.x + VALUE_COLUMN + cursor_position as u16)
            .min(modal.body.x + modal.body.width.saturating_sub(1));
        frame.set_cursor_position((x, modal.body.y + visible_row));
    }
    area
}

fn preview_line(theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            " Preview ",
            Style::default().bg(theme.input_surface()).fg(theme.fg()),
        ),
        Span::raw("  "),
        Span::styled(
            " Accent ",
            Style::default().bg(theme.brand()).fg(theme.surface()),
        ),
        Span::raw("  "),
        Span::styled("success", Style::default().fg(theme.ok())),
        Span::raw("  "),
        Span::styled("warning", Style::default().fg(theme.warn())),
        Span::raw("  "),
        Span::styled("error", Style::default().fg(theme.err())),
    ])
}
