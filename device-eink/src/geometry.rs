//! Screen geometry — a single axis-aligned rectangle (RR2).
//!
//! `x`/`y` are signed (a dirty rect can originate off-screen during a fling);
//! `w`/`h` are unsigned extents. The policy speaks only in [`Rect`]s; the Kotlin
//! adapter maps them to panel coordinates.

/// An axis-aligned rectangle in screen/page pixel space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rect {
    /// Left edge (signed: may be negative while content is scrolling on-screen).
    pub x: i32,
    /// Top edge (signed).
    pub y: i32,
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
}

impl Rect {
    /// A rectangle at `(x, y)` with extent `w × h`.
    #[must_use]
    pub const fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    /// The full-screen rectangle anchored at the origin for a `width × height` panel.
    #[must_use]
    pub const fn full(width: u32, height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            w: width,
            h: height,
        }
    }

    /// The smallest rectangle covering both `self` and `other`.
    ///
    /// Empty rectangles (zero `w` or `h`) are treated as "no contribution" so unioning
    /// with an empty rect returns the other rect; unioning two empties yields an empty
    /// rect at `self`'s origin.
    #[must_use]
    pub fn union(self, other: Rect) -> Rect {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let left = self.x.min(other.x);
        let top = self.y.min(other.y);
        // Right/bottom are computed in i64 to avoid overflow, then clamped non-negative.
        let right = (self.x as i64 + self.w as i64).max(other.x as i64 + other.w as i64);
        let bottom = (self.y as i64 + self.h as i64).max(other.y as i64 + other.h as i64);
        Rect {
            x: left,
            y: top,
            w: (right - left as i64).max(0) as u32,
            h: (bottom - top as i64).max(0) as u32,
        }
    }

    /// True when the rectangle has zero area.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.w == 0 || self.h == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_and_full() {
        assert_eq!(
            Rect::new(1, 2, 3, 4),
            Rect {
                x: 1,
                y: 2,
                w: 3,
                h: 4
            }
        );
        assert_eq!(Rect::full(800, 600), Rect::new(0, 0, 800, 600));
    }

    #[test]
    fn union_of_disjoint_rects_covers_both() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(20, 20, 5, 5);
        // bounding box: x..=24, y..=24  => origin (0,0), 25x25
        assert_eq!(a.union(b), Rect::new(0, 0, 25, 25));
    }

    #[test]
    fn union_with_empty_is_identity() {
        let a = Rect::new(3, 4, 10, 10);
        let empty = Rect::new(100, 100, 0, 0);
        assert_eq!(a.union(empty), a);
        assert_eq!(empty.union(a), a);
    }

    #[test]
    fn union_handles_negative_origin() {
        let a = Rect::new(-5, -5, 10, 10); // spans -5..5
        let b = Rect::new(0, 0, 10, 10); // spans 0..10
        assert_eq!(a.union(b), Rect::new(-5, -5, 15, 15));
    }

    #[test]
    fn union_is_commutative() {
        let a = Rect::new(2, 3, 7, 9);
        let b = Rect::new(-1, 4, 3, 3);
        assert_eq!(a.union(b), b.union(a));
    }
}
