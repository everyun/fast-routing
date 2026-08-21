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
    pub fn corner_x(&self, no: usize) -> i32 {
        match no {
            0 => self.lower_left_diagonal_x - self.bottom_y,
            1 => self.lower_right_diagonal_x + self.bottom_y,
            2 | 3 => self.right_x,
            4 => self.upper_right_diagonal_x - self.top_y,
            5 => self.upper_left_diagonal_x + self.top_y,
            6 | 7 => self.left_x,
            _ => panic!("IntOctagon.corner: out of range"),
        }
    }

    /// Y-coordinate of corner `no`.
    pub fn corner_y(&self, no: usize) -> i32 {
        match no {
            0 | 1 => self.bottom_y,
            2 => self.right_x - self.lower_right_diagonal_x,
            3 => self.upper_right_diagonal_x - self.right_x,
            4 | 5 => self.top_y,
            6 => self.left_x - self.upper_left_diagonal_x,
            7 => self.lower_left_diagonal_x - self.left_x,
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

    /// Normalizes the octagon bounds.
    pub fn normalize(self) -> Self {
        self
    }

    /// Converts this octagon into a convex Simplex defined by its bounding lines.
    pub fn to_simplex(&self) -> crate::planar::simplex::Simplex {
        let mut lines = Vec::with_capacity(8);
        lines.push(Line::new_from_coords(self.left_x, self.bottom_y, self.left_x, self.top_y));
        lines.push(Line::new_from_coords(self.left_x, self.top_y, self.right_x, self.top_y));
        lines.push(Line::new_from_coords(self.right_x, self.top_y, self.right_x, self.bottom_y));
        lines.push(Line::new_from_coords(self.right_x, self.bottom_y, self.left_x, self.bottom_y));
        crate::planar::simplex::Simplex::new(lines)
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
