//! Integer octagon with 45-degree angle constraints, ported from
//! `app.freerouting.geometry.planar.IntOctagon`.

use crate::planar::{
    area::Area, float_point::FloatPoint, int_box::IntBox, int_point::IntPoint, limits::Limits,
    line::Line, point::Point, vector::Vector,
};

/// An octagon with integer coordinates and 45-degree angle constraints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IntOctagon {
    // Vertical boundaries (east/west)
    pub left_x: i32,
    pub right_x: i32,

    // Horizontal boundaries (north/south)
    pub bottom_y: i32,
    pub top_y: i32,

    // Diagonal boundaries at +45° angle (x - y = c)
    pub upper_left_diagonal_x: i32,
    pub lower_right_diagonal_x: i32,

    // Diagonal boundaries at -45° angle (x + y = c)
    pub lower_left_diagonal_x: i32,
    pub upper_right_diagonal_x: i32,
}

impl IntOctagon {
    /// Reusable instance of an empty octagon.
    pub const EMPTY: IntOctagon = IntOctagon {
        left_x: Limits::CRIT_INT,
        bottom_y: Limits::CRIT_INT,
        right_x: -Limits::CRIT_INT,
        top_y: -Limits::CRIT_INT,
        upper_left_diagonal_x: Limits::CRIT_INT,
        lower_right_diagonal_x: -Limits::CRIT_INT,
        lower_left_diagonal_x: Limits::CRIT_INT,
        upper_right_diagonal_x: -Limits::CRIT_INT,
    };

    /// Creates an `IntOctagon` from 8 integer boundary values.
    ///
    /// The parameter order matches the upstream constructor:
    /// `(lx, ly, rx, uy, ulx, lrx, llx, urx)`.
    pub const fn new(
        lx: i32,
        ly: i32,
        rx: i32,
        uy: i32,
        ulx: i32,
        lrx: i32,
        llx: i32,
        urx: i32,
    ) -> Self {
        IntOctagon {
            left_x: lx,
            bottom_y: ly,
            right_x: rx,
            top_y: uy,
            upper_left_diagonal_x: ulx,
            lower_right_diagonal_x: lrx,
            lower_left_diagonal_x: llx,
            upper_right_diagonal_x: urx,
        }
    }

    /// Returns true if this octagon is empty.
    pub fn is_empty(&self) -> bool {
        *self == Self::EMPTY
            || self.left_x > self.right_x
            || self.bottom_y > self.top_y
            || self.upper_left_diagonal_x > self.lower_right_diagonal_x
            || self.lower_left_diagonal_x > self.upper_right_diagonal_x
    }

    /// Bounding box of this octagon.
    pub fn bounding_box(&self) -> IntBox {
        IntBox::new(self.left_x, self.bottom_y, self.right_x, self.top_y)
    }

    /// Self bounding octagon.
    pub fn bounding_octagon(&self) -> IntOctagon {
        *self
    }

    /// Dimension of the octagon: -1 for empty, 0 for point, 1 for line, 2 for 2D.
    pub fn dimension(&self) -> i32 {
        if self.is_empty() {
            -1
        } else if self.right_x > self.left_x
            && self.top_y > self.bottom_y
            && self.lower_right_diagonal_x > self.upper_left_diagonal_x
            && self.upper_right_diagonal_x > self.lower_left_diagonal_x
        {
            2
        } else if self.right_x == self.left_x && self.top_y == self.bottom_y {
            0
        } else {
            1
        }
    }

    /// Returns the `no`-th corner of this octagon (0..8).
    pub fn corner(&self, no: usize) -> IntPoint {
        IntPoint::new(self.corner_x(no), self.corner_y(no))
    }

    /// X-coordinate of corner `no`.
    ///
    /// Uses saturating arithmetic so degenerate or unbounded octagons
    /// (e.g. `EMPTY` or bounds near the integer limits) stay finite.
    pub fn corner_x(&self, no: usize) -> i32 {
        match no {
            0 => self.lower_left_diagonal_x.saturating_sub(self.bottom_y),
            1 => self.lower_right_diagonal_x.saturating_add(self.bottom_y),
            2 | 3 => self.right_x,
            4 => self.upper_right_diagonal_x.saturating_sub(self.top_y),
            5 => self.upper_left_diagonal_x.saturating_add(self.top_y),
            6 | 7 => self.left_x,
            _ => panic!("IntOctagon.corner: out of range"),
        }
    }

