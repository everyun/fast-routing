//! Integer bounding box, ported from `app.freerouting.geometry.planar.IntBox`.

use crate::planar::{
    area::Area, convex_shape::ConvexShape, direction::Direction, float_point::FloatPoint,
    int_octagon::IntOctagon, int_point::IntPoint, limits::Limits, line::Line, point::Point,
    shape::Shape, side::Side, simplex::Simplex, vector::Vector,
};

/// An orthogonal rectangle in the plane with integer coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IntBox {
    pub ll: IntPoint,
    pub ur: IntPoint,
}

impl IntBox {
    /// Standard implementation of an empty box.
    pub const EMPTY: IntBox = IntBox {
        ll: IntPoint::new(Limits::CRIT_INT, Limits::CRIT_INT),
        ur: IntPoint::new(-Limits::CRIT_INT, -Limits::CRIT_INT),
    };

    /// Creates an `IntBox` from lower-left and upper-right corners.
    pub const fn new_from_points(ll: IntPoint, ur: IntPoint) -> Self {
        IntBox { ll, ur }
    }

    /// Creates an `IntBox` from four integer coordinates, normalizing bounds.
    pub fn new(ll_x: i32, ll_y: i32, ur_x: i32, ur_y: i32) -> Self {
        IntBox {
            ll: IntPoint::new(ll_x.min(ur_x), ll_y.min(ur_y)),
            ur: IntPoint::new(ll_x.max(ur_x), ll_y.max(ur_y)),
        }
    }

    /// Returns true if the box is empty.
    pub fn is_empty(&self) -> bool {
        self.ll.x > self.ur.x || self.ll.y > self.ur.y
    }

    /// Number of border lines (always 4).
    pub fn border_line_count(&self) -> usize {
        4
    }

    /// Horizontal extension of the box.
    pub fn width(&self) -> i32 {
        self.ur.x - self.ll.x
    }

    /// Vertical extension of the box.
    pub fn height(&self) -> i32 {
        self.ur.y - self.ll.y
    }

    /// Max of width and height.
    pub fn max_width(&self) -> f64 {
        (self.ur.x - self.ll.x).max(self.ur.y - self.ll.y) as f64
    }

    /// Min of width and height.
    pub fn min_width(&self) -> f64 {
        (self.ur.x - self.ll.x).min(self.ur.y - self.ll.y) as f64
    }

    /// Area of the box.
    pub fn area(&self) -> f64 {
        ((self.ur.x - self.ll.x) as f64) * ((self.ur.y - self.ll.y) as f64)
    }

    /// Circumference of the box.
    pub fn circumference(&self) -> f64 {
        2.0 * ((self.ur.x - self.ll.x) + (self.ur.y - self.ll.y)) as f64
    }

    /// Corner `no`: 0 = ll, 1 = (ur.x, ll.y), 2 = ur, 3 = (ll.x, ur.y).
    pub fn corner(&self, no: usize) -> IntPoint {
        match no {
            0 => self.ll,
            1 => IntPoint::new(self.ur.x, self.ll.y),
            2 => self.ur,
            3 => IntPoint::new(self.ll.x, self.ur.y),
            _ => panic!("IntBox.corner: out of range"),
        }
    }

    /// Dimension of the box: -1 for empty, 0 for single point, 1 for line segment, 2 for 2D.
    pub fn dimension(&self) -> i32 {
        if self.is_empty() {
            -1
        } else if self.ll == self.ur {
            0
        } else if self.ur.x == self.ll.x || self.ll.y == self.ur.y {
            1
        } else {
            2
        }
    }

    /// Returns true if `point` is in the strict interior.
    pub fn contains_inside(&self, point: &IntPoint) -> bool {
        point.x > self.ll.x && point.x < self.ur.x && point.y > self.ll.y && point.y < self.ur.y
    }

    /// Nearest point on this box to `from_point`.
    pub fn nearest_point(&self, from_point: &FloatPoint) -> FloatPoint {
        let x = if from_point.x <= self.ll.x as f64 {
            self.ll.x as f64
        } else if from_point.x >= self.ur.x as f64 {
            self.ur.x as f64
        } else {
            from_point.x
        };

        let y = if from_point.y <= self.ll.y as f64 {
            self.ll.y as f64
        } else if from_point.y >= self.ur.y as f64 {
            self.ur.y as f64
        } else {
            from_point.y
        };

        FloatPoint::new(x, y)
    }

    /// Distance of this box to `from_point`.
    pub fn distance(&self, from_point: &FloatPoint) -> f64 {
        from_point.distance(&self.nearest_point(from_point))
    }

