use crate::layout::Margin;

/// Two-dimensional size in integer terminal character cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Size {
    pub width: u16,
    pub height: u16,
}

impl Size {
    pub const ZERO: Self = Self { width: 0, height: 0 };

    pub const fn new(width: u16, height: u16) -> Self {
        Self { width, height }
    }

    pub const fn area(&self) -> u32 {
        self.width as u32 * self.height as u32
    }

    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// Two-dimensional offset in terminal character cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Offset {
    pub x: i32,
    pub y: i32,
}

impl Offset {
    pub const ZERO: Self = Self { x: 0, y: 0 };

    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    pub const fn add(&self, other: Offset) -> Offset {
        Offset {
            x: self.x + other.x,
            y: self.y + other.y,
        }
    }

    pub fn saturating_add_to_u16(&self, x: u16, y: u16) -> (u16, u16) {
        let rx = (x as i32 + self.x).clamp(0, u16::MAX as i32) as u16;
        let ry = (y as i32 + self.y).clamp(0, u16::MAX as i32) as u16;
        (rx, ry)
    }
}

/// Downward layout constraints enforcing min/max dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BoxConstraints {
    pub min_width: u16,
    pub max_width: u16,
    pub min_height: u16,
    pub max_height: u16,
}

impl Default for BoxConstraints {
    fn default() -> Self {
        Self {
            min_width: 0,
            max_width: u16::MAX,
            min_height: 0,
            max_height: u16::MAX,
        }
    }
}

impl BoxConstraints {
    pub const fn new(min_width: u16, max_width: u16, min_height: u16, max_height: u16) -> Self {
        let max_w = if max_width < min_width { min_width } else { max_width };
        let max_h = if max_height < min_height { min_height } else { max_height };
        Self {
            min_width,
            max_width: max_w,
            min_height,
            max_height: max_h,
        }
    }

    /// Fixed size constraint: min == max.
    pub const fn tight(size: Size) -> Self {
        Self {
            min_width: size.width,
            max_width: size.width,
            min_height: size.height,
            max_height: size.height,
        }
    }

    /// Loose constraint: min is 0, max is given size.
    pub const fn loose(size: Size) -> Self {
        Self {
            min_width: 0,
            max_width: size.width,
            min_height: 0,
            max_height: size.height,
        }
    }

    /// Tight constraint for specific axes if supplied.
    pub fn tight_for(width: Option<u16>, height: Option<u16>) -> Self {
        Self {
            min_width: width.unwrap_or(0),
            max_width: width.unwrap_or(u16::MAX),
            min_height: height.unwrap_or(0),
            max_height: height.unwrap_or(u16::MAX),
        }
    }

    /// Clamps a size to satisfy constraints.
    pub fn constrain(&self, size: Size) -> Size {
        Size {
            width: self.constrain_width(size.width),
            height: self.constrain_height(size.height),
        }
    }

    pub fn constrain_width(&self, width: u16) -> u16 {
        width.clamp(self.min_width, self.max_width)
    }

    pub fn constrain_height(&self, height: u16) -> u16 {
        height.clamp(self.min_height, self.max_height)
    }

    /// Shrinks constraints by horizontal and vertical margins.
    pub fn deflate(&self, margin: Margin) -> Self {
        let h = (margin.horizontal * 2).min(self.max_width);
        let v = (margin.vertical * 2).min(self.max_height);
        Self {
            min_width: self.min_width.saturating_sub(h),
            max_width: self.max_width.saturating_sub(h),
            min_height: self.min_height.saturating_sub(v),
            max_height: self.max_height.saturating_sub(v),
        }
    }

    pub const fn is_tight(&self) -> bool {
        self.min_width == self.max_width && self.min_height == self.max_height
    }

    pub const fn has_bounded_width(&self) -> bool {
        self.max_width < u16::MAX
    }

    pub const fn has_bounded_height(&self) -> bool {
        self.max_height < u16::MAX
    }

    /// Relax the minimum constraints to zero while keeping maximums.
    pub const fn loosen(&self) -> Self {
        Self {
            min_width: 0,
            max_width: self.max_width,
            min_height: 0,
            max_height: self.max_height,
        }
    }
}
