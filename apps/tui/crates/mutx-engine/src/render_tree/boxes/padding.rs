use std::any::Any;
use crate::layout::Margin;
use super::super::constraints::{BoxConstraints, Offset, Size};
use super::super::context::{HitTestResult, PaintContext};
use super::super::render_box::RenderBox;

/// Insets its child by given horizontal and vertical margins.
#[derive(Debug)]
pub struct RenderPadding {
    pub padding: Margin,
    pub child: Box<dyn RenderBox>,
    size: Size,
    offset: Offset,
    dirty_layout: bool,
    dirty_paint: bool,
}

impl RenderPadding {
    pub fn new(padding: Margin, child: Box<dyn RenderBox>) -> Self {
        Self {
            padding,
            child,
            size: Size::ZERO,
            offset: Offset::ZERO,
            dirty_layout: true,
            dirty_paint: true,
        }
    }
}

impl RenderBox for RenderPadding {
    fn tag(&self) -> &'static str {
        "padding"
    }

    fn layout(&mut self, constraints: BoxConstraints) -> Size {
        let deflated = constraints.deflate(self.padding);
        let child_size = self.child.layout(deflated);
        self.child.set_offset(Offset::new(self.padding.horizontal as i32, self.padding.vertical as i32));

        let width = constraints.constrain_width(child_size.width + self.padding.horizontal * 2);
        let height = constraints.constrain_height(child_size.height + self.padding.vertical * 2);

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
        ctx.with_offset(self.child.offset(), |ctx| {
            self.child.paint(ctx);
        });
    }

    fn hit_test(&self, local_pos: (u16, u16), result: &mut HitTestResult) -> bool {
        let (x, y) = local_pos;
        if x < self.padding.horizontal || y < self.padding.vertical {
            return false;
        }
        let child_x = x - self.padding.horizontal;
        let child_y = y - self.padding.vertical;
        let child_size = self.child.size();
        if child_x < child_size.width && child_y < child_size.height {
            self.child.hit_test((child_x, child_y), result)
        } else {
            false
        }
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
