//! Config manager modal — the root settings overlay.
//!
//! Opened via the `/config` slash command. Presents a flat settings index with
//! current values visible at a glance; `Enter` / `Space` drills into a page.
//!
//! The category list is static for now — as more configurable surfaces are
//! added (compaction, hooks, permissions defaults, …), they each get a row
//! here and a dedicated sub-page module under `overlays/`.

use neenee_tui_engine::{
    Frame, Style, {Line, Span},
};

use crate::components::list::{SelectableListPage, draw_selectable_list_page};
use crate::components::modal::{ModalHeader, modal_body_width};
use crate::components::options::{ChoiceTone, choice_style};
use crate::layout::Strategy;
use crate::primitives::{ContentModalSpec, FooterHint, keyvocab};
use crate::view::Theme;

/// One configurable category row in the config root modal.
struct ConfigCategory {
    label: &'static str,
    description: &'static str,
    value: String,
}

/// Current values summarized on the settings index.
#[derive(Clone, Copy)]
pub struct ConfigOverview<'a> {
    pub color_scheme: &'a str,
    pub layout: Strategy,
}

/// The static category list. As more configurable surfaces are added, append
/// here and create a matching sub-page module.
///
/// **Index matters**: the `ConfigActivate` handler dispatches on `modal_index`.
fn categories(color_scheme: &str, layout: Strategy) -> Vec<ConfigCategory> {
    vec![
        ConfigCategory {
            label: "Appearance",
            description: "Color scheme, previews, and custom palette",
            value: Theme::color_scheme_label(color_scheme).to_string(),
        },
        ConfigCategory {
            label: "Layout",
            description: "Transcript grouping and vertical rhythm",
            value: match layout {
                Strategy::Default => "Turn-band",
                Strategy::Legacy => "Legacy",
            }
            .to_string(),
        },
    ]
}

/// Draw the config root modal: a centered, dismissable, selectable list of
/// configurable categories. Each row shows the category name, a short
/// description, and a `›` drill-in affordance. `Enter` / `Space` opens the
/// selected category's sub-page; `Esc` closes.
pub fn draw_config_modal(
    frame: &mut Frame,
    modal_index: usize,
    scroll: &mut usize,
    overview: ConfigOverview<'_>,
    keymap_open: bool,
    theme: &Theme,
) -> neenee_tui_engine::Rect {
    let body_width = modal_body_width(frame, ContentModalSpec::CONFIG);

    let cats = categories(overview.color_scheme, overview.layout);

    let mut body: Vec<Line<'static>> = vec![
        Line::from(Span::styled(
            "Tune neenee without leaving the conversation.",
            Style::default().fg(theme.muted()),
        )),
        Line::from(""),
    ];
    let mut selected_line: Option<usize> = None;

    for (i, cat) in cats.iter().enumerate() {
        let is_sel = i == modal_index;
        let style = choice_style(ChoiceTone::Flat, is_sel, theme);
        let glyph = if is_sel { "›" } else { " " };
        let prefix_width = 4usize;
        let value_width = cat.value.len();
        let gap = body_width
            .saturating_sub(prefix_width + cat.label.len() + value_width)
            .max(1);
        let value_color = if is_sel { theme.brand() } else { theme.muted() };
        if is_sel {
            selected_line = Some(body.len());
        }
        body.push(Line::from(vec![
            Span::styled(
                format!(" {glyph}  "),
                Style::default().fg(if is_sel { theme.brand() } else { theme.dim() }),
            ),
            Span::styled(
                cat.label.to_string(),
                Style::default().fg(style.fg).add_modifier(if is_sel {
                    neenee_tui_engine::Modifier::BOLD
                } else {
                    neenee_tui_engine::Modifier::empty()
                }),
            ),
            Span::raw(" ".repeat(gap)),
            Span::styled(cat.value.clone(), Style::default().fg(value_color)),
        ]));

        let desc_budget = body_width.saturating_sub(prefix_width).max(1);
        let desc = if cat.description.len() > desc_budget {
            &cat.description[..desc_budget.saturating_sub(1)]
        } else {
            cat.description
        };
        body.push(Line::from(vec![
            Span::raw(" ".repeat(prefix_width)),
            Span::styled(desc.to_string(), Style::default().fg(style.dim)),
        ]));
        if i + 1 < cats.len() {
            body.push(Line::from(""));
        }
    }

    draw_selectable_list_page(
        frame,
        SelectableListPage {
            geometry: ContentModalSpec::CONFIG,
            header: ModalHeader::title("Settings"),
            lines: body,
            scroll,
            selected_line,
            follow_selection: true,
            has_items: !cats.is_empty(),
            item_footer_hints: &[
                FooterHint::navigation(keyvocab::ARROWS_UD, "select"),
                FooterHint::primary(keyvocab::ENTER, "open"),
                FooterHint::always(keyvocab::ESC, "close"),
            ],
            empty_footer_hints: &[FooterHint::always(keyvocab::ESC, "close")],
            extra_footer_hints: &[],
            keymap_open,
        },
        theme,
    )
}