    /// Weighted distance to `other` box.
    pub fn weighted_distance(
        &self,
        other: &IntBox,
        horizontal_weight: f64,
        vertical_weight: f64,
    ) -> f64 {
        let max_ll_x = (self.ll.x.max(other.ll.x)) as f64;
        let max_ll_y = (self.ll.y.max(other.ll.y)) as f64;
        let min_ur_x = (self.ur.x.min(other.ur.x)) as f64;
        let min_ur_y = (self.ur.y.min(other.ur.y)) as f64;

        if min_ur_x >= max_ll_x {
            (vertical_weight * (max_ll_y - min_ur_y)).max(0.0)
        } else if min_ur_y >= max_ll_y {
            (horizontal_weight * (max_ll_x - min_ur_x)).max(0.0)
        } else {
            let delta_x = (max_ll_x - min_ur_x) * horizontal_weight;
            let delta_y = (max_ll_y - min_ur_y) * vertical_weight;
            (delta_x * delta_x + delta_y * delta_y).sqrt()
        }
    }

    /// Stable identifier.
    pub fn get_id_no(&self) -> i32 {
        31 * self.ll.get_id_no() + self.ur.get_id_no()
    }

    /// Converts this box to an equivalent `IntOctagon`.
    pub fn to_int_octagon(&self) -> IntOctagon {
        IntOctagon::new(
            self.ll.x,
            self.ll.y,
            self.ur.x,
            self.ur.y,
            self.ll.x - self.ur.y,
            self.ur.x - self.ll.y,
            self.ll.x + self.ll.y,
            self.ur.x + self.ur.y,
        )
    }

    /// Bounding octagon of this box.
    pub fn bounding_octagon(&self) -> IntOctagon {
        self.to_int_octagon()
    }

    /// Union with another `IntBox`.
    pub fn union(&self, other: &IntBox) -> IntBox {
        let llx = self.ll.x.min(other.ll.x);
        let lly = self.ll.y.min(other.ll.y);
        let urx = self.ur.x.max(other.ur.x);
        let ury = self.ur.y.max(other.ur.y);
        IntBox::new(llx, lly, urx, ury)
    }

    /// Intersection with another `IntBox`.
    pub fn intersection(&self, other: &IntBox) -> IntBox {
        if other.ll.x > self.ur.x
            || other.ll.y > self.ur.y
            || self.ll.x > other.ur.x
            || self.ll.y > other.ur.y
        {
            return IntBox::EMPTY;
        }
        let llx = self.ll.x.max(other.ll.x);
        let urx = self.ur.x.min(other.ur.x);
        let lly = self.ll.y.max(other.ll.y);
        let ury = self.ur.y.min(other.ur.y);
        IntBox::new(llx, lly, urx, ury)
    }

    /// Returns true if this box intersects with `other`.
    pub fn intersects(&self, other: &IntBox) -> bool {
        !(other.ll.x > self.ur.x
            || other.ll.y > self.ur.y
            || self.ll.x > other.ur.x
            || self.ll.y > other.ur.y)
    }

    /// Returns true if the intersection is 2-dimensional.
    pub fn overlaps(&self, other: &IntBox) -> bool {
        !(other.ll.x >= self.ur.x
            || other.ll.y >= self.ur.y
            || self.ll.x >= other.ur.x
            || self.ll.y >= other.ur.y)
    }

    /// Translates this box by `vector`.
    pub fn translate_by(&self, vector: &Vector) -> IntBox {
        if vector.is_zero() {
            return *self;
        }
        let new_ll = match self.ll.translate_by(vector) {
            crate::planar::Point::Int(ip) => ip,
            _ => panic!("IntBox translateBy only implemented for integer vectors"),
        };
        let new_ur = match self.ur.translate_by(vector) {
            crate::planar::Point::Int(ip) => ip,
            _ => panic!("IntBox translateBy only implemented for integer vectors"),
        };
        IntBox::new_from_points(new_ll, new_ur)
    }

    /// Turns this box by `factor` times 90° around `pole`.
    pub fn turn_90_degree(&self, factor: i32, pole: &IntPoint) -> IntBox {
        let p1 = self.ll.turn_90_degree(factor, pole);
        let p2 = self.ur.turn_90_degree(factor, pole);
        let llx = p1.x.min(p2.x);
        let lly = p1.y.min(p2.y);
        let urx = p1.x.max(p2.x);
        let ury = p1.y.max(p2.y);
        IntBox::new(llx, lly, urx, ury)
    }

