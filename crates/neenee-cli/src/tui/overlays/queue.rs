//! Queue modal — the full outbox overview.
//!
//! The persistent queue bar below the transcript gap shows a one-item preview
//! of the next staged message; this modal is its expand surface, opened by
//! clicking that bar or pressing `F2`. It lists every queued dispatch for the
//! viewed session in dispatch order (front pops first), each with its queued
//! time and (truncated) text. `↑`/`↓` move the highlight; `Enter` recalls the
//! newest staged item into the composer for editing (then closes); `Esc` /
//! outside-click closes.

use neenee_tui_engine::{
    Frame, {Line, Span},
};

use super::common::{placeholder, truncate_ellipsis};
use crate::tui::components::list::{SelectableListPage, draw_selectable_list_page, row_style};
use crate::tui::components::modal::ModalHeader;
use crate::tui::primitives::{ContentModalSpec, FooterHint, keyvocab};
use crate::tui::view::{QueueItemView, Theme};
use unicode_width::UnicodeWidthStr;

/// Inputs for [`draw_queue_modal`]. The items slice is the viewed session's
/// outbox in dispatch order; `paused` flags whether staged items are held
/// back (mirrors the queue bar).
pub struct QueueModalView<'a> {
    pub items: &'a [QueueItemView],
    pub paused: bool,
}

/// Draw the queue overview modal. Each row carries the queued-at time and the
/// item text truncated to the row budget (every staged message waits for the
/// next round, so there is no per-row target badge). The header shows the
/// total count (and a "paused" tag when items are held back).
#[allow(clippy::too_many_arguments)]
pub fn draw_queue_modal(
    frame: &mut Frame,
    view: QueueModalView<'_>,
    modal_index: usize,
    scroll: &mut usize,
    follow_selection: bool,
    theme: &Theme,
) -> neenee_tui_engine::Rect {
    let body_width =
        crate::tui::components::modal::modal_body_width(frame, ContentModalSpec::QUEUE);

    let QueueModalView { items, paused } = view;

    // Header title carries the live count so the bar's summary is echoed and
    // extended here: `Queue · 3` (or `Queue · 3 · paused`).
    let count = items.len();
    let title = if count == 0 {
        "Queue".to_string()
    } else if paused {
        format!("Queue · {count} · paused")
    } else {
        format!("Queue · {count}")
    };

    let mut body: Vec<Line> = Vec::new();
    let mut selected_line: Option<usize> = None;

    if items.is_empty() {
        body.push(placeholder(
            "No messages queued. Press Enter while the agent runs to stage one.",
            true,
            theme.muted(),
        ));
    } else {
        // Column geometry: gutter + time(HH:MM = 5) + 2-col gap + text. With
        // the insert/next-round distinction gone there is no per-row target
        // badge; selection is shown via the existing background row highlight,
        // matching the other list modals.
        const GUTTER_W: usize = 2;
        const TIME_W: usize = 5; // HH:MM
        let text_budget = body_width.saturating_sub(GUTTER_W + TIME_W + 2).max(1);

        for (i, item) in items.iter().enumerate() {
            let is_sel = i == modal_index;
            let style = row_style(is_sel, theme);
            if is_sel {
                selected_line = Some(body.len());
            }
            let time = crate::tui::time::sent_time_label(item.queued_at_ms);
            let one_line = super::common::one_line(item.text.trim());
            let text = if one_line.width() > text_budget {
                truncate_ellipsis(&one_line, text_budget)
            } else {
                one_line
            };
            let left_w = GUTTER_W + TIME_W + 2 + text.width();
            let pad = body_width.saturating_sub(left_w);
            body.push(Line::from(vec![
                Span::styled(
                    " ".repeat(GUTTER_W),
                    neenee_tui_engine::Style::default().bg(style.bg),
                ),
                Span::styled(
                    format!("{time}  "),
                    neenee_tui_engine::Style::default()
                        .bg(style.bg)
                        .fg(style.dim),
                ),
                Span::styled(
                    text,
                    neenee_tui_engine::Style::default()
                        .bg(style.bg)
                        .fg(style.fg),
                ),
                Span::styled(
                    " ".repeat(pad),
                    neenee_tui_engine::Style::default().bg(style.bg),
                ),
            ]));
        }
    }

    let has_items = !items.is_empty();
    draw_selectable_list_page(
        frame,
        SelectableListPage {
            geometry: ContentModalSpec::QUEUE,
            header: ModalHeader::title(&title),
            lines: body,
            scroll,
            selected_line,
            follow_selection,
            has_items,
            item_footer_hints: &[
                FooterHint::navigation(keyvocab::ARROWS_UD, "select"),
                FooterHint::primary(keyvocab::ENTER, "edit latest"),
                FooterHint::always(keyvocab::ESC, "close"),
            ],
            empty_footer_hints: &[FooterHint::always(keyvocab::ESC, "close")],
            extra_footer_hints: &[],
            keymap_open: false,
        },
        theme,
    )
}
