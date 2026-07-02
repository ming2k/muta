//! Reusable list components and list-modal composition.

use neenee_tui::{Color, Frame, Rect};

use crate::modal::Modal;

use super::super::Theme;
use super::super::primitives::{SCROLL_EDGE_MARGIN, contrast_fg};
use super::footer::FooterHint;
use super::modal::{ModalHeader, ModalPage, ModalPageSize, draw_modal_page};
use super::scroll::ScrollBody;

pub(in crate::render) struct RowStyle {
    pub bg: Color,
    pub fg: Color,
    pub dim: Color,
}

pub(in crate::render) fn row_style(selected: bool, theme: &Theme) -> RowStyle {
    let bg = if selected {
        theme.brand()
    } else {
        theme.panel()
    };
    let fg = if selected {
        contrast_fg(theme.brand())
    } else {
        theme.fg()
    };
    let dim = if selected {
        contrast_fg(theme.brand())
    } else {
        theme.muted()
    };
    RowStyle { bg, fg, dim }
}

pub(in crate::render) struct SelectableListPage<'a> {
    pub modal: Modal,
    pub header: ModalHeader<'a>,
    pub lines: Vec<neenee_tui::Line<'static>>,
    pub scroll: &'a mut usize,
    pub selected_line: Option<usize>,
    pub follow_selection: bool,
    pub has_items: bool,
    pub item_footer_hints: &'a [FooterHint],
    pub empty_footer_hints: &'a [FooterHint],
}

pub(in crate::render) fn draw_selectable_list_page(
    frame: &mut Frame,
    page: SelectableListPage<'_>,
    theme: &Theme,
) -> Rect {
    let follow = if page.has_items && page.follow_selection {
        page.selected_line
    } else {
        None
    };
    let footer_hints = if page.has_items {
        page.item_footer_hints
    } else {
        page.empty_footer_hints
    };
    draw_modal_page(
        frame,
        ModalPage {
            modal: page.modal,
            size: ModalPageSize::Content,
            header: page.header,
            body: ScrollBody {
                lines: page.lines,
                scroll: page.scroll,
                follow,
                edge_margin: SCROLL_EDGE_MARGIN,
                wrap: false,
            },
            footer_hints,
        },
        theme,
    )
}