    /// Returns the `no`-th border line (0 = lower, 1 = right, 2 = upper, 3 = left).
    pub fn border_line(&self, no: usize) -> Line {
        let (ax, ay, bx, by) = match no {
            0 => (0, self.ll.y, 1, self.ll.y),
            1 => (self.ur.x, 0, self.ur.x, 1),
            2 => (0, self.ur.y, -1, self.ur.y),
            3 => (self.ll.x, 0, self.ll.x, -1),
            _ => panic!("IntBox.border_line: out of range"),
        };
        Line::new_from_coords(ax, ay, bx, by)
    }

    /// Offsets the box by `dist` (positive = expand outward).
    pub fn offset(&self, dist: f64) -> IntBox {
        if dist == 0.0 || self.is_empty() {
            return *self;
        }
        let rounded = dist.round() as i32;
        let lower_left = IntPoint::new(self.ll.x - rounded, self.ll.y - rounded);
        let upper_right = IntPoint::new(self.ur.x + rounded, self.ur.y + rounded);
        IntBox::new_from_points(lower_left, upper_right)
    }

    /// Horizontal offset only.
    pub fn horizontal_offset(&self, dist: f64) -> IntBox {
        if dist == 0.0 || self.is_empty() {
            return *self;
        }
        let rounded = dist.round() as i32;
        let lower_left = IntPoint::new(self.ll.x - rounded, self.ll.y);
        let upper_right = IntPoint::new(self.ur.x + rounded, self.ur.y);
        IntBox::new_from_points(lower_left, upper_right)
    }

    /// Vertical offset only.
    pub fn vertical_offset(&self, dist: f64) -> IntBox {
        if dist == 0.0 || self.is_empty() {
            return *self;
        }
        let rounded = dist.round() as i32;
        let lower_left = IntPoint::new(self.ll.x, self.ll.y - rounded);
        let upper_right = IntPoint::new(self.ur.x, self.ur.y + rounded);
        IntBox::new_from_points(lower_left, upper_right)
    }

    /// Shrinks width and height by `width`. The box will not vanish completely.
    pub fn shrink(&self, width: i32) -> IntBox {
        let (ll_x, ur_x) = if 2 * width <= self.ur.x - self.ll.x {
            (self.ll.x + width, self.ur.x - width)
        } else {
            let mid = (self.ll.x + self.ur.x) / 2;
            (mid, mid)
        };
        let (ll_y, ur_y) = if 2 * width <= self.ur.y - self.ll.y {
            (self.ll.y + width, self.ur.y - width)
        } else {
            let mid = (self.ll.y + self.ur.y) / 2;
            (mid, mid)
        };
        IntBox::new(ll_x, ll_y, ur_x, ur_y)
    }

    /// Compares the `edge_no`-th edge line with `other`.
    pub fn compare(&self, other: &IntBox, edge_no: usize) -> Side {
        match edge_no {
            0 => {
                if self.ll.y > other.ll.y {
                    Side::OnTheLeft
                } else if self.ll.y < other.ll.y {
                    Side::OnTheRight
                } else {
                    Side::Collinear
                }
            }
            1 => {
                if self.ur.x < other.ur.x {
                    Side::OnTheLeft
                } else if self.ur.x > other.ur.x {
                    Side::OnTheRight
                } else {
                    Side::Collinear
                }
            }
            2 => {
                if self.ur.y < other.ur.y {
                    Side::OnTheLeft
                } else if self.ur.y > other.ur.y {
                    Side::OnTheRight
                } else {
                    Side::Collinear
                }
            }
            3 => {
                if self.ll.x > other.ll.x {
                    Side::OnTheLeft
                } else if self.ll.x < other.ll.x {
                    Side::OnTheRight
                } else {
                    Side::Collinear
                }
            }
            _ => panic!("IntBox.compare: edge_no out of range"),
        }
    }

    /// Returns true if this box is contained in `other`.
    pub fn is_contained_in(&self, other: &IntBox) -> bool {
        if self.is_empty() || self == other {
            return true;
        }
        self.ll.x >= other.ll.x
            && self.ll.y >= other.ll.y
            && self.ur.x <= other.ur.x
            && self.ur.y <= other.ur.y
    }

    /// Part of `from_box` with minimal distance to this box.
    pub fn nearest_part(&self, from_box: &IntBox) -> IntBox {
        let ll_x = if from_box.ll.x >= self.ll.x {
            from_box.ll.x
        } else {
            from_box.ur.x.min(self.ll.x)
        };

        let ur_x = if from_box.ur.x <= self.ur.x {
            from_box.ur.x
        } else {
            from_box.ll.x.max(self.ur.x)
        };

        let ll_y = if from_box.ll.y >= self.ll.y {
            from_box.ll.y
        } else {
            from_box.ur.y.min(self.ll.y)
        };

        let ur_y = if from_box.ur.y <= self.ur.y {
            from_box.ur.y
        } else {
            from_box.ll.y.max(self.ur.y)
        };

        IntBox::new(ll_x, ll_y, ur_x, ur_y)
    }

