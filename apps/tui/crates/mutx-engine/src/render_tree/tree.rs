use super::constraints::{BoxConstraints, Offset, Size};
use super::context::{HitTestResult, PaintContext};
use super::render_box::RenderBox;

/// Coordinates the layout, paint, and hit-testing passes for a retained UI tree.
#[derive(Debug)]
pub struct RenderTree {
    pub root: Box<dyn RenderBox>,
    viewport: Size,
}

impl RenderTree {
    pub fn new(root: Box<dyn RenderBox>) -> Self {
        Self {
            root,
            viewport: Size::ZERO,
        }
    }

    pub fn set_root(&mut self, root: Box<dyn RenderBox>) {
        self.root = root;
        self.mark_needs_layout();
    }

    pub fn viewport(&self) -> Size {
        self.viewport
    }

    /// Perform a full downward layout pass from root satisfying viewport constraints.
    pub fn layout(&mut self, viewport: Size) -> Size {
        self.viewport = viewport;
        let constraints = BoxConstraints::tight(viewport);
        let actual_size = self.root.layout(constraints);
        self.root.set_offset(Offset::ZERO);
        actual_size
    }

    /// Perform a full downward paint pass into the provided context.
    pub fn paint(&self, ctx: &mut PaintContext<'_, '_>) {
        self.root.paint(ctx);
    }

    /// Perform hit-testing against the rendered tree.
    pub fn hit_test(&self, screen_pos: (u16, u16)) -> HitTestResult {
        let mut result = HitTestResult::new();
        let (x, y) = screen_pos;
        if x < self.viewport.width && y < self.viewport.height {
            self.root.hit_test(screen_pos, &mut result);
        }
        result
    }

    pub fn mark_needs_layout(&mut self) {
        self.root.mark_needs_layout();
    }

    pub fn mark_needs_paint(&mut self) {
        self.root.mark_needs_paint();
    }

    pub fn is_dirty(&self) -> bool {
        self.root.is_layout_dirty() || self.root.is_paint_dirty()
    }
}
