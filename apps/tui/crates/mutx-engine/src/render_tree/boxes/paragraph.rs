use std::any::Any;
use crate::layout::Rect;
use crate::widgets::{Alignment, Line, Paragraph, Wrap};
use super::super::constraints::{BoxConstraints, Offset, Size};
use super::super::context::{HitTestResult, PaintContext};
use super::super::render_box::RenderBox;

/// Formatted text paragraph box supporting line wrapping and alignment.
#[derive(Debug)]
pub struct RenderParagraph {
    pub lines: Vec<Line<'static>>,
    pub alignment: Alignment,
    pub wrap: Option<Wrap>,
    size: Size,
    offset: Offset,
    dirty_layout: bool,
    dirty_paint: bool,
}

impl RenderParagraph {
    pub fn new(lines: Vec<Line<'static>>) -> Self {
        Self {
            lines,
            alignment: Alignment::Left,
            wrap: Some(Wrap { trim: false }),
            size: Size::ZERO,
            offset: Offset::ZERO,
            dirty_layout: true,
            dirty_paint: true,
        }
    }

    pub fn text(text: impl Into<String>) -> Self {
        let s: String = text.into();
        let lines = s.lines().map(|l| Line::raw(l.to_string())).collect();
        Self::new(lines)
    }

    pub fn alignment(mut self, alignment: Alignment) -> Self {
        self.alignment = alignment;
        self
    }
}

impl RenderBox for RenderParagraph {
    fn tag(&self) -> &'static str {
        "paragraph"
    }

    fn layout(&mut self, constraints: BoxConstraints) -> Size {
        let max_w = constraints.max_width as usize;
        let mut max_line_w = 0usize;
        let mut total_lines = 0usize;

        for line in &self.lines {
            let line_w = line.width();
            max_line_w = max_line_w.max(line_w);

            if self.wrap.is_some() && max_w > 0 && line_w > max_w {
                let wrapped_count = (line_w + max_w - 1) / max_w;
                total_lines += wrapped_count.max(1);
            } else {
                total_lines += 1;
            }
        }

        let width = constraints.constrain_width(max_line_w.min(u16::MAX as usize) as u16);
        let height = constraints.constrain_height(total_lines.min(u16::MAX as usize) as u16);

        self.size = Size::new(width, height);
        self.dirty_layout = false;
        self.size
    }

    fn size(&self) -> Size {
        self.size
    }

    fn offset(&self) -> Offset {
        self.offset
    }

    fn set_offset(&mut self, offset: Offset) {
        self.offset = offset;
    }

    fn paint(&self, ctx: &mut PaintContext<'_, '_>) {
        let mut p = Paragraph::new(self.lines.clone()).alignment(self.alignment);
        if let Some(wrap) = self.wrap {
            p = p.wrap(wrap);
        }
        let area = Rect::new(0, 0, self.size.width, self.size.height);
        ctx.render_widget(p, area);
    }

    fn hit_test(&self, local_pos: (u16, u16), _result: &mut HitTestResult) -> bool {
        let (x, y) = local_pos;
        x < self.size.width && y < self.size.height
    }

    fn mark_needs_layout(&mut self) {
        self.dirty_layout = true;
    }

    fn mark_needs_paint(&mut self) {
        self.dirty_paint = true;
    }

    fn is_layout_dirty(&self) -> bool {
        self.dirty_layout
    }

    fn is_paint_dirty(&self) -> bool {
        self.dirty_paint
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
