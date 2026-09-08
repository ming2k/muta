use std::any::Any;
use std::fmt::{self, Debug};
use std::sync::Arc;
use super::super::constraints::{BoxConstraints, Offset, Size};
use super::super::context::{HitTestEntry, HitTestResult, PaintContext};
use super::super::render_box::RenderBox;

pub type LayoutFn = Arc<dyn Fn(BoxConstraints) -> Size + Send + Sync>;
pub type PaintFn = Arc<dyn Fn(&mut PaintContext<'_, '_>, Size) + Send + Sync>;
pub type HitTestFn = Arc<dyn Fn((u16, u16), Size) -> bool + Send + Sync>;

/// A customizable leaf RenderBox with functional measurement, painting, and hit-testing.
pub struct RenderCustom {
    id: Option<u64>,
    tag: &'static str,
    layout_fn: Option<LayoutFn>,
    paint_fn: Option<PaintFn>,
    hit_test_fn: Option<HitTestFn>,
    size: Size,
    offset: Offset,
    dirty_layout: bool,
    dirty_paint: bool,
}

impl Debug for RenderCustom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RenderCustom")
            .field("id", &self.id)
            .field("tag", &self.tag)
            .field("size", &self.size)
            .field("offset", &self.offset)
            .finish()
    }
}

impl RenderCustom {
    pub fn new(tag: &'static str) -> Self {
        Self {
            id: None,
            tag,
            layout_fn: None,
            paint_fn: None,
            hit_test_fn: None,
            size: Size::ZERO,
            offset: Offset::ZERO,
            dirty_layout: true,
            dirty_paint: true,
        }
    }

    pub fn with_id(mut self, id: u64) -> Self {
        self.id = Some(id);
        self
    }

    pub fn on_layout(mut self, f: impl Fn(BoxConstraints) -> Size + Send + Sync + 'static) -> Self {
        self.layout_fn = Some(Arc::new(f));
        self
    }

    pub fn on_paint(mut self, f: impl Fn(&mut PaintContext<'_, '_>, Size) + Send + Sync + 'static) -> Self {
        self.paint_fn = Some(Arc::new(f));
        self
    }

    pub fn on_hit_test(mut self, f: impl Fn((u16, u16), Size) -> bool + Send + Sync + 'static) -> Self {
        self.hit_test_fn = Some(Arc::new(f));
        self
    }
}

impl RenderBox for RenderCustom {
    fn id(&self) -> Option<u64> {
        self.id
    }

    fn tag(&self) -> &'static str {
        self.tag
    }

    fn layout(&mut self, constraints: BoxConstraints) -> Size {
        self.size = if let Some(ref f) = self.layout_fn {
            let s = f(constraints);
            constraints.constrain(s)
        } else {
            constraints.constrain(Size::ZERO)
        };
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
        if let Some(ref f) = self.paint_fn {
            f(ctx, self.size);
        }
    }

    fn hit_test(&self, local_pos: (u16, u16), result: &mut HitTestResult) -> bool {
        let (x, y) = local_pos;
        if x >= self.size.width || y >= self.size.height {
            return false;
        }

        let hit = if let Some(ref f) = self.hit_test_fn {
            f(local_pos, self.size)
        } else {
            true
        };

        if hit {
            let global_pos = ctx_global_pos(self.offset, local_pos);
            result.add(HitTestEntry {
                id: self.id,
                tag: self.tag,
                local_pos,
                global_pos,
            });
            true
        } else {
            false
        }
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

/// A fixed-dimension leaf RenderBox.
#[derive(Debug)]
pub struct RenderLeaf {
    id: Option<u64>,
    tag: &'static str,
    desired_size: Size,
    size: Size,
    offset: Offset,
    interactive: bool,
    dirty_layout: bool,
    dirty_paint: bool,
}

impl RenderLeaf {
    pub fn new(tag: &'static str, desired_size: Size) -> Self {
        Self {
            id: None,
            tag,
            desired_size,
            size: Size::ZERO,
            offset: Offset::ZERO,
            interactive: false,
            dirty_layout: true,
            dirty_paint: true,
        }
    }

    pub fn with_id(mut self, id: u64) -> Self {
        self.id = Some(id);
        self
    }

    pub fn interactive(mut self, interactive: bool) -> Self {
        self.interactive = interactive;
        self
    }
}

impl RenderBox for RenderLeaf {
    fn id(&self) -> Option<u64> {
        self.id
    }

    fn tag(&self) -> &'static str {
        self.tag
    }

    fn layout(&mut self, constraints: BoxConstraints) -> Size {
        self.size = constraints.constrain(self.desired_size);
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

    fn paint(&self, _ctx: &mut PaintContext<'_, '_>) {}

    fn hit_test(&self, local_pos: (u16, u16), result: &mut HitTestResult) -> bool {
        if !self.interactive {
            return false;
        }
        let (x, y) = local_pos;
        if x < self.size.width && y < self.size.height {
            let global_pos = ctx_global_pos(self.offset, local_pos);
            result.add(HitTestEntry {
                id: self.id,
                tag: self.tag,
                local_pos,
                global_pos,
            });
            true
        } else {
            false
        }
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

fn ctx_global_pos(offset: Offset, local_pos: (u16, u16)) -> (u16, u16) {
    let gx = (offset.x + local_pos.0 as i32).max(0) as u16;
    let gy = (offset.y + local_pos.1 as i32).max(0) as u16;
    (gx, gy)
}
