use std::any::Any;
use crate::layout::Rect;
use super::super::constraints::{BoxConstraints, Offset, Size};
use super::super::context::{HitTestResult, PaintContext};
use super::super::render_box::RenderBox;

/// Clips its child to its own layout boundaries.
#[derive(Debug)]
pub struct RenderClip {
    pub child: Box<dyn RenderBox>,
    size: Size,
    offset: Offset,
    dirty_layout: bool,
    dirty_paint: bool,
}

impl RenderClip {
    pub fn new(child: Box<dyn RenderBox>) -> Self {
        Self {
            child,
            size: Size::ZERO,
            offset: Offset::ZERO,
            dirty_layout: true,
            dirty_paint: true,
        }
    }
}

impl RenderBox for RenderClip {
    fn tag(&self) -> &'static str {
        "clip"
    }

    fn layout(&mut self, constraints: BoxConstraints) -> Size {
        self.size = self.child.layout(constraints);
        self.child.set_offset(Offset::ZERO);
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
        let clip_rect = Rect::new(0, 0, self.size.width, self.size.height);
        ctx.with_clip(clip_rect, |ctx| {
            self.child.paint(ctx);
        });
    }

    fn hit_test(&self, local_pos: (u16, u16), result: &mut HitTestResult) -> bool {
        let (x, y) = local_pos;
        if x >= self.size.width || y >= self.size.height {
            return false;
        }
        self.child.hit_test(local_pos, result)
    }

    fn mark_needs_layout(&mut self) {
        self.dirty_layout = true;
        self.child.mark_needs_layout();
    }

    fn mark_needs_paint(&mut self) {
        self.dirty_paint = true;
        self.child.mark_needs_paint();
    }

    fn is_layout_dirty(&self) -> bool {
        self.dirty_layout || self.child.is_layout_dirty()
    }

    fn is_paint_dirty(&self) -> bool {
        self.dirty_paint || self.child.is_paint_dirty()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
