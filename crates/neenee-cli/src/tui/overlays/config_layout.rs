//! Transcript layout sub-page of the config manager modal.
//!
//! Reached from [`super::config`] by selecting the "Layout" row. Lists the
//! layout strategies; the active one is marked and highlighted. `Space` /
//! `Enter` applies the selected strategy — sent as
//! `AgentRequest::UpdateTuiLayout`, persisted to `config.toml`, and the
//! harness replies with `AgentResponse::TuiLayoutUpdated` which re-seeds
//! `App::transcript_layout`(App::transcript_layout). `Esc`
//! returns to the config root.

use neenee_tui_engine::{
    Frame, Modifier, Style, {Line, Span},
};

use crate::tui::components::modal::{ModalHeader, ModalPage, ModalPageSize, draw_modal_page};
use crate::tui::components::options::{ChoiceStyle, ChoiceTone, choice_style};
use crate::tui::components::scroll::ScrollBody;
use crate::tui::design::MODAL_INNER_H_PADDING;
use crate::tui::layout::Strategy;
use crate::tui::primitives::{
    ContentModalSpec, FooterHint, HeaderPart, SCROLL_EDGE_MARGIN, content_modal_probe,
};
use crate::tui::view::Theme;

/// One selectable layout strategy + its human description.
struct LayoutOption {
    /// The canonical config-string this row maps to (written verbatim to
    /// `config.toml`). Matches [`Strategy::from_config`]'s accepted spellings.
    config_value: &'static str,
    label: &'static str,
    description: &'static str,
}

/// The static option list. **Order matters**: `modal_index` selects by
/// position, and `apply_index` maps an index back to a `config_value`.
fn options() -> [LayoutOption; 2] {
    [
        LayoutOption {
            config_value: "default",
            label: "Round-band",
            description: "Each tool round grouped under a labelled header",
        },
        LayoutOption {
            config_value: "legacy",
            label: "Legacy",
            description: "Original flush stack: tight gaps, batched tool calls",
        },
    ]
}

/// The canonical config-string for the option at `index`, for the apply path.
pub fn config_value_at(index: usize) -> Option<&'static str> {
    options().get(index).map(|o| o.config_value)
}

/// Number of selectable rows.
pub const ROW_COUNT: usize = 2;

/// Draw the layout sub-page modal. `modal_index` is the selection cursor;
/// `current` is the live [`Strategy`] from `App.transcript_layout`, used to
/// mark the active option. The caller sends `AgentRequest::UpdateTuiLayout`
/// when the user applies a choice.
pub fn draw_config_layout_modal(
    frame: &mut Frame,
    current: Strategy,
    modal_index: usize,
    scroll: &mut usize,
    keymap_open: bool,
    theme: &Theme,
) -> neenee_tui_engine::Rect {
    let probe = content_modal_probe(frame, ContentModalSpec::CONFIG_LAYOUT);
    let body_width = (probe.width as usize)
        .saturating_sub(2 * MODAL_INNER_H_PADDING as usize)
        .max(1);

    let mut body: Vec<Line> = Vec::new();
    let mut selected_line: Option<usize> = None;

    // One-line description of the sub-page, rendered before the option list.
    // Muted, not selectable.
    body.push(Line::from(Span::styled(
        "How the transcript arranges tool rounds. Round-band groups each \
         model request under a header so the history reads as discrete steps.",
        Style::default().fg(theme.muted()),
    )));
    body.push(Line::from(""));

    const GUTTER_W: usize = 2;
    const PREFIX_W: usize = GUTTER_W + 2; // gutter + glyph

    let current_value = match current {
        Strategy::Default => "default",
        Strategy::Legacy => "legacy",
    };

    for (i, opt) in options().iter().enumerate() {
        let is_sel = i == modal_index;
        let is_active = opt.config_value == current_value;
        let s: ChoiceStyle = choice_style(ChoiceTone::Flat, is_sel, theme);
        let glyph = if is_sel { "›" } else { " " };
        let mark = if is_active { "● " } else { "○ " };

        let label_w = 12usize;
        let desc_budget = body_width
            .saturating_sub(PREFIX_W + label_w + 2 + mark.len())
            .max(1);
        let desc = opt.description;
        let desc_truncated = if desc.len() > desc_budget {
            &desc[..desc_budget.saturating_sub(1)]
        } else {
            desc
        };
        let pad =
            body_width.saturating_sub(PREFIX_W + label_w + 2 + mark.len() + desc_truncated.len());

        if is_sel {
            selected_line = Some(body.len());
        }
        body.push(Line::from(vec![
            Span::styled(" ".repeat(GUTTER_W), Style::default().bg(s.bg)),
            Span::styled(format!("{glyph} "), Style::default().bg(s.bg).fg(s.fg)),
            Span::styled(
                format!("{:<w$}", opt.label, w = label_w),
                Style::default()
                    .bg(s.bg)
                    .fg(s.fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(mark, Style::default().bg(s.bg).fg(s.dim)),
            Span::styled(desc_truncated, Style::default().bg(s.bg).fg(s.dim)),
            Span::styled(" ".repeat(pad), Style::default().bg(s.bg)),
        ]));
    }

    // Footnote: what the active setting currently is.
    body.push(Line::from(""));
    body.push(Line::from(Span::styled(
        format!("Active: {}", current_value),
        Style::default().fg(theme.muted()),
    )));

    let header = [
        HeaderPart::Text {
            text: "Settings  /  ",
            accent: false,
        },
        HeaderPart::title("Layout"),
    ];
    draw_modal_page(
        frame,
        ModalPage {
            size: ModalPageSize::Content(ContentModalSpec::CONFIG_LAYOUT),
            header: ModalHeader::parts(&header),
            body: ScrollBody {
                lines: body,
                scroll,
                follow: selected_line,
                edge_margin: SCROLL_EDGE_MARGIN,
                wrap: false,
            },
            footer_hints: &[
                FooterHint::navigation("↑↓", "select"),
                FooterHint::primary("Enter/Space", "apply"),
                FooterHint::always("Esc", "back"),
            ],
            extra_footer_hints: &[],
            keymap_open,
            show_more: true,
        },
        theme,
    )
}
