//! Config manager modal — the root settings overlay.
//!
//! Opened via the `/config` slash command. Lists the configurable categories
//! (Nudge, …) as selectable rows; `Enter` / `Space` drills into a category's
//! sub-page ([`super::config_nudge`] for the Nudge sub-page). `Esc` closes.
//!
//! The category list is static for now — as more configurable surfaces are
//! added (compaction, hooks, permissions defaults, …), they each get a row
//! here and a dedicated sub-page module under `overlays/`.

use neenee_tui::{
    Frame, Style, {Line, Span},
};

use crate::render::Theme;
use crate::render::components::list::{SelectableListPage, draw_selectable_list_page, row_style};
use crate::render::components::modal::{ModalHeader, modal_body_width};
use crate::render::primitives::{ContentModalSpec, FooterHint};

/// One configurable category row in the config root modal.
struct ConfigCategory {
    label: &'static str,
    description: &'static str,
}

/// The static category list. As more configurable surfaces are added, append
/// here and create a matching sub-page module.
///
/// **Index matters**: the `ConfigActivate` handler dispatches on `modal_index`
/// (0 = Nudge, 1 = Layout). Keep this order in sync with that match.
fn categories() -> Vec<ConfigCategory> {
    vec![
        ConfigCategory {
            label: "Nudge",
            description: "Read-loop guard: thresholds and master switch",
        },
        ConfigCategory {
            label: "Layout",
            description: "Transcript round grouping & spacing",
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
    theme: &Theme,
) -> neenee_tui::Rect {
    let body_width = modal_body_width(frame, ContentModalSpec::CONFIG);

    let cats = categories();

    let mut body: Vec<Line> = Vec::new();
    let mut selected_line: Option<usize> = None;

    const GUTTER_W: usize = 2;
    const PREFIX_W: usize = GUTTER_W + 2; // gutter + glyph
    let label_col = 12usize;

    for (i, cat) in cats.iter().enumerate() {
        let is_sel = i == modal_index;
        let style = row_style(is_sel, theme);
        let glyph = if is_sel { "▸" } else { " " };
        let desc = cat.description;
        let desc_budget = body_width.saturating_sub(PREFIX_W + label_col + 2).max(1);
        let desc_truncated = if desc.len() > desc_budget {
            &desc[..desc_budget.saturating_sub(1)]
        } else {
            desc
        };
        let pad = body_width.saturating_sub(PREFIX_W + label_col + 2 + desc_truncated.len());
        if is_sel {
            selected_line = Some(body.len());
        }
        body.push(Line::from(vec![
            Span::styled(" ".repeat(GUTTER_W), Style::default().bg(style.bg)),
            Span::styled(
                format!("{glyph} "),
                Style::default().bg(style.bg).fg(style.fg),
            ),
            Span::styled(
                format!("{:<w$}", cat.label, w = label_col),
                Style::default().bg(style.bg).fg(style.fg),
            ),
            Span::styled(
                format!("  {desc_truncated}"),
                Style::default().bg(style.bg).fg(style.dim),
            ),
            Span::styled(" ".repeat(pad), Style::default().bg(style.bg)),
        ]));
    }

    draw_selectable_list_page(
        frame,
        SelectableListPage {
            geometry: ContentModalSpec::CONFIG,
            header: ModalHeader::title("Configuration"),
            lines: body,
            scroll,
            selected_line,
            follow_selection: true,
            has_items: !cats.is_empty(),
            item_footer_hints: &[
                FooterHint::navigation("↑↓", "select"),
                FooterHint::primary("Enter", "open"),
                FooterHint::always("Esc", "close"),
            ],
            empty_footer_hints: &[FooterHint::always("Esc", "close")],
            extra_footer_hints: &[],
            keymap_open: false,
        },
        theme,
    )
}