    /// Y-coordinate of corner `no`.
    ///
    /// Uses saturating arithmetic so degenerate or unbounded octagons
    /// (e.g. `EMPTY` or bounds near the integer limits) stay finite.
    pub fn corner_y(&self, no: usize) -> i32 {
        match no {
            0 | 1 => self.bottom_y,
            2 => self.right_x.saturating_sub(self.lower_right_diagonal_x),
            3 => self.upper_right_diagonal_x.saturating_sub(self.right_x),
            4 | 5 => self.top_y,
            6 => self.left_x.saturating_sub(self.upper_left_diagonal_x),
            7 => self.lower_left_diagonal_x.saturating_sub(self.left_x),
            _ => panic!("IntOctagon.corner: out of range"),
        }
    }

    /// Stable identifier.
    pub fn get_id_no(&self) -> i32 {
        let mut result = self.left_x;
        result = 31 * result + self.right_x;
        result = 31 * result + self.bottom_y;
        result = 31 * result + self.top_y;
        result = 31 * result + self.lower_left_diagonal_x;
        result = 31 * result + self.upper_right_diagonal_x;
        result = 31 * result + self.upper_left_diagonal_x;
        31 * result + self.lower_right_diagonal_x
    }

    /// Area of the octagon via surveyor's formula.
    pub fn area(&self) -> f64 {
        let mut sum = 0.0;
        for i in 0..8 {
            let prev = if i == 0 { 7 } else { i - 1 };
            let next = if i == 7 { 0 } else { i + 1 };
            let xi = self.corner_x(i) as f64;
            let y_prev = self.corner_y(prev) as f64;
            let y_next = self.corner_y(next) as f64;
            sum += xi * (y_next - y_prev);
        }
        0.5 * sum.abs()
    }

    /// Returns the `no`-th border line (0..8).
    pub fn border_line(&self, no: usize) -> Line {
        let (ax, ay, bx, by) = match no {
            0 => (0, self.bottom_y, 1, self.bottom_y),
            1 => (self.lower_right_diagonal_x, 0, self.lower_right_diagonal_x + 1, 1),
            2 => (self.right_x, 0, self.right_x, 1),
            3 => (self.upper_right_diagonal_x, 0, self.upper_right_diagonal_x - 1, 1),
            4 => (0, self.top_y, -1, self.top_y),
            5 => (self.upper_left_diagonal_x, 0, self.upper_left_diagonal_x - 1, -1),
            6 => (self.left_x, 0, self.left_x, -1),
            7 => (self.lower_left_diagonal_x, 0, self.lower_left_diagonal_x + 1, -1),
            _ => panic!("IntOctagon.border_line: out of range"),
        };
        Line::new_from_coords(ax, ay, bx, by)
    }

    /// Enlarges the octagon by `dist` (positive = expand outward).
    pub fn offset(&self, dist: f64) -> IntOctagon {
        if dist == 0.0 || self.is_empty() {
            return *self;
        }
        let rounded_dist = dist.round() as i32;
        let diagonal_dist = (dist * Limits::SQRT2).round() as i32;
        IntOctagon::new(
            self.left_x - rounded_dist,
            self.bottom_y - rounded_dist,
            self.right_x + rounded_dist,
            self.top_y + rounded_dist,
            self.upper_left_diagonal_x - diagonal_dist,
            self.lower_right_diagonal_x + diagonal_dist,
            self.lower_left_diagonal_x - diagonal_dist,
            self.upper_right_diagonal_x + diagonal_dist,
        )
    }

