use std::any::Any;
use crate::layout::Rect;
use crate::widgets::Block;
use super::super::constraints::{BoxConstraints, Offset, Size};
use super::super::context::{HitTestResult, PaintContext};
use super::super::render_box::RenderBox;

/// Draws a decorative block (background, borders, title) and layouts its optional child inside.
#[derive(Debug)]
pub struct RenderBlock {
    pub block: Block<'static>,
    pub child: Option<Box<dyn RenderBox>>,
    size: Size,
    offset: Offset,
    dirty_layout: bool,
    dirty_paint: bool,
}

impl RenderBlock {
    pub fn new(block: Block<'static>) -> Self {
        Self {
            block,
            child: None,
            size: Size::ZERO,
            offset: Offset::ZERO,
            dirty_layout: true,
            dirty_paint: true,
        }
    }

    pub fn with_child(mut self, child: Box<dyn RenderBox>) -> Self {
        self.child = Some(child);
        self
    }
}

impl RenderBox for RenderBlock {
    fn tag(&self) -> &'static str {
        "block"
    }

    fn layout(&mut self, constraints: BoxConstraints) -> Size {
        let (border_h, border_v) = (2u16, 2u16);
        if let Some(ref mut child) = self.child {
            let child_constraints = BoxConstraints::new(
                constraints.min_width.saturating_sub(border_h),
                constraints.max_width.saturating_sub(border_h),
                constraints.min_height.saturating_sub(border_v),
                constraints.max_height.saturating_sub(border_v),
            );
            let child_size = child.layout(child_constraints);
            child.set_offset(Offset::new(1, 1));

            let width = constraints.constrain_width(child_size.width + border_h);
            let height = constraints.constrain_height(child_size.height + border_v);
            self.size = Size::new(width, height);
        } else {
            self.size = constraints.constrain(Size::new(border_h, border_v));
        }

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
        let area = Rect::new(0, 0, self.size.width, self.size.height);
        ctx.render_widget(self.block.clone(), area);

        if let Some(ref child) = self.child {
            ctx.with_offset(child.offset(), |ctx| {
                child.paint(ctx);
            });
        }
    }

    fn hit_test(&self, local_pos: (u16, u16), result: &mut HitTestResult) -> bool {
        let (x, y) = local_pos;
        if x >= self.size.width || y >= self.size.height {
            return false;
        }

        if let Some(ref child) = self.child {
            let (cx, cy) = (child.offset().x as i32, child.offset().y as i32);
            let child_size = child.size();
            if (x as i32) >= cx
                && (x as i32) < cx + child_size.width as i32
                && (y as i32) >= cy
                && (y as i32) < cy + child_size.height as i32
            {
                let local_child_x = (x as i32 - cx) as u16;
                let local_child_y = (y as i32 - cy) as u16;
                return child.hit_test((local_child_x, local_child_y), result);
            }
        }
        false
    }

    fn mark_needs_layout(&mut self) {
        self.dirty_layout = true;
        if let Some(ref mut child) = self.child {
            child.mark_needs_layout();
        }
    }

    fn mark_needs_paint(&mut self) {
        self.dirty_paint = true;
        if let Some(ref mut child) = self.child {
            child.mark_needs_paint();
        }
    }

    fn is_layout_dirty(&self) -> bool {
        self.dirty_layout || self.child.as_ref().is_some_and(|c| c.is_layout_dirty())
    }

    fn is_paint_dirty(&self) -> bool {
        self.dirty_paint || self.child.as_ref().is_some_and(|c| c.is_paint_dirty())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
