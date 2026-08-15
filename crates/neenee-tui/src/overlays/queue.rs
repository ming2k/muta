//! Queue modal — the full outbox overview.
//!
//! The persistent queue bar below the transcript gap shows a one-line summary
//! (identity, next-item preview, key legend); this modal is its expand
//! surface, opened by clicking that bar or pressing `F2`. It lists every
//! queued dispatch for the viewed session in dispatch order (front pops
//! first), each with its queued time and (truncated) text.
//!
//! Opening the modal **auto-blocks** the outbox (no message auto-drains) so
//! the list can be managed safely: `↑`/`↓` move the highlight, `Enter` re-edits
//! the *selected* item (pulls it back to the composer), `D` deletes it, `J`/`K`
//! reorder it, and `F3` toggles the persistent block. Closing the modal (Esc /
//! outside-click) resumes normal auto-drain. `Esc` / outside-click close.
//!
//! Only **manageable** (next-round, `Waiting`) items are listed: in-flight
//! mid-round steers (`F4`, shown in the queue bar with a `steer›` badge) are
//! already with the running round, so they cannot be re-edited, deleted, or
//! reordered here — a race loss returns them to this list as ordinary
//! entries, at which point they become manageable again.

use neenee_tui_engine::{
    Frame, {Line, Span},
};

use super::common::{placeholder, truncate_ellipsis};
use crate::components::list::{SelectableListPage, draw_selectable_list_page, row_style};
use crate::components::modal::ModalHeader;
use crate::primitives::{ContentModalSpec, FooterHint, keyvocab};
use crate::view::{QueueItemView, Theme};
use unicode_width::UnicodeWidthStr;

/// Inputs for [`draw_queue_modal`]. The items slice is the viewed session's
/// outbox in dispatch order.
pub struct QueueModalView<'a> {
    pub items: &'a [QueueItemView],
    /// `true` while the outbox is hard-blocked (the modal auto-blocks on open
    /// for safe editing; `F3` toggles a persistent block). The block state is
    /// explicit in the persistent queue bar, so the modal title stays plain.
    pub blocked: bool,
}

/// Draw the queue overview modal. Each row carries the queued-at time and the
/// item text truncated to the row budget. The header is a plain `Queue` title
/// — count and held-back state already live in the queue bar.
#[allow(clippy::too_many_arguments)]
pub fn draw_queue_modal(
    frame: &mut Frame,
    view: QueueModalView<'_>,
    modal_index: usize,
    scroll: &mut usize,
    follow_selection: bool,
    theme: &Theme,
) -> neenee_tui_engine::Rect {
    let body_width = crate::components::modal::modal_body_width(frame, ContentModalSpec::QUEUE);

    let QueueModalView { items, blocked } = view;

    // Keep the modal title a plain `Queue`: the count and the paused/blocked
    // state are already visible in the persistent queue bar, so echoing them
    // in the header only adds noise. The footer still surfaces `F3 block /
    // resume` where the block state matters.
    let title = "Queue";

    let mut body: Vec<Line> = Vec::new();
    let mut selected_line: Option<usize> = None;

    if items.is_empty() {
        body.push(placeholder(
            "No messages queued. Press Enter while the agent runs to stage one.",
            true,
            theme.muted(),
        ));
    } else {
        // Column geometry: gutter + time(HH:MM = 5) + 2-col gap + text.
        // Selection is shown via the existing background row highlight,
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
            let time = crate::time::sent_time_label(item.queued_at_ms);
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
            header: ModalHeader::title(title),
            lines: body,
            scroll,
            selected_line,
            follow_selection,
            has_items,
            item_footer_hints: &[
                FooterHint::navigation(keyvocab::ARROWS_UD, "select"),
                FooterHint::primary(keyvocab::ENTER, "edit"),
                FooterHint::always(keyvocab::ESC, "close"),
                FooterHint::secondary("J/K", "reorder"),
                FooterHint::secondary("F3", if blocked { "resume" } else { "block" }),
                FooterHint::secondary("F4", "insert"),
            ],
            empty_footer_hints: &[FooterHint::always(keyvocab::ESC, "close")],
            // Destructive delete sits at band 70 (outlives secondaries, never
            // the always-keep close) — the same convention as Connections /
            // Sessions.
            extra_footer_hints: &[FooterHint::with_band("D", "delete", 70)],
            keymap_open: false,
        },
        theme,
    )
}