    /// Intersection with another octagon.
    pub fn intersection(&self, other: &IntOctagon) -> IntOctagon {
        let lx = self.left_x.max(other.left_x);
        let rx = self.right_x.min(other.right_x);
        if lx > rx {
            return IntOctagon::EMPTY;
        }
        let ly = self.bottom_y.max(other.bottom_y);
        let uy = self.top_y.min(other.top_y);
        if ly > uy {
            return IntOctagon::EMPTY;
        }
        let ulx = self.upper_left_diagonal_x.max(other.upper_left_diagonal_x);
        let lrx = self.lower_right_diagonal_x.min(other.lower_right_diagonal_x);
        if ulx > lrx {
            return IntOctagon::EMPTY;
        }
        let llx = self.lower_left_diagonal_x.max(other.lower_left_diagonal_x);
        let urx = self.upper_right_diagonal_x.min(other.upper_right_diagonal_x);
        if llx > urx {
            return IntOctagon::EMPTY;
        }
        IntOctagon::new(lx, ly, rx, uy, ulx, lrx, llx, urx)
    }

    /// Returns true if this octagon intersects with `other`.
    pub fn intersects(&self, other: &IntOctagon) -> bool {
        !self.intersection(other).is_empty()
    }

    /// Translates this octagon by `vector`.
    pub fn translate_by(&self, vector: &Vector) -> IntOctagon {
        if vector.is_zero() {
            return *self;
        }
        let (vx, vy) = match vector {
            Vector::Int(iv) => (iv.x, iv.y),
            Vector::Rational(_) => panic!("IntOctagon translateBy only implemented for integer vectors"),
        };
        IntOctagon::new(
            self.left_x + vx,
            self.bottom_y + vy,
            self.right_x + vx,
            self.top_y + vy,
            self.upper_left_diagonal_x + vx - vy,
            self.lower_right_diagonal_x + vx - vy,
            self.lower_left_diagonal_x + vx + vy,
            self.upper_right_diagonal_x + vx + vy,
        )
    }

    /// Normalizes the octagon bounds so that each of the 8 border lines
    /// touches the octagon, like the upstream `IntOctagon.normalize`.
    ///
    /// Each bound is mutually clamped against the others (orthogonal bounds
    /// against diagonal bounds and vice versa, plus the intersections of
    /// adjacent diagonal border lines) until a fixed point is reached.
    /// Returns `EMPTY` if the bounds cross, so `is_empty()` is reliable on
    /// the canonical form.
    pub fn normalize(self) -> Self {
        // floor and ceiling of `v / 2` for the diagonal line intersections
        fn div2_floor(v: i64) -> i64 {
            v.div_euclid(2)
        }
        fn div2_ceil(v: i64) -> i64 {
            (v + 1).div_euclid(2)
        }

        // Work in i64 so intermediate sums/differences of i32 bounds cannot
        // overflow; non-empty fixed-point values always fit back into i32.
        let mut lx = self.left_x as i64;
        let mut ly = self.bottom_y as i64;
        let mut rx = self.right_x as i64;
        let mut uy = self.top_y as i64;
        let mut ulx = self.upper_left_diagonal_x as i64;
        let mut lrx = self.lower_right_diagonal_x as i64;
        let mut llx = self.lower_left_diagonal_x as i64;
        let mut urx = self.upper_right_diagonal_x as i64;

        loop {
            if lx > rx || ly > uy || llx > urx || ulx > lrx {
                return IntOctagon::EMPTY;
            }
            let prev = (lx, ly, rx, uy, ulx, lrx, llx, urx);

            // Clamp the orthogonal bounds against the diagonal bounds:
            // llx <= x + y <= urx and ulx <= x - y <= lrx restrict x and y.
            lx = lx.max(llx - uy).max(ulx + ly);
            rx = rx.min(urx - ly).min(lrx + uy);
            ly = ly.max(lx - lrx).max(llx - rx);
            uy = uy.min(urx - lx).min(rx - ulx);

            // Clamp the diagonal bounds against the corner points of the box.
            llx = llx.max(lx + ly);
            lrx = lrx.min(rx - ly);
            urx = urx.min(rx + uy);
            ulx = ulx.max(lx - uy);

            // Clamp the orthogonal bounds against the intersections of
            // adjacent diagonal border lines, e.g. the upper left line
            // x - y = ulx and the upper right line x + y = urx intersect
            // at y = (urx - ulx) / 2.
            uy = uy.min(div2_ceil(urx - ulx));
            ly = ly.max(div2_floor(llx - lrx));
            rx = rx.min(div2_ceil(urx + lrx));
            lx = lx.max(div2_floor(llx + ulx));

            if (lx, ly, rx, uy, ulx, lrx, llx, urx) == prev {
                break;
            }
        }
        IntOctagon::new(
            lx as i32, ly as i32, rx as i32, uy as i32, ulx as i32, lrx as i32, llx as i32,
            urx as i32,
        )
    }

