use std::any::Any;
use super::super::constraints::{BoxConstraints, Offset, Size};
use super::super::context::{HitTestResult, PaintContext};
use super::super::render_box::RenderBox;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Alignment {
    #[default]
    TopLeft,
    TopCenter,
    TopRight,
    CenterLeft,
    Center,
    CenterRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

impl Alignment {
    pub fn offset_for(self, container: Size, child: Size) -> Offset {
        let x = match self {
            Self::TopLeft | Self::CenterLeft | Self::BottomLeft => 0,
            Self::TopCenter | Self::Center | Self::BottomCenter => {
                container.width.saturating_sub(child.width) / 2
            }
            Self::TopRight | Self::CenterRight | Self::BottomRight => {
                container.width.saturating_sub(child.width)
            }
        };

        let y = match self {
            Self::TopLeft | Self::TopCenter | Self::TopRight => 0,
            Self::CenterLeft | Self::Center | Self::CenterRight => {
                container.height.saturating_sub(child.height) / 2
            }
            Self::BottomLeft | Self::BottomCenter | Self::BottomRight => {
                container.height.saturating_sub(child.height)
            }
        };

        Offset::new(x as i32, y as i32)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StackFit {
    #[default]
    Loose,
    Expand,
    Passthrough,
}

#[derive(Debug)]
pub struct StackChild {
    pub render_box: Box<dyn RenderBox>,
    pub alignment: Option<Alignment>,
    pub custom_offset: Option<Offset>,
    pub fill: bool,
}

impl StackChild {
    pub fn aligned(render_box: Box<dyn RenderBox>, alignment: Alignment) -> Self {
        Self {
            render_box,
            alignment: Some(alignment),
            custom_offset: None,
            fill: false,
        }
    }

    pub fn positioned(render_box: Box<dyn RenderBox>, offset: Offset) -> Self {
        Self {
            render_box,
            alignment: None,
            custom_offset: Some(offset),
            fill: false,
        }
    }

    pub fn fill(render_box: Box<dyn RenderBox>) -> Self {
        Self {
            render_box,
            alignment: None,
            custom_offset: None,
            fill: true,
        }
    }
}

/// A container that stacks children on top of each other.
#[derive(Debug)]
pub struct RenderStack {
    pub default_alignment: Alignment,
    pub fit: StackFit,
    pub children: Vec<StackChild>,
    size: Size,
    offset: Offset,
    dirty_layout: bool,
    dirty_paint: bool,
}

impl RenderStack {
    pub fn new() -> Self {
        Self {
            default_alignment: Alignment::TopLeft,
            fit: StackFit::Loose,
            children: Vec::new(),
            size: Size::ZERO,
            offset: Offset::ZERO,
            dirty_layout: true,
            dirty_paint: true,
        }
    }

    pub fn expand() -> Self {
        Self {
            fit: StackFit::Expand,
            ..Self::new()
        }
    }

    pub fn with_child(mut self, child: StackChild) -> Self {
        self.children.push(child);
        self
    }

    pub fn add_child(&mut self, child: StackChild) {
        self.children.push(child);
        self.mark_needs_layout();
    }
}

impl RenderBox for RenderStack {
    fn tag(&self) -> &'static str {
        "stack"
    }

    fn layout(&mut self, constraints: BoxConstraints) -> Size {
        let mut max_w = 0u16;
        let mut max_h = 0u16;

        // First pass: measure non-fill children with loose constraints
        for child in &mut self.children {
            if !child.fill {
                let child_constraints = constraints.loosen();
                let child_size = child.render_box.layout(child_constraints);
                max_w = max_w.max(child_size.width);
                max_h = max_h.max(child_size.height);
            }
        }

        let container_size = match self.fit {
            StackFit::Expand => Size::new(
                if constraints.has_bounded_width() { constraints.max_width } else { max_w },
                if constraints.has_bounded_height() { constraints.max_height } else { max_h },
            ),
            _ => constraints.constrain(Size::new(max_w, max_h)),
        };

        // Second pass: layout fill children with tight container constraints, and position all
        for child in &mut self.children {
            if child.fill {
                child.render_box.layout(BoxConstraints::tight(container_size));
                child.render_box.set_offset(Offset::ZERO);
            } else {
                let child_offset = if let Some(offset) = child.custom_offset {
                    offset
                } else {
                    let align = child.alignment.unwrap_or(self.default_alignment);
                    align.offset_for(container_size, child.render_box.size())
                };
                child.render_box.set_offset(child_offset);
            }
        }

        self.size = container_size;
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
        for child in &self.children {
            ctx.with_offset(child.render_box.offset(), |ctx| {
                child.render_box.paint(ctx);
            });
        }
    }

    fn hit_test(&self, local_pos: (u16, u16), result: &mut HitTestResult) -> bool {
        let (x, y) = local_pos;
        if x >= self.size.width || y >= self.size.height {
            return false;
        }

        // Hit-test in reverse paint order: topmost layer receives events first
        for child in self.children.iter().rev() {
            let child_offset = child.render_box.offset();
            let child_size = child.render_box.size();
            let (cx, cy) = (child_offset.x as i32, child_offset.y as i32);

            if (x as i32) >= cx
                && (x as i32) < cx + child_size.width as i32
                && (y as i32) >= cy
                && (y as i32) < cy + child_size.height as i32
            {
                let local_child_x = (x as i32 - cx) as u16;
                let local_child_y = (y as i32 - cy) as u16;
                if child.render_box.hit_test((local_child_x, local_child_y), result) {
                    return true;
                }
            }
        }
        false
    }

    fn mark_needs_layout(&mut self) {
        self.dirty_layout = true;
        for child in &mut self.children {
            child.render_box.mark_needs_layout();
        }
    }

    fn mark_needs_paint(&mut self) {
        self.dirty_paint = true;
        for child in &mut self.children {
            child.render_box.mark_needs_paint();
        }
    }

    fn is_layout_dirty(&self) -> bool {
        self.dirty_layout || self.children.iter().any(|c| c.render_box.is_layout_dirty())
    }

    fn is_paint_dirty(&self) -> bool {
        self.dirty_paint || self.children.iter().any(|c| c.render_box.is_paint_dirty())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
