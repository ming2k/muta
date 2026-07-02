//! Scrollable content components.

use neenee_tui::{Frame, Line, Rect};

use super::super::Theme;
use super::super::primitives::render_body;

pub(in crate::render) struct ScrollBody<'a> {
    pub lines: Vec<Line<'static>>,
    pub scroll: &'a mut usize,
    pub follow: Option<usize>,
    pub edge_margin: usize,
    pub wrap: bool,
}

impl<'a> ScrollBody<'a> {
    pub(in crate::render) fn render(self, frame: &mut Frame, rect: Rect, theme: &Theme) {
        render_body(
            frame,
            rect,
            self.lines,
            self.scroll,
            self.follow,
            self.edge_margin,
            self.wrap,
            theme,
        );
    }
}
