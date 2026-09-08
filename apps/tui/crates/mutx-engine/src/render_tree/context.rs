use crate::cell::{Cell, Style};
use crate::frame::{Frame, Widget};
use crate::layout::Rect;
use super::constraints::{Offset, Size};

/// An entry captured during hit-testing along the render tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HitTestEntry {
    pub id: Option<u64>,
    pub tag: &'static str,
    pub local_pos: (u16, u16),
    pub global_pos: (u16, u16),
}

/// Accumulator for hit-test targets under a screen position.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HitTestResult {
    pub entries: Vec<HitTestEntry>,
    pub absorbed: bool,
}

impl HitTestResult {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, entry: HitTestEntry) {
        self.entries.push(entry);
    }

    pub fn absorb(&mut self) {
        self.absorbed = true;
    }

    pub fn top(&self) -> Option<&HitTestEntry> {
        self.entries.last()
    }

    pub fn contains_id(&self, id: u64) -> bool {
        self.entries.iter().any(|entry| entry.id == Some(id))
    }

    pub fn contains_tag(&self, tag: &str) -> bool {
        self.entries.iter().any(|entry| entry.tag == tag)
    }
}

/// Scoped paint context managing coordinate translation and clipping boundaries.
pub struct PaintContext<'a, 'f> {
    frame: &'a mut Frame<'f>,
    offset_stack: Vec<Offset>,
    clip_stack: Vec<Rect>,
}

impl<'a, 'f> PaintContext<'a, 'f> {
    pub fn new(frame: &'a mut Frame<'f>) -> Self {
        let area = frame.area();
        Self {
            frame,
            offset_stack: vec![Offset::ZERO],
            clip_stack: vec![area],
        }
    }

    pub fn current_offset(&self) -> Offset {
        self.offset_stack.last().copied().unwrap_or(Offset::ZERO)
    }

    pub fn current_clip(&self) -> Rect {
        self.clip_stack.last().copied().unwrap_or(self.frame.area())
    }

    /// Scope painting within a translated local coordinate offset.
    pub fn with_offset<R>(&mut self, offset: Offset, f: impl FnOnce(&mut PaintContext<'_, 'f>) -> R) -> R {
        let combined = self.current_offset().add(offset);
        self.offset_stack.push(combined);
        let result = f(self);
        self.offset_stack.pop();
        result
    }

    /// Scope painting within an intersecting clipping box.
    pub fn with_clip<R>(&mut self, local_clip: Rect, f: impl FnOnce(&mut PaintContext<'_, 'f>) -> R) -> R {
        let (x, y) = self.current_offset().saturating_add_to_u16(local_clip.x, local_clip.y);
        let global_rect = Rect::new(x, y, local_clip.width, local_clip.height);
        let intersecting = self.current_clip().intersection(global_rect);
        self.clip_stack.push(intersecting);
        let result = f(self);
        self.clip_stack.pop();
        result
    }

    /// Put a styled string at local coordinates, subject to current offset and clipping.
    pub fn put(&mut self, local_x: u16, local_y: u16, style: Style, text: &str) {
        let (x, y) = self.current_offset().saturating_add_to_u16(local_x, local_y);
        let clip = self.current_clip();
        if y < clip.y || y >= clip.bottom() {
            return;
        }
        if x >= clip.right() {
            return;
        }

        // Clip text horizontally against the active clip boundary
        let start_col = x.max(clip.x);
        let end_col = clip.right();
        if start_col >= end_col {
            return;
        }

        // Draw clipped string
        let skip_chars = (start_col - x) as usize;
        let take_chars = (end_col - start_col) as usize;
        let clipped_text: String = text.chars().skip(skip_chars).take(take_chars).collect();
        if !clipped_text.is_empty() {
            self.frame.put(start_col, y, style, &clipped_text);
        }
    }

    /// Fill a local rect with a cell style/symbol, respecting clipping.
    pub fn fill_rect(&mut self, local_rect: Rect, cell: Cell) {
        let (gx, gy) = self.current_offset().saturating_add_to_u16(local_rect.x, local_rect.y);
        let global_rect = Rect::new(gx, gy, local_rect.width, local_rect.height);
        let effective = self.current_clip().intersection(global_rect);
        if effective.area() == 0 {
            return;
        }

        let grid = self.frame.buffer_mut();
        for y in effective.y..effective.bottom() {
            for x in effective.x..effective.right() {
                grid.set(x, y, cell.clone());
            }
        }
    }

    /// Render a native widget at local coordinates.
    pub fn render_widget<W: Widget>(&mut self, widget: W, local_rect: Rect) {
        let (gx, gy) = self.current_offset().saturating_add_to_u16(local_rect.x, local_rect.y);
        let global_rect = Rect::new(gx, gy, local_rect.width, local_rect.height);
        let effective = self.current_clip().intersection(global_rect);
        if effective.area() == 0 {
            return;
        }
        self.frame.paint_clipped(effective, |f| {
            widget.render(global_rect, f.buffer_mut());
        });
    }

    pub fn viewport(&self) -> Size {
        let a = self.frame.area();
        Size::new(a.width, a.height)
    }
}