    /// Converts this octagon into a convex Simplex defined by its 8 border lines.
    pub fn to_simplex(&self) -> crate::planar::simplex::Simplex {
        if self.is_empty() {
            return crate::planar::simplex::Simplex::EMPTY;
        }
        let lines: Vec<Line> = (0..8).map(|no| self.border_line(no)).collect();
        crate::planar::simplex::Simplex::new(lines).remove_redundant_lines()
    }
}

impl crate::planar::area::Area for IntOctagon {
    fn is_empty(&self) -> bool {
        self.is_empty()
    }

    fn is_bounded(&self) -> bool {
        !self.is_empty()
    }

    fn dimension(&self) -> i32 {
        self.dimension()
    }

    fn is_contained_in(&self, box_: &IntBox) -> bool {
        let b = self.bounding_box();
        box_.left_x() <= b.left_x()
            && box_.right_x() >= b.right_x()
            && box_.bottom_y() <= b.bottom_y()
            && box_.top_y() >= b.top_y()
    }

    fn bounding_box(&self) -> IntBox {
        self.bounding_box()
    }

    fn bounding_octagon(&self) -> IntOctagon {
        *self
    }

    fn contains_point(&self, point: &Point) -> bool {
        match point {
            Point::Int(ip) => {
                ip.x >= self.left_x
                    && ip.x <= self.right_x
                    && ip.y >= self.bottom_y
                    && ip.y <= self.top_y
                    && (ip.x - ip.y) >= self.upper_left_diagonal_x
                    && (ip.x - ip.y) <= self.lower_right_diagonal_x
                    && (ip.x + ip.y) >= self.lower_left_diagonal_x
                    && (ip.x + ip.y) <= self.upper_right_diagonal_x
            }
            Point::Rational(rp) => {
                let fp = rp.to_float();
                fp.x >= self.left_x as f64
                    && fp.x <= self.right_x as f64
                    && fp.y >= self.bottom_y as f64
                    && fp.y <= self.top_y as f64
            }
        }
    }

    fn contains_float_point(&self, point: &FloatPoint) -> bool {
        let fp = *point;
        fp.x >= self.left_x as f64
            && fp.x <= self.right_x as f64
            && fp.y >= self.bottom_y as f64
            && fp.y <= self.top_y as f64
    }

    fn nearest_point_approx(&self, from_point: &FloatPoint) -> FloatPoint {
        FloatPoint::new(
            from_point.x.clamp(self.left_x as f64, self.right_x as f64),
            from_point.y.clamp(self.bottom_y as f64, self.top_y as f64),
        )
    }

    fn corner_approx_arr(&self) -> Vec<FloatPoint> {
        let b = self.bounding_box();
        vec![
            FloatPoint::new(b.left_x() as f64, b.bottom_y() as f64),
            FloatPoint::new(b.right_x() as f64, b.bottom_y() as f64),
            FloatPoint::new(b.right_x() as f64, b.top_y() as f64),
            FloatPoint::new(b.left_x() as f64, b.top_y() as f64),
        ]
    }
}

impl crate::planar::shape::Shape for IntOctagon {
    fn circumference(&self) -> f64 {
        2.0 * ((self.right_x - self.left_x) + (self.top_y - self.bottom_y)) as f64
    }

    fn area(&self) -> f64 {
        self.area()
    }

    fn centre_of_gravity(&self) -> FloatPoint {
        let b = self.bounding_box();
        FloatPoint::new(
            (b.left_x() + b.right_x()) as f64 * 0.5,
            (b.bottom_y() + b.top_y()) as f64 * 0.5,
        )
    }

    fn is_outside(&self, point: &Point) -> bool {
        !self.contains_point(point)
    }

    fn contains_inside(&self, point: &Point) -> bool {
        self.contains_point(point)
    }

    fn contains_on_border(&self, point: &Point) -> bool {
        self.contains_point(point)
    }

    fn distance(&self, point: &FloatPoint) -> f64 {
        self.nearest_point_approx(point).distance(point)
    }

