/// A 2D rectangular region in terminal cell coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    pub const fn right(self) -> u16 {
        self.x.saturating_add(self.width)
    }

    pub const fn bottom(self) -> u16 {
        self.y.saturating_add(self.height)
    }

    /// Returns the inner rectangle shrunk by `(margin_x, margin_y)` on each side.
    pub const fn inner(self, margin_x: u16, margin_y: u16) -> Self {
        let double_x = margin_x.saturating_mul(2);
        let double_y = margin_y.saturating_mul(2);
        if self.width <= double_x || self.height <= double_y {
            Self::new(self.x, self.y, 0, 0)
        } else {
            Self::new(
                self.x.saturating_add(margin_x),
                self.y.saturating_add(margin_y),
                self.width - double_x,
                self.height - double_y,
            )
        }
    }

    /// Returns whether `(col, row)` falls inside this rectangle.
    pub const fn contains(self, col: u16, row: u16) -> bool {
        col >= self.x && col < self.right() && row >= self.y && row < self.bottom()
    }

    /// Computes the intersection of two rectangles.
    pub fn intersection(self, other: Self) -> Self {
        let x1 = self.x.max(other.x);
        let y1 = self.y.max(other.y);
        let x2 = self.right().min(other.right());
        let y2 = self.bottom().min(other.bottom());
        Self {
            x: x1,
            y: y1,
            width: x2.saturating_sub(x1),
            height: y2.saturating_sub(y1),
        }
    }
}
