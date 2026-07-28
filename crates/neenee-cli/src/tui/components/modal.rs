//! Reusable modal components built on top of low-level render primitives.

use neenee_tui_engine::{Frame, Rect};

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
    Fixed(FixedModalSpec),
    Content(ContentModalSpec),
}

pub(crate) enum ModalHeader<'a> {
    Title(&'a str),
    Parts(&'a [HeaderPart<'a>]),
}

impl<'a> ModalHeader<'a> {
    pub(crate) const fn title(title: &'a str) -> Self {
        Self::Title(title)
    }

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
        // Title gets a " · keybindings" suffix when the page is open.
        match page.header {
            ModalHeader::Title(title) => {
                modal_header(frame, f.header, &format!("{title} · keybindings"), theme);
            }
            ModalHeader::Parts(parts) => modal_header_parts(frame, f.header, parts, theme),
        }
        let lines = keymap_body_lines(page.footer_hints, page.extra_footer_hints, theme);
        ScrollBody {
            lines,
            scroll: page.body.scroll,
            follow: None,
            edge_margin: page.body.edge_margin,
            wrap: false,
        }
        .render(frame, f.body, theme);
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