    fn border_distance(&self, point: &FloatPoint) -> f64 {
        self.distance(point)
    }

    fn smallest_radius(&self) -> f64 {
        let b = self.bounding_box();
        (b.width().min(b.height()) as f64) * 0.5
    }
}

impl crate::planar::convex_shape::ConvexShape for IntOctagon {
    fn max_width(&self) -> f64 {
        let b = self.bounding_box();
        ((b.width() as f64).powi(2) + (b.height() as f64).powi(2)).sqrt()
    }

    fn min_width(&self) -> f64 {
        let b = self.bounding_box();
        b.width().min(b.height()) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Octagon inscribed in the box `(0, 0, 100, 100)` with all 4 corners
    /// clipped by diagonal border lines; already in canonical form.
    fn proper_octagon() -> IntOctagon {
        IntOctagon::new(0, 0, 100, 100, -80, 80, 20, 180)
    }

    #[test]
    fn normalize_is_identity_on_canonical_octagon() {
        let oct = proper_octagon();
        assert_eq!(oct.normalize(), oct);
        assert_eq!(IntOctagon::EMPTY.normalize(), IntOctagon::EMPTY);
    }

    #[test]
    fn normalize_tightens_all_bounds_to_touching_border_lines() {
        // Wide-open orthogonal bounds around the wedge x >= |y| clipped at
        // x <= 1000. The left bound can only be tightened via the
        // intersection of the two left diagonal border lines.
        let oct = IntOctagon::new(-1000, -1000, 1000, 1000, 0, 2000, 0, 2000).normalize();
        assert_eq!(oct, IntOctagon::new(0, -1000, 1000, 1000, 0, 2000, 0, 2000));
        // The degenerate left edge collapses to the single point (0, 0).
        assert_eq!(oct.corner(6), IntPoint::new(0, 0));
        assert_eq!(oct.corner(7), IntPoint::new(0, 0));
        // Normalization is idempotent.
        assert_eq!(oct.normalize(), oct);
    }

    #[test]
    fn normalize_reports_geometrically_empty_octagon() {
        // No bound pair crosses directly, but x + y >= 100 excludes the
        // whole box [0, 10] x [0, 10].
        let oct = IntOctagon::new(0, 0, 10, 10, -50, 50, 100, 200);
        assert_eq!(oct.normalize(), IntOctagon::EMPTY);
        assert!(oct.normalize().is_empty());
    }

    #[test]
    fn degenerate_octagon_corners_stay_finite() {
        // Saturating corner arithmetic must not overflow when the diagonal
        // bounds are unbounded (like a simplex whose diagonal constraints
        // are missing): e.g. corner 0 is llx - ly = i32::MIN - 1.
        let unbounded_diagonals =
            IntOctagon::new(0, 1, 100, 100, i32::MIN, i32::MAX, i32::MIN, i32::MAX);
        for no in 0..8 {
            let _ = IntOctagon::EMPTY.corner(no);
            let _ = unbounded_diagonals.corner(no);
        }
    }

    #[test]
    fn to_simplex_keeps_all_8_border_lines() {
        let oct = proper_octagon();
        let simplex = oct.to_simplex();
        assert_eq!(simplex.border_line_count(), 8);
        // The diagonal constraints must survive: (5, 5) satisfies the box
        // bounds but violates x + y >= 20.
        let inside = Point::Int(IntPoint::new(50, 50));
        let clipped = Point::Int(IntPoint::new(5, 5));
        assert!(oct.contains_point(&inside));
        assert!(simplex.contains(&inside));
        assert!(!oct.contains_point(&clipped));
        assert!(!simplex.contains(&clipped));
    }

    #[test]
    fn to_simplex_contains_matches_octagon_contains() {
        let oct = proper_octagon();
        let simplex = oct.to_simplex();
        for x in (-20..=120).step_by(10) {
            for y in (-20..=120).step_by(10) {
                let p = Point::Int(IntPoint::new(x, y));
                assert_eq!(
                    simplex.contains(&p),
                    oct.contains_point(&p),
                    "containment mismatch at ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn to_simplex_of_empty_octagon_is_empty() {
        assert!(IntOctagon::EMPTY.to_simplex().is_empty());
    }
}
