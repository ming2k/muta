//! Reusable modal components built on top of low-level render primitives.

use neenee_tui::{Frame, Rect};

use crate::modal::Modal;

use super::super::Theme;
use super::super::design::MODAL_INNER_H_PADDING;
use super::super::primitives::{
    HeaderPart, content_modal_area, modal_area, modal_chrome_rows, modal_frame, modal_header,
    modal_header_parts, modal_spec,
};
use super::footer::{FooterHint, render_modal_footer};
use super::scroll::ScrollBody;

#[derive(Clone, Copy)]
pub(in crate::render) enum ModalPageSize {
    Fixed,
    Content,
}

pub(in crate::render) enum ModalHeader<'a> {
    Title(&'a str),
    Parts(&'a [HeaderPart<'a>]),
}

impl<'a> ModalHeader<'a> {
    pub(in crate::render) const fn title(title: &'a str) -> Self {
        Self::Title(title)
    }

    pub(in crate::render) const fn parts(parts: &'a [HeaderPart<'a>]) -> Self {
        Self::Parts(parts)
    }
}

pub(in crate::render) struct ModalPage<'a> {
    pub modal: Modal,
    pub size: ModalPageSize,
    pub header: ModalHeader<'a>,
    pub body: ScrollBody<'a>,
    pub footer_hints: &'a [FooterHint],
}

pub(in crate::render) fn modal_body_width(frame: &Frame, modal: Modal) -> usize {
    let probe = super::super::primitives::content_modal_probe(frame, modal)
        .or_else(|| super::super::primitives::modal_area(frame, modal))
        .expect("modal has geometry");
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
pub(in crate::render) fn draw_modal_page(
    frame: &mut Frame,
    page: ModalPage<'_>,
    theme: &Theme,
) -> Rect {
    let area = match page.size {
        ModalPageSize::Fixed => {
            modal_area(frame, page.modal).expect("fixed modal page has fixed geometry")
        }
        ModalPageSize::Content => {
            let spec = modal_spec(page.modal).expect("content modal page has geometry");
            let desired = page.body.lines.len() as u16 + modal_chrome_rows(spec);
            content_modal_area(frame, page.modal, desired)
                .expect("content modal page has content geometry")
        }
    };
    let f = modal_frame(frame, area, theme.panel(), true, true);

    match page.header {
        ModalHeader::Title(title) => modal_header(frame, f.header, title, theme),
        ModalHeader::Parts(parts) => modal_header_parts(frame, f.header, parts, theme),
    }

    page.body.render(frame, f.body, theme);

    if let Some(footer) = f.footer {
        render_modal_footer(frame, footer, page.footer_hints, theme);
    }

    area
}
