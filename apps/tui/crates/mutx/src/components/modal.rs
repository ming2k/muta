//! Reusable modal components built on top of low-level render primitives.

use mutx_engine::{Frame, Rect};

use super::super::Theme;
use super::super::design::MODAL_INNER_H_PADDING;
use super::super::primitives::{
    ContentModalSpec, FixedModalSpec, HeaderPart, content_modal_area, content_modal_probe,
    modal_area, modal_chrome_rows, modal_frame, modal_header, modal_header_parts,
};
use super::footer::{
    FooterHint, FooterHintWithBand, keymap_body_lines, keymap_page_footer_hints,
    render_modal_footer, render_modal_footer_with_more,
};
use super::scroll::ScrollBody;

#[derive(Clone, Copy)]
pub(crate) enum ModalPageSize {
    /// Fixed-geometry page shell. No current caller (Help, the last fixed-
    /// geometry `ModalPage` user, renders its selectable body through the
    /// `modal_frame` ceremony directly); kept as the shell's complete
    /// geometry vocabulary.
    #[allow(dead_code)]
    Fixed(FixedModalSpec),
    Content(ContentModalSpec),
}

impl ModalPageSize {
    #[allow(dead_code)]
    pub fn exact_dimensions(&self, frame: &Frame) -> (u16, u16) {
        match self {
            ModalPageSize::Fixed(geometry) => geometry.exact_dimensions(frame),
            ModalPageSize::Content(geometry) => {
                let probe = content_modal_probe(frame, *geometry);
                (probe.width, probe.height)
            }
        }
    }

    #[allow(dead_code)]
    pub fn max_dimensions(&self, frame: &Frame) -> (u16, u16) {
        match self {
            ModalPageSize::Fixed(geometry) => geometry.exact_dimensions(frame),
            ModalPageSize::Content(geometry) => geometry.max_dimensions(frame),
        }
    }
}

pub(crate) enum ModalHeader<'a> {
    Title(&'a str),
    #[allow(dead_code)]
    Parts(&'a [HeaderPart<'a>]),
}

impl<'a> ModalHeader<'a> {
    pub(crate) const fn title(title: &'a str) -> Self {
        Self::Title(title)
    }

    #[allow(dead_code)]
    pub(crate) const fn parts(parts: &'a [HeaderPart<'a>]) -> Self {
        Self::Parts(parts)
    }
}

pub(crate) struct ModalPage<'a> {
    pub size: ModalPageSize,
    pub header: ModalHeader<'a>,
    pub body: ScrollBody<'a>,
    pub footer_hints: &'a [FooterHint],
    /// Custom-band extra hints (e.g. `D delete` at band 70), shown after
    /// `footer_hints` and dropped/collapsed with them. Empty for most modals.
    pub extra_footer_hints: &'a [FooterHintWithBand],
    /// When true, the body is replaced by the full keymap page for
    /// `footer_hints` + `extra_footer_hints`, and the footer becomes
    /// `↑↓ scroll · Esc back` (in-modal `?` expand — not a nested modal).
    pub keymap_open: bool,
    /// When true, a collapsed footer appends `? help` (list modals that wire
    /// in-modal keymap expand). Help / pure info pages leave this false.
    pub show_more: bool,
    /// Selection context for the body / keymap page as a selectable
    /// document. `Some((&selection, &mut layout_map))` routes the keymap
    /// page's rows through `render_selectable_body` (drag-select + copy);
    /// `None` keeps the plain `ScrollBody` (control surfaces — pickers,
    /// editors). The main body always uses `ScrollBody`; documents migrate
    /// by calling `render_selectable_body` directly instead of through the
    /// page shell.
    pub select_doc: Option<(
        &'a crate::model::selection::SelectionState,
        &'a mut crate::model::layout::LayoutMap,
    )>,
}

pub(crate) fn modal_body_width(frame: &Frame, geometry: ContentModalSpec) -> usize {
    let probe = content_modal_probe(frame, geometry);
    (probe.width as usize)
        .saturating_sub(2 * MODAL_INNER_H_PADDING as usize)
        .max(1)
}

/// Draw a complete centered modal page from declarative parts.
///
/// This is the common modal shell: geometry, panel chrome, header, scrollable
/// body, and footer hints. Callers still own business-specific body construction
/// and input state, but they no longer need to repeat the frame/header/body/
/// footer ceremony for each simple overlay.
///
/// When [`ModalPage::keymap_open`] is true the body is replaced by a full
/// keybindings list derived from the footer hints (in-modal `?` expand — not a
/// nested modal).
pub(crate) fn draw_modal_page(frame: &mut Frame, page: ModalPage<'_>, theme: &Theme) -> Rect {
    let area = match page.size {
        ModalPageSize::Fixed(geometry) => modal_area(frame, geometry),
        ModalPageSize::Content(geometry) => {
            let spec = geometry.modal_spec();
            let desired = if page.keymap_open {
                // Keymap page: one title + blank + one row per hint.
                (page.footer_hints.len() + page.extra_footer_hints.len()) as u16
                    + 2
                    + modal_chrome_rows(spec)
            } else {
                page.body.lines.len() as u16 + modal_chrome_rows(spec)
            };
            content_modal_area(frame, geometry, desired)
        }
    };
    let f = modal_frame(frame, area, theme.panel(), true, true);

    if page.keymap_open {
        // Breadcrumb: `{title}` modal › its keybindings sub-page (hierarchy is
        // never joined with `·`, which is reserved for same-rank modifiers).
        match page.header {
            ModalHeader::Title(title) => {
                modal_header(
                    frame,
                    f.header,
                    &format!("{title}{}{}", crate::design::JOIN_BREADCRUMB, "keybindings"),
                    theme,
                );
            }
            ModalHeader::Parts(parts) => modal_header_parts(frame, f.header, parts, theme),
        }
        // The keymap sub-page is a selectable document: keycap labels and
        // descriptions register as MODAL_DOC rows so they can be copied like
        // any other help text.
        let lines = keymap_body_lines(page.footer_hints, page.extra_footer_hints, theme);
        if let Some((selection, layout_map)) = page.select_doc {
            let rows: Vec<crate::components::selectable_body::SelectableRow> = lines
                .into_iter()
                .map(crate::components::selectable_body::SelectableRow::from_line)
                .collect();
            crate::components::selectable_body::render_selectable_body(
                frame,
                f.body,
                &rows,
                page.body.scroll,
                None,
                theme,
                selection,
                layout_map,
            );
        } else {
            ScrollBody {
                lines,
                scroll: page.body.scroll,
                follow: None,
                edge_margin: page.body.edge_margin,
                wrap: false,
            }
            .render(frame, f.body, theme);
        }
        if let Some(footer) = f.footer {
            // No recursive `? help` while already on the keymap page.
            render_modal_footer(frame, footer, &keymap_page_footer_hints(), theme);
        }
    } else {
        match page.header {
            ModalHeader::Title(title) => modal_header(frame, f.header, title, theme),
            ModalHeader::Parts(parts) => modal_header_parts(frame, f.header, parts, theme),
        }
        page.body.render(frame, f.body, theme);
        if let Some(footer) = f.footer {
            if page.show_more || !page.extra_footer_hints.is_empty() {
                render_modal_footer_with_more(
                    frame,
                    footer,
                    page.footer_hints,
                    page.extra_footer_hints,
                    theme,
                );
            } else {
                render_modal_footer(frame, footer, page.footer_hints, theme);
            }
        }
    }

    area
}
