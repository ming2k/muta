//! Global view quick switcher (ADR-0139, `Ctrl+L`).
//!
//! A centered picker over every browse surface — open views first in MRU
//! order, then the not-yet-opened ones so the list doubles as discovery.
//! `Enter` switches: the current view is hidden (state retained in the
//! [`ViewRegistry`](crate::views::ViewRegistry)) and the target's retained
//! scroll/index is restored — the same "leave and come back, nothing lost"
//! contract sessions have. `Esc` closes with nothing changed; `Del` closes
//! the selected view's TUI state without deleting backend data.
//!
//! The switcher itself is not a retained view: it is a transient chooser
//! *over* views, so it never enters its own registry.

use mutx_engine::{
    Frame, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::common::{placeholder, truncate_ellipsis};
use crate::components::list::{SelectableListPage, draw_selectable_list_page, row_style};
use crate::components::modal::ModalHeader;
use crate::primitives::{FooterHint, keyvocab};
use crate::view::Theme;
use crate::views::ViewId;

use crate::primitives::ContentModalSpec;

/// Quick-switcher panel geometry: a compact centered list (one row per view
/// plus a hint column), sized like the queue overview it sits beside.
pub(crate) const VIEW_SWITCHER: ContentModalSpec = ContentModalSpec::BTW;

/// Draw the quick switcher. `rows` is the registry's switcher row set (MRU
/// first), `active` the modal the switcher was opened over (marked with a
/// `●` marker so "where am I" is readable in the list), `open` the set of
/// views currently in the MRU order (their rows show the `open` badge).
#[allow(clippy::too_many_arguments)] // showcase parity with other modal renderers
pub(crate) fn draw_view_switcher(
    frame: &mut Frame,
    rows: &[ViewId],
    query: &str,
    modal_index: usize,
    open_ids: &[ViewId],
    active: Option<ViewId>,
    scroll: &mut usize,
    follow_selection: bool,
    theme: &Theme,
    selection: &crate::model::selection::SelectionState,
    layout_map: &mut crate::model::layout::LayoutMap,
) -> mutx_engine::Rect {
    let body_width = crate::components::modal::modal_body_width(frame, VIEW_SWITCHER);

    let title = if rows.is_empty() {
        "Switch view".to_string()
    } else {
        format!("Switch view ({})", rows.len())
    };
    // The live filter (phase 5): shown in the header as `filter: <query>` —
    // the switcher does not borrow the composer, so the header is its only
    // visible query surface.
    let filter_title = format!("{} · filter: {query}", title);
    let header = if query.is_empty() {
        ModalHeader::title(&title)
    } else {
        ModalHeader::title(&filter_title)
    };

    let mut body: Vec<Line> = Vec::new();
    let mut selected_line: Option<usize> = None;

    if rows.is_empty() {
        // Unreachable while `ViewId::ALL` is non-empty, but the empty-list
        // path keeps the shared page component's contract total.
        body.push(placeholder("No views.", true, theme.muted()));
    } else {
        // Column geometry: gutter + badge (5 = "open " / "here " / "     ")
        // + hint (right-aligned, dim) + 2-col gap + label.
        const GUTTER_W: usize = 2;
        const BADGE_W: usize = 5;
        let text_budget = body_width.saturating_sub(GUTTER_W + BADGE_W + 2).max(1);

        for (i, id) in rows.iter().enumerate() {
            let is_sel = i == modal_index;
            let style = row_style(is_sel, theme);
            if is_sel {
                selected_line = Some(body.len());
            }
            let is_open = open_ids.contains(id);
            let is_here = Some(*id) == active;
            // Badge: `here` (the surface the switcher was opened over) is
            // the state that matters most, then `open` (in the MRU order,
            // i.e. conceptually mounted), else quiet blank space.
            let (badge, badge_color) = if is_here {
                ("here ", theme.brand())
            } else if is_open {
                ("open", theme.primary)
            } else {
                ("     ", style.dim)
            };
            let hint = id.hint();
            let label = id.label();
            let one_line = format!("{label}  ·  {hint}");
            let text = if one_line.width() > text_budget {
                truncate_ellipsis(&one_line, text_budget)
            } else {
                one_line
            };
            let left_w = GUTTER_W + BADGE_W + 2 + text.width();
            let pad = body_width.saturating_sub(left_w);
            body.push(Line::from(vec![
                Span::styled(" ".repeat(GUTTER_W), Style::default().bg(style.bg)),
                Span::styled(
                    format!("{badge:<BADGE_W$}"),
                    Style::default().bg(style.bg).fg(badge_color),
                ),
                Span::styled("  ", Style::default().bg(style.bg)),
                Span::styled(text, Style::default().bg(style.bg).fg(style.fg)),
                Span::styled(" ".repeat(pad), Style::default().bg(style.bg)),
            ]));
        }
    }

    draw_selectable_list_page(
        frame,
        SelectableListPage {
            geometry: VIEW_SWITCHER,
            header,
            lines: body,
            scroll,
            selected_line,
            follow_selection,
            has_items: !rows.is_empty(),
            item_footer_hints: &[
                FooterHint::navigation(keyvocab::ARROWS_UD, "select"),
                FooterHint::primary(keyvocab::ENTER, "switch"),
                FooterHint::always("Del", "close view"),
                FooterHint::always(keyvocab::ESC, "close"),
            ],
            empty_footer_hints: &[FooterHint::always(keyvocab::ESC, "close")],
            extra_footer_hints: &[],
            keymap_open: false,
            select_doc: Some((selection, layout_map)),
        },
        theme,
    )
}
