use std::any::Any;
use std::fmt::Debug;
use super::constraints::{BoxConstraints, Offset, Size};
use super::context::{HitTestResult, PaintContext};

/// The fundamental interface for a node in the retained Render Tree.
///
/// In accord with classical UI architecture (ADR-0195):
/// 1. Constraints flow downward (`layout` receives `BoxConstraints`).
/// 2. Geometry flows upward (`layout` returns its determined `Size`).
/// 3. Parent establishes child spatial `Offset`.
/// 4. Painting flows downward through `paint` with clip/offset scoping.
/// 5. Hit testing flows downward and collects interactive targets.
pub trait RenderBox: Debug + Send + Sync {
    /// Optional stable identifier for the node.
    fn id(&self) -> Option<u64> {
        None
    }

    /// Descriptive tag or role of this box (e.g. "flex", "button", "composer").
    fn tag(&self) -> &'static str {
        "box"
    }

    /// Measure and layout this box according to constraints, returning the chosen size.
    fn layout(&mut self, constraints: BoxConstraints) -> Size;

    /// The size determined during the most recent layout pass.
    fn size(&self) -> Size;

    /// The spatial offset assigned to this box relative to its parent.
    fn offset(&self) -> Offset;

    /// Sets the spatial offset of this box relative to its parent.
    fn set_offset(&mut self, offset: Offset);

    /// Paint this box and its children into the paint context.
    fn paint(&self, ctx: &mut PaintContext<'_, '_>);

    /// Perform hit-testing at the given local position (0..size.width, 0..size.height).
    ///
    /// If hit, appends target(s) to `result` and returns true.
    fn hit_test(&self, local_pos: (u16, u16), result: &mut HitTestResult) -> bool;

    /// Mark that this box needs layout recalculation.
    fn mark_needs_layout(&mut self);

    /// Mark that this box needs repainting.
    fn mark_needs_paint(&mut self);

    /// Whether this box requires layout before painting.
    fn is_layout_dirty(&self) -> bool;

    /// Whether this box requires repainting.
    fn is_paint_dirty(&self) -> bool;

    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}
