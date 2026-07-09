//! Doom-guard sub-page of the config manager modal.
//!
//! Reached from [`super::config`] by selecting the "Nudge" row. Shows the
//! master `enabled` switch and the `window` size. `Space` toggles the
//! enabled flag; `←`/`→` adjust the window; `Esc` returns to the config root.
//! Edits are sent as `AgentRequest::UpdateDoomGuardConfig` and the harness
//! replies with `AgentResponse::DoomGuardConfigUpdated`, which re-seeds the
//! snapshot the modal reads.

use neenee_core::DoomGuardConfig;
use neenee_tui::{
    Frame, Style, {Line, Span},
};

use crate::modal::Modal;
use crate::render::Theme;
use crate::render::components::modal::{ModalHeader, ModalPage, ModalPageSize, draw_modal_page};
use crate::render::components::options::{ChoiceStyle, ChoiceTone, choice_style};
use crate::render::components::scroll::ScrollBody;
use crate::render::design::MODAL_INNER_H_PADDING;
use crate::render::primitives::{FooterHint, HeaderPart, SCROLL_EDGE_MARGIN, content_modal_probe};

/// Row index of the `enabled` toggle in the field list. `Space` only toggles
/// when this row is selected; the `window` row responds to `←`/`→` instead.
pub const ROW_ENABLED: usize = 0;
/// Row index of `window`.
pub const ROW_WINDOW: usize = 1;

/// Total number of rows in the doom-guard sub-page (enabled + window).
pub const ROW_COUNT: usize = 2;

/// Draw the doom-guard sub-page modal. `modal_index` is the selection cursor;
/// `config` is the live snapshot from the harness. The caller sends
/// `AgentRequest::UpdateDoomGuardConfig` when the user edits a field; the
/// harness reply re-seeds the snapshot so this renderer always reads the
/// authoritative state.
pub fn draw_config_nudge_modal(
    frame: &mut Frame,
    config: &DoomGuardConfig,
    modal_index: usize,
    scroll: &mut usize,
    theme: &Theme,
) -> neenee_tui::Rect {
    let probe =
        content_modal_probe(frame, Modal::ConfigNudge).expect("config nudge modal geometry");
    let body_width = (probe.width as usize)
        .saturating_sub(2 * MODAL_INNER_H_PADDING as usize)
        .max(1);

    let mut body: Vec<Line> = Vec::new();
    let mut selected_line: Option<usize> = None;

    // A one-line description of the whole sub-page, rendered before the
    // field list. Muted, not selectable.
    body.push(Line::from(Span::styled(
        "Blocks a tool call before it runs when the same call was already issued \
         this turn — interrupts doom loops of bash/read/edit/webfetch. Default is off.",
        Style::default().fg(theme.muted()),
    )));
    body.push(Line::from(""));

    // ── field rows ──
    const GUTTER_W: usize = 2;
    const PREFIX_W: usize = GUTTER_W + 2; // gutter + glyph
    let name_col = 16usize;
    let val_col = 8usize;

    let rows: [(String, String); ROW_COUNT] = [
        (
            "enabled".to_string(),
            if config.enabled {
                "on".to_string()
            } else {
                "off".to_string()
            },
        ),
        ("window".to_string(), config.window.to_string()),
    ];

    for (i, (name, val)) in rows.iter().enumerate() {
        let row_idx = i; // 0-based within the field list
        let is_sel = row_idx == modal_index;
        let s: ChoiceStyle = choice_style(ChoiceTone::Filled, is_sel, theme);
        let glyph = if is_sel { "▸" } else { " " };
        let desc = field_hint(name);
        let desc_budget = body_width
            .saturating_sub(PREFIX_W + name_col + val_col + 4)
            .max(1);
        let desc_truncated = if desc.len() > desc_budget {
            &desc[..desc_budget.saturating_sub(1)]
        } else {
            desc
        };
        let pad =
            body_width.saturating_sub(PREFIX_W + name_col + val_col + 4 + desc_truncated.len());
        if is_sel {
            selected_line = Some(body.len());
        }
        // For the enabled row, render a [on]/[off] badge instead of a bare
        // value, so the toggle affordance is visually distinct from a number.
        let val_display = if name == "enabled" {
            format!("[{val}]")
        } else {
            format!(" {val}")
        };
        body.push(Line::from(vec![
            Span::styled(" ".repeat(GUTTER_W), Style::default().bg(s.bg)),
            Span::styled(format!("{glyph} "), Style::default().bg(s.bg).fg(s.fg)),
            Span::styled(
                format!("{:<w$}", name, w = name_col),
                Style::default().bg(s.bg).fg(s.fg),
            ),
            Span::styled(
                format!("{:>w$}", val_display, w = val_col),
                Style::default().bg(s.bg).fg(s.dim),
            ),
            Span::styled(
                format!("  {desc_truncated}"),
                Style::default().bg(s.bg).fg(s.dim),
            ),
            Span::styled(" ".repeat(pad), Style::default().bg(s.bg)),
        ]));
    }

    let header = [
        HeaderPart::Text {
            text: "← ",
            accent: false,
        },
        HeaderPart::title("Doom Guard"),
    ];
    let hints: &[FooterHint] = if modal_index == ROW_ENABLED {
        &[
            FooterHint::navigation("↑↓", "select"),
            FooterHint::primary("Space", "toggle"),
            FooterHint::always("Esc", "back"),
        ]
    } else {
        &[
            FooterHint::navigation("↑↓", "select"),
            FooterHint::primary("←→", "adjust"),
            FooterHint::always("Esc", "back"),
        ]
    };
    draw_modal_page(
        frame,
        ModalPage {
            modal: Modal::ConfigNudge,
            size: ModalPageSize::Content,
            header: ModalHeader::parts(&header),
            body: ScrollBody {
                lines: body,
                scroll,
                follow: selected_line,
                edge_margin: SCROLL_EDGE_MARGIN,
                wrap: false,
            },
            footer_hints: hints,
            extra_footer_hints: &[],
            keymap_open: false,
            show_more: false,
        },
        theme,
    )
}

/// Short per-field hint shown to the right of the value.
fn field_hint(name: &str) -> &'static str {
    match name {
        "enabled" => "master switch",
        "window" => "sliding-window size (recent watched rounds)",
        _ => "",
    }
}

/// Apply a ±1 delta to the value at `row_index` in the doom-guard sub-page.
/// Row 0 (`enabled`) is not adjustable and must be excluded by the caller.
/// `window` is clamped to `>= 1`.
pub fn apply_threshold_delta(config: &mut DoomGuardConfig, row_index: usize, delta: i32) {
    let clamp_usize = |v: usize, d: i32| (v as i32 + d).max(1) as usize;
    if row_index == ROW_WINDOW {
        config.window = clamp_usize(config.window, delta);
    }
}