    /// Divides this box into sections of about equal size with width/height <= `max_section_width`.
    pub fn divide_into_sections(&self, max_section_width: f64) -> Vec<IntBox> {
        if max_section_width <= 0.0 {
            return Vec::new();
        }
        let length = (self.ur.x - self.ll.x) as f64;
        let height = (self.ur.y - self.ll.y) as f64;
        let xcount = (length / max_section_width).ceil() as usize;
        let ycount = (height / max_section_width).ceil() as usize;
        if xcount == 0 || ycount == 0 {
            return Vec::new();
        }
        let section_length_x = (length / xcount as f64).ceil() as i32;
        let section_length_y = (height / ycount as f64).ceil() as i32;
        let mut result = Vec::with_capacity(xcount * ycount);

        for j in 0..ycount {
            let curr_lly = self.ll.y + (j as i32) * section_length_y;
            let curr_ury = if j == ycount - 1 {
                self.ur.y
            } else {
                curr_lly + section_length_y
            };
            for i in 0..xcount {
                let curr_llx = self.ll.x + (i as i32) * section_length_x;
                let curr_urx = if i == xcount - 1 {
                    self.ur.x
                } else {
                    curr_llx + section_length_x
                };
                result.push(IntBox::new(curr_llx, curr_lly, curr_urx, curr_ury));
            }
        }
        result
    }

    /// Converts this box to a 4-line Simplex.
    pub fn to_simplex(&self) -> Simplex {
        if self.is_empty() {
            return Simplex::EMPTY;
        }
        let lines = [
            Line::new_from_point_and_dir(self.ll, &Direction::RIGHT),
            Line::new_from_point_and_dir(self.ur, &Direction::UP),
            Line::new_from_point_and_dir(self.ur, &Direction::LEFT),
            Line::new_from_point_and_dir(self.ll, &Direction::DOWN),
        ];
        Simplex::new(lines.to_vec())
    }

    /// Cuts this box out of `outer`, returning up to 4 rectangular IntBoxes.
    pub fn cutout_from_box(&self, outer: &IntBox) -> Vec<IntBox> {
        let c = self.intersection(outer);
        if self.is_empty() || c.dimension() < 2 {
            return vec![*outer];
        }

        let mut result = [
            IntBox::new(outer.ll.x, outer.ll.y, c.ur.x, c.ll.y),
            IntBox::new(outer.ll.x, c.ll.y, c.ll.x, outer.ur.y),
            IntBox::new(c.ur.x, outer.ll.y, outer.ur.x, c.ur.y),
            IntBox::new(c.ll.x, c.ur.y, outer.ur.x, outer.ur.y),
        ];

        // Optimize division so cumulative circumference is minimal
        if c.ll.x - outer.ll.x > c.ll.y - outer.ll.y {
            let b0 = result[0];
            result[0] = IntBox::new(c.ll.x, b0.ll.y, b0.ur.x, b0.ur.y);
            let b1 = result[1];
            result[1] = IntBox::new(b1.ll.x, outer.ll.y, b1.ur.x, b1.ur.y);
        }
        if outer.ur.y - c.ur.y > c.ll.x - outer.ll.x {
            let b1 = result[1];
            result[1] = IntBox::new(b1.ll.x, b1.ll.y, b1.ur.x, c.ur.y);
            let b3 = result[3];
            result[3] = IntBox::new(outer.ll.x, b3.ll.y, b3.ur.x, b3.ur.y);
        }
        if outer.ur.x - c.ur.x > outer.ur.y - c.ur.y {
            let b2 = result[2];
            result[2] = IntBox::new(b2.ll.x, b2.ll.y, b2.ur.x, outer.ur.y);
            let b3 = result[3];
            result[3] = IntBox::new(b3.ll.x, b3.ll.y, c.ur.x, b3.ur.y);
        }
        if c.ll.y - outer.ll.y > outer.ur.x - c.ur.x {
            let b0 = result[0];
            result[0] = IntBox::new(b0.ll.x, b0.ll.y, outer.ur.x, b0.ur.y);
            let b2 = result[2];
            result[2] = IntBox::new(b2.ll.x, c.ll.y, b2.ur.x, b2.ur.y);
        }

        result.iter().filter(|b| b.dimension() == 2).copied().collect()
    }

