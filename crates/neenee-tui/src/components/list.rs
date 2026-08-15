//! Reusable list components and list-modal composition.

use neenee_tui_engine::{Color, Frame, Rect};

use super::super::Theme;
use super::super::primitives::{ContentModalSpec, SCROLL_EDGE_MARGIN};
use super::footer::{FooterHint, FooterHintWithBand};
use super::modal::{ModalHeader, ModalPage, ModalPageSize, draw_modal_page};
use super::options::{ChoiceTone, choice_style};
use super::scroll::ScrollBody;

/// Row palette for a Filled-tone selectable row. A thin alias over the
/// canonical [`crate::components::options::ChoiceStyle`] so the legacy
/// `row_style()` call sites keep working while routing through one color rule.
pub(crate) struct RowStyle {
    pub bg: Color,
    pub fg: Color,
    pub dim: Color,
}

impl From<super::options::ChoiceStyle> for RowStyle {
    fn from(s: super::options::ChoiceStyle) -> Self {
        Self {
            bg: s.bg,
            fg: s.fg,
            dim: s.dim,
        }
    }
}

/// Resolve the palette for a centered modal list row (the Filled tone). Every
/// columnar selectable surface — config, tools, mcp, sessions — goes through
/// here so there is exactly one "what does selected look like" rule.
pub(crate) fn row_style(selected: bool, theme: &Theme) -> RowStyle {
    choice_style(ChoiceTone::Filled, selected, theme).into()
}

pub(crate) struct SelectableListPage<'a> {
    pub geometry: ContentModalSpec,
    pub header: ModalHeader<'a>,
    pub lines: Vec<neenee_tui_engine::Line<'static>>,
    pub scroll: &'a mut usize,
    pub selected_line: Option<usize>,
    pub follow_selection: bool,
    pub has_items: bool,
    pub item_footer_hints: &'a [FooterHint],
    pub empty_footer_hints: &'a [FooterHint],
    /// Custom-band extra hints (e.g. a destructive `d` at band 70), shown after
    /// `item_footer_hints` / `empty_footer_hints`. Empty for most list modals.
    pub extra_footer_hints: &'a [FooterHintWithBand],
    /// When true, the body is replaced by the full keymap page (in-modal `?`).
    pub keymap_open: bool,
}

pub(crate) fn draw_selectable_list_page(
    frame: &mut Frame,
    page: SelectableListPage<'_>,
    theme: &Theme,
) -> Rect {
    let follow = if page.has_items && page.follow_selection && !page.keymap_open {
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
            size: ModalPageSize::Content(page.geometry),
            header: page.header,
            body: ScrollBody {
                lines: page.lines,
                scroll: page.scroll,
                follow,
                edge_margin: SCROLL_EDGE_MARGIN,
                wrap: false,
            },
            footer_hints,
            extra_footer_hints: page.extra_footer_hints,
            keymap_open: page.keymap_open,
            // List modals support in-modal `?` expand, so surface `? help`
            // when the footer has collapsed. Empty-list pages have no keymap
            // wiring, so suppress it there.
            show_more: page.has_items,
        },
        theme,
    )
}
