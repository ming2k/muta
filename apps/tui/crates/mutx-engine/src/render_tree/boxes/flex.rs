use std::any::Any;
use super::super::constraints::{BoxConstraints, Offset, Size};
use super::super::context::{HitTestResult, PaintContext};
use super::super::render_box::RenderBox;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FlexDirection {
    #[default]
    Vertical,
    Horizontal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MainAxisAlignment {
    #[default]
    Start,
    End,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CrossAxisAlignment {
    #[default]
    Stretch,
    Start,
    End,
    Center,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FlexFit {
    #[default]
    Loose,
    Tight,
}

#[derive(Debug)]
pub struct FlexChild {
    pub render_box: Box<dyn RenderBox>,
    pub flex: u16,
    pub fit: FlexFit,
}

impl FlexChild {
    pub fn fixed(render_box: Box<dyn RenderBox>) -> Self {
        Self {
            render_box,
            flex: 0,
            fit: FlexFit::Loose,
        }
    }

    pub fn flex(render_box: Box<dyn RenderBox>, flex: u16) -> Self {
        Self {
            render_box,
            flex,
            fit: FlexFit::Tight,
        }
    }

    pub fn flexible(render_box: Box<dyn RenderBox>, flex: u16) -> Self {
        Self {
            render_box,
            flex,
            fit: FlexFit::Loose,
        }
    }
}

/// Linear layout container supporting main-axis distribution and cross-axis alignment.
#[derive(Debug)]
pub struct RenderFlex {
    pub direction: FlexDirection,
    pub main_axis_alignment: MainAxisAlignment,
    pub cross_axis_alignment: CrossAxisAlignment,
    pub gap: u16,
    pub children: Vec<FlexChild>,
    size: Size,
    offset: Offset,
    dirty_layout: bool,
    dirty_paint: bool,
}

impl RenderFlex {
    pub fn new(direction: FlexDirection) -> Self {
        Self {
            direction,
            main_axis_alignment: MainAxisAlignment::Start,
            cross_axis_alignment: CrossAxisAlignment::Stretch,
            gap: 0,
            children: Vec::new(),
            size: Size::ZERO,
            offset: Offset::ZERO,
            dirty_layout: true,
            dirty_paint: true,
        }
    }

    pub fn row() -> Self {
        Self::new(FlexDirection::Horizontal)
    }

    pub fn column() -> Self {
        Self::new(FlexDirection::Vertical)
    }

    pub fn gap(mut self, gap: u16) -> Self {
        self.gap = gap;
        self
    }

    pub fn main_align(mut self, align: MainAxisAlignment) -> Self {
        self.main_axis_alignment = align;
        self
    }

    pub fn cross_align(mut self, align: CrossAxisAlignment) -> Self {
        self.cross_axis_alignment = align;
        self
    }

    pub fn with_child(mut self, child: FlexChild) -> Self {
        self.children.push(child);
        self
    }

    pub fn add_child(&mut self, child: FlexChild) {
        self.children.push(child);
        self.mark_needs_layout();
    }
}

impl RenderBox for RenderFlex {
    fn tag(&self) -> &'static str {
        match self.direction {
            FlexDirection::Horizontal => "row",
            FlexDirection::Vertical => "column",
        }
    }

    fn layout(&mut self, constraints: BoxConstraints) -> Size {
        let is_vert = self.direction == FlexDirection::Vertical;
        let max_main = if is_vert { constraints.max_height } else { constraints.max_width };
        let max_cross = if is_vert { constraints.max_width } else { constraints.max_height };

        let child_count = self.children.len();
        if child_count == 0 {
            self.size = constraints.constrain(Size::ZERO);
            self.dirty_layout = false;
            return self.size;
        }

        let total_gap = self.gap.saturating_mul(child_count.saturating_sub(1) as u16);
        let mut total_flex = 0u16;
        let mut allocated_main = total_gap;
        let mut max_child_cross = 0u16;

        // Pass 1: Measure non-flex children
        for child in &mut self.children {
            if child.flex > 0 {
                total_flex += child.flex;
            } else {
                let inner_constraints = if is_vert {
                    BoxConstraints::new(
                        if self.cross_axis_alignment == CrossAxisAlignment::Stretch && max_cross < u16::MAX { max_cross } else { 0 },
                        max_cross,
                        0,
                        max_main.saturating_sub(allocated_main),
                    )
                } else {
                    BoxConstraints::new(
                        0,
                        max_main.saturating_sub(allocated_main),
                        if self.cross_axis_alignment == CrossAxisAlignment::Stretch && max_cross < u16::MAX { max_cross } else { 0 },
                        max_cross,
                    )
                };
                let child_size = child.render_box.layout(inner_constraints);
                let (child_main, child_cross) = if is_vert {
                    (child_size.height, child_size.width)
                } else {
                    (child_size.width, child_size.height)
                };
                allocated_main = allocated_main.saturating_add(child_main);
                max_child_cross = max_child_cross.max(child_cross);
            }
        }

        // Pass 2: Layout flex children with remaining space
        let free_main = max_main.saturating_sub(allocated_main);
        if total_flex > 0 && free_main > 0 {
            let mut flex_remainder = free_main;
            for child in &mut self.children {
                if child.flex > 0 {
                    let share = (free_main as u32 * child.flex as u32 / total_flex as u32) as u16;
                    let child_main_target = share.min(flex_remainder);
                    flex_remainder = flex_remainder.saturating_sub(child_main_target);

                    let min_main = if child.fit == FlexFit::Tight { child_main_target } else { 0 };
                    let inner_constraints = if is_vert {
                        BoxConstraints::new(
                            if self.cross_axis_alignment == CrossAxisAlignment::Stretch && max_cross < u16::MAX { max_cross } else { 0 },
                            max_cross,
                            min_main,
                            child_main_target,
                        )
                    } else {
                        BoxConstraints::new(
                            min_main,
                            child_main_target,
                            if self.cross_axis_alignment == CrossAxisAlignment::Stretch && max_cross < u16::MAX { max_cross } else { 0 },
                            max_cross,
                        )
                    };
                    let child_size = child.render_box.layout(inner_constraints);
                    let (child_main, child_cross) = if is_vert {
                        (child_size.height, child_size.width)
                    } else {
                        (child_size.width, child_size.height)
                    };
                    allocated_main = allocated_main.saturating_add(child_main);
                    max_child_cross = max_child_cross.max(child_cross);
                }
            }
        }

        // Determine container size
        let container_main = constraints.constrain_height(allocated_main);
        let actual_main = if is_vert {
            if constraints.min_height == constraints.max_height { constraints.max_height } else { container_main }
        } else {
            if constraints.min_width == constraints.max_width { constraints.max_width } else { constraints.constrain_width(allocated_main) }
        };

        let actual_cross = if self.cross_axis_alignment == CrossAxisAlignment::Stretch && max_cross < u16::MAX {
            max_cross
        } else {
            if is_vert { constraints.constrain_width(max_child_cross) } else { constraints.constrain_height(max_child_cross) }
        };

        // Pass 3: Position children along main and cross axes
        let surplus = actual_main.saturating_sub(allocated_main);
        let mut current_main = match self.main_axis_alignment {
            MainAxisAlignment::Start | MainAxisAlignment::SpaceBetween => 0,
            MainAxisAlignment::End => surplus,
            MainAxisAlignment::Center => surplus / 2,
            MainAxisAlignment::SpaceAround => surplus / (child_count as u16 * 2).max(1),
            MainAxisAlignment::SpaceEvenly => surplus / (child_count as u16 + 1).max(1),
        };

        let additional_space = match self.main_axis_alignment {
            MainAxisAlignment::SpaceBetween if child_count > 1 => surplus / (child_count as u16 - 1),
            MainAxisAlignment::SpaceAround if child_count > 0 => surplus / child_count as u16,
            MainAxisAlignment::SpaceEvenly if child_count > 0 => surplus / (child_count as u16 + 1),
            _ => 0,
        };

        for child in &mut self.children {
            let child_size = child.render_box.size();
            let (child_main, child_cross) = if is_vert {
                (child_size.height, child_size.width)
            } else {
                (child_size.width, child_size.height)
            };

            let cross_offset = match self.cross_axis_alignment {
                CrossAxisAlignment::Start | CrossAxisAlignment::Stretch => 0,
                CrossAxisAlignment::End => actual_cross.saturating_sub(child_cross),
                CrossAxisAlignment::Center => actual_cross.saturating_sub(child_cross) / 2,
            };

            let offset = if is_vert {
                Offset::new(cross_offset as i32, current_main as i32)
            } else {
                Offset::new(current_main as i32, cross_offset as i32)
            };
            child.render_box.set_offset(offset);

            current_main = current_main
                .saturating_add(child_main)
                .saturating_add(self.gap)
                .saturating_add(additional_space);
        }

        self.size = if is_vert {
            Size::new(actual_cross, actual_main)
        } else {
            Size::new(actual_main, actual_cross)
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

        // Reverse search children for hit test
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