    /// Cuts `inner` out of this box.
    pub fn cutout(&self, inner: &IntBox) -> Vec<IntBox> {
        inner.cutout_from_box(self)
    }

    /// Smallest radius from centre of gravity to border.
    pub fn smallest_radius(&self) -> f64 {
        0.5 * (self.width().min(self.height()) as f64)
    }

    pub fn left_x(&self) -> i32 {
        self.ll.x
    }

    pub fn right_x(&self) -> i32 {
        self.ur.x
    }

    pub fn bottom_y(&self) -> i32 {
        self.ll.y
    }

    pub fn top_y(&self) -> i32 {
        self.ur.y
    }

    /// Returns true if this box completely contains `other`.
    pub fn contains_box(&self, other: &IntBox) -> bool {
        self.ll.x <= other.ll.x
            && self.ur.x >= other.ur.x
            && self.ll.y <= other.ll.y
            && self.ur.y >= other.ur.y
    }
}

impl Area for IntBox {
    fn is_empty(&self) -> bool {
        self.is_empty()
    }

    fn is_bounded(&self) -> bool {
        true
    }

    fn dimension(&self) -> i32 {
        self.dimension()
    }

    fn is_contained_in(&self, box_: &IntBox) -> bool {
        box_.contains_box(self)
    }

    fn bounding_box(&self) -> IntBox {
        *self
    }

    fn bounding_octagon(&self) -> IntOctagon {
        self.to_int_octagon()
    }

    fn contains_point(&self, point: &Point) -> bool {
        match point {
            Point::Int(ip) => {
                ip.x >= self.ll.x && ip.x <= self.ur.x && ip.y >= self.ll.y && ip.y <= self.ur.y
            }
            Point::Rational(rp) => {
                let fp = rp.to_float();
                fp.x >= self.ll.x as f64
                    && fp.x <= self.ur.x as f64
                    && fp.y >= self.ll.y as f64
                    && fp.y <= self.ur.y as f64
            }
        }
    }

    fn contains_float_point(&self, point: &FloatPoint) -> bool {
        point.x >= self.ll.x as f64
            && point.x <= self.ur.x as f64
            && point.y >= self.ll.y as f64
            && point.y <= self.ur.y as f64
    }

    fn nearest_point_approx(&self, from_point: &FloatPoint) -> FloatPoint {
        FloatPoint::new(
            from_point.x.clamp(self.ll.x as f64, self.ur.x as f64),
            from_point.y.clamp(self.ll.y as f64, self.ur.y as f64),
        )
    }

    fn corner_approx_arr(&self) -> Vec<FloatPoint> {
        vec![
            FloatPoint::new(self.ll.x as f64, self.ll.y as f64),
            FloatPoint::new(self.ur.x as f64, self.ll.y as f64),
            FloatPoint::new(self.ur.x as f64, self.ur.y as f64),
            FloatPoint::new(self.ll.x as f64, self.ur.y as f64),
        ]
    }
}

impl Shape for IntBox {
    fn circumference(&self) -> f64 {
        self.circumference()
    }

    fn area(&self) -> f64 {
        self.area()
    }

    fn centre_of_gravity(&self) -> FloatPoint {
        FloatPoint::new(
            (self.ll.x + self.ur.x) as f64 * 0.5,
            (self.ll.y + self.ur.y) as f64 * 0.5,
        )
    }

    fn is_outside(&self, point: &Point) -> bool {
        !self.contains_point(point)
    }

    fn contains_inside(&self, point: &Point) -> bool {
        match point {
            Point::Int(ip) => self.contains_inside(ip),
            Point::Rational(rp) => {
                let fp = rp.to_float();
                fp.x > self.ll.x as f64
                    && fp.x < self.ur.x as f64
                    && fp.y > self.ll.y as f64
                    && fp.y < self.ur.y as f64
            }
        }
    }

    fn contains_on_border(&self, point: &Point) -> bool {
        self.contains_point(point) && !<Self as Shape>::contains_inside(self, point)
    }

    fn distance(&self, point: &FloatPoint) -> f64 {
        self.nearest_point_approx(point).distance(point)
    }

    fn border_distance(&self, point: &FloatPoint) -> f64 {
        self.distance(point)
    }

    fn smallest_radius(&self) -> f64 {
        (self.width().min(self.height()) as f64) * 0.5
    }
}

impl ConvexShape for IntBox {
    fn max_width(&self) -> f64 {
        ((self.width() as f64).powi(2) + (self.height() as f64).powi(2)).sqrt()
    }

    fn min_width(&self) -> f64 {
        self.width().min(self.height()) as f64
    }
}
