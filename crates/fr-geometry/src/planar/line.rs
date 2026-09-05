//! Directed line in the plane, ported from `app.freerouting.geometry.planar.Line`.

use num_bigint::{BigInt, Sign};
use num_traits::{Signed, Zero};

use crate::planar::{
    direction::Direction,
    float_point::FloatPoint,
    int_point::IntPoint,
    limits::{determinant_exact, side_of_exact, Limits},
    point::Point,
    rational_point::RationalPoint,
    side::Side,
    vector::Vector,
};
use crate::util::jround_i32;
use fr_datastructures::Signum;

/// A directed line in the plane defined by two points `a` and `b`.
///
/// In 99.9% of PCB routing applications, `a` and `b` are `IntPoint`s. The
/// fields are stored directly as `IntPoint` for performance.
///
/// # Invariant
///
/// Both defining points obey the [`IntPoint`] coordinate envelope
/// (`|coord| <= Limits::CRIT_INT`); [`Line::try_new`] is the checked
/// constructor for the IO boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Line {
    pub a: IntPoint,
    pub b: IntPoint,
}

impl Line {
    /// Creates a directed line from two points.
    pub const fn new(a: IntPoint, b: IntPoint) -> Self {
        Line { a, b }
    }

    /// Creates a directed line from four integer coordinates.
    pub const fn new_from_coords(ax: i32, ay: i32, bx: i32, by: i32) -> Self {
        Line {
            a: IntPoint::new(ax, ay),
            b: IntPoint::new(bx, by),
        }
    }

    /// Checked constructor for the IO boundary: returns `None` unless both
    /// points are inside the `CRIT_INT` envelope.
    #[inline]
    pub const fn try_new(a: IntPoint, b: IntPoint) -> Option<Self> {
        if a.is_in_envelope() && b.is_in_envelope() {
            Some(Line { a, b })
        } else {
            None
        }
    }

    /// Checked constructor from four integer coordinates; see [`Line::try_new`].
    #[inline]
    pub const fn try_new_from_coords(ax: i32, ay: i32, bx: i32, by: i32) -> Option<Self> {
        match (IntPoint::try_new(ax, ay), IntPoint::try_new(bx, by)) {
            (Some(a), Some(b)) => Some(Line { a, b }),
            _ => None,
        }
    }

    /// Creates a directed line from a point and a direction.
    pub fn new_from_point_and_dir(a: IntPoint, dir: &Direction) -> Self {
        let b = a.translate_by_int(&dir.get_int_vector());
        Line { a, b }
    }

    /// Accessor for `a` as an `IntPoint`.
    pub fn a_int(&self) -> IntPoint {
        self.a
    }

    /// Accessor for `b` as an `IntPoint`.
    pub fn b_int(&self) -> IntPoint {
        self.b
    }

    /// Stable identifier.
    pub fn get_id_no(&self) -> i32 {
        31 * self.a.get_id_no() + self.b.get_id_no()
    }

    /// Gets the normalized direction of this directed line.
    pub fn direction(&self) -> Direction {
        let d = self.b.difference_by_int(&self.a);
        d.to_normalized_direction()
    }

    /// Side of `point`: `OnTheLeft` if the line is on the left of `point`,
    /// `OnTheRight` if on the right, `Collinear` if on the line.
    pub fn side_of(&self, point: &IntPoint) -> Side {
        point.side_of(self).negate()
    }

    /// Side of float point with tolerance.
    pub fn side_of_float_tol(&self, point: &FloatPoint, tolerance: f64) -> Side {
        let det = ((self.b.y - self.a.y) as f64) * (point.x - self.a.x as f64)
            - ((self.b.x - self.a.x) as f64) * (point.y - self.a.y as f64);
        if det - tolerance > 0.0 {
            Side::OnTheLeft
        } else if det + tolerance < 0.0 {
            Side::OnTheRight
        } else {
            Side::Collinear
        }
    }

    /// Side of float point without tolerance.
    pub fn side_of_float(&self, point: &FloatPoint) -> Side {
        self.side_of_float_tol(point, 0.0)
    }

    /// Signed distance of this line from `point` (positive = left).
    pub fn signed_distance(&self, point: &FloatPoint) -> f64 {
        let dx = (self.b.x - self.a.x) as f64;
        let dy = (self.b.y - self.a.y) as f64;
        let det = dy * (point.x - self.a.x as f64) - dx * (point.y - self.a.y as f64);
        let length = (dx * dx + dy * dy).sqrt();
        det / length
    }

    /// Returns true if two lines define the same set of points (may have opposite directions).
    pub fn overlaps(&self, other: &Line) -> bool {
        self.side_of(&other.a) == Side::Collinear && self.side_of(&other.b) == Side::Collinear
    }

    /// Line with swapped direction.
    pub fn opposite(&self) -> Line {
        Line::new(self.b, self.a)
    }

    /// Exact intersection point of two lines.
    pub fn intersection(&self, other: &Line) -> Point {
        let delta1 = self.b.difference_by_int(&self.a);
        let delta2 = other.b.difference_by_int(&other.a);

        // Fast paths for orthogonal and 45-degree lines
        if delta1.x == 0 {
            // this line is vertical
            if delta2.y == 0 {
                return Point::Int(IntPoint::new(self.a.x, other.a.y));
            }
            if delta2.x == delta2.y {
                let this_x = self.a.x;
                return Point::Int(IntPoint::new(this_x, other.a.y + this_x - other.a.x));
            }
            if delta2.x == -delta2.y {
                let this_x = self.a.x;
                return Point::Int(IntPoint::new(this_x, other.a.y + other.a.x - this_x));
            }
        } else if delta1.y == 0 {
            // this line is horizontal
            if delta2.x == 0 {
                return Point::Int(IntPoint::new(other.a.x, self.a.y));
            }
            if delta2.x == delta2.y {
                let this_y = self.a.y;
                return Point::Int(IntPoint::new(other.a.x + this_y - other.a.y, this_y));
            }
            if delta2.x == -delta2.y {
                let this_y = self.a.y;
                return Point::Int(IntPoint::new(other.a.x + other.a.y - this_y, this_y));
            }
        } else if delta1.x == delta1.y {
            // this line is right diagonal (+45°)
            if delta2.x == 0 {
                let other_x = other.a.x;
                return Point::Int(IntPoint::new(other_x, self.a.y + other_x - self.a.x));
            }
            if delta2.y == 0 {
                let other_y = other.a.y;
                return Point::Int(IntPoint::new(self.a.x + other_y - self.a.y, other_y));
            }
        } else if delta1.x == -delta1.y {
            // this line is left diagonal (-45°)
            if delta2.x == 0 {
                let other_x = other.a.x;
                return Point::Int(IntPoint::new(other_x, self.a.y + self.a.x - other_x));
            }
            if delta2.y == 0 {
                let other_y = other.a.y;
                return Point::Int(IntPoint::new(self.a.x + self.a.y - other_y, other_y));
            }
        }

        // General arbitrary-angle intersection via BigInteger
        let det1 = BigInt::from(self.a.determinant(&self.b));
        let det2 = BigInt::from(other.a.determinant(&other.b));
        let mut det = BigInt::from(delta2.determinant(&delta1));
        let tmp1 = &det1 * BigInt::from(delta2.x);
        let tmp2 = &det2 * BigInt::from(delta1.x);
        let mut is_x = tmp1 - tmp2;
        let tmp1 = &det1 * BigInt::from(delta2.y);
        let tmp2 = &det2 * BigInt::from(delta1.y);
        let mut is_y = tmp1 - tmp2;

        if !det.is_zero() {
            if det.sign() == Sign::Minus {
                det = -det;
                is_x = -is_x;
                is_y = -is_y;
            }
            if (&is_x % &det).is_zero() && (&is_y % &det).is_zero() {
                is_x = &is_x / &det;
                is_y = &is_y / &det;
                if is_x.abs() <= Limits::crit_int_big() && is_y.abs() <= Limits::crit_int_big() {
                    let xi = i32::try_from(&is_x).expect("is_x fits i32");
                    let yi = i32::try_from(&is_y).expect("is_y fits i32");
                    return Point::Int(IntPoint::new(xi, yi));
                }
                det = BigInt::from(1);
            }
        }
        Point::Rational(RationalPoint::new(is_x, is_y, det))
    }

    /// Approximate intersection as `FloatPoint`.
    pub fn intersection_approx(&self, other: &Line) -> FloatPoint {
        let d1x = (self.b.x - self.a.x) as f64;
        let d1y = (self.b.y - self.a.y) as f64;
        let d2x = (other.b.x - other.a.x) as f64;
        let d2y = (other.b.y - other.a.y) as f64;
        let det1 = (self.a.x as f64) * (self.b.y as f64) - (self.a.y as f64) * (self.b.x as f64);
        let det2 =
            (other.a.x as f64) * (other.b.y as f64) - (other.a.y as f64) * (other.b.x as f64);
        let det = d2x * d1y - d2y * d1x;
        if det == 0.0 {
            FloatPoint::new(i32::MAX as f64, i32::MAX as f64)
        } else {
            let is_x = (d2x * det1 - d1x * det2) / det;
            let is_y = (d2y * det1 - d1y * det2) / det;
            FloatPoint::new(is_x, is_y)
        }
    }

    /// Perpendicular translation of the line by `dist` (positive = left).
    pub fn translate(&self, dist: f64) -> Line {
        let v = self.direction().get_int_vector();
        let vxvx = (v.x as f64) * (v.x as f64);
        let vyvy = (v.y as f64) * (v.y as f64);
        let length = (vxvx + vyvy).sqrt();
        let new_a = if vxvx <= vyvy {
            let rel_x = jround_i32((dist * length) / v.y as f64);
            IntPoint::new(self.a.x - rel_x, self.a.y)
        } else {
            let rel_y = jround_i32((dist * length) / v.x as f64);
            IntPoint::new(self.a.x, self.a.y + rel_y)
        };
        Line::new_from_point_and_dir(new_a, &self.direction())
    }

    /// Translates this line by `vector`.
    pub fn translate_by(&self, vector: &Vector) -> Line {
        if vector.is_zero() {
            return *self;
        }
        let (vx, vy) = match vector {
            Vector::Int(iv) => (iv.x, iv.y),
            Vector::Rational(_) => panic!("Line translateBy only implemented for integer vectors"),
        };
        Line::new_from_coords(self.a.x + vx, self.a.y + vy, self.b.x + vx, self.b.y + vy)
    }

    /// Returns true if this line is axis-parallel.
    pub fn is_orthogonal(&self) -> bool {
        self.direction().is_orthogonal()
    }

    /// Returns true if this line is diagonal.
    pub fn is_diagonal(&self) -> bool {
        self.direction().is_diagonal()
    }

    /// Returns true if the direction is a multiple of 45°.
    pub fn is_multiple_of_45_degree(&self) -> bool {
        self.direction().is_multiple_of_45_degree()
    }

    /// Checks if this line and `other` are parallel.
    pub fn is_parallel(&self, other: &Line) -> bool {
        self.direction().side_of(&other.direction()) == Side::Collinear
    }

    /// Checks if this line and `other` are perpendicular.
    pub fn is_perpendicular(&self, other: &Line) -> bool {
        let v1 = self.direction().get_vector();
        let v2 = other.direction().get_vector();
        v1.projection(&v2) == Signum::Zero
    }

    /// Direction angular comparison (compareTo).
    ///
    /// This is an ordering predicate (`Ord for Line` is built on it), so the
    /// tie-breaking determinant is evaluated exactly: the deltas are widened to
    /// `i64` (their `i32` subtraction can overflow) and the determinant runs
    /// the `i64`/`i128` tier instead of an `f64` product that loses its sign
    /// above the double mantissa.
    pub fn cmp_line(&self, other: &Line) -> i32 {
        let dx1 = self.b.x as i64 - self.a.x as i64;
        let dy1 = self.b.y as i64 - self.a.y as i64;
        let dx2 = other.b.x as i64 - other.a.x as i64;
        let dy2 = other.b.y as i64 - other.a.y as i64;

        if dy1 > 0 {
            if dy2 < 0 {
                return -1;
            }
            if dy2 == 0 {
                if dx2 > 0 {
                    return 1;
                }
                return -1;
            }
        } else if dy1 < 0 {
            if dy2 >= 0 {
                return 1;
            }
        } else {
            // dy1 == 0
            if dx1 > 0 {
                if dy2 != 0 || dx2 < 0 {
                    return -1;
                }
                return 0;
            }
            // dx1 < 0
            if dy2 > 0 || (dy2 == 0 && dx2 > 0) {
                return 1;
            }
            if dy2 < 0 {
                return -1;
            }
            return 0;
        }

        determinant_exact(dx2, dy2, dx1, dy1).signum() as i32
    }

    /// Euclidean length of the segment `[a, b]`.
    pub fn length(&self) -> f32 {
        let dx = (self.b.x - self.a.x) as f32;
        let dy = (self.b.y - self.a.y) as f32;
        (dx * dx + dy * dy).sqrt()
    }

    /// Turns this line by `factor` times 90° around `pole`.
    pub fn turn_90_degree(&self, factor: i32, pole: &IntPoint) -> Line {
        let new_a = self.a.turn_90_degree(factor, pole);
        let new_b = self.b.turn_90_degree(factor, pole);
        Line::new(new_a, new_b)
    }

    /// Mirrors this line at the vertical line through `pole`.
    pub fn mirror_vertical(&self, pole: &IntPoint) -> Line {
        let new_a = self.b.mirror_vertical(pole);
        let new_b = self.a.mirror_vertical(pole);
        Line::new(new_a, new_b)
    }

    /// Mirrors this line at the horizontal line through `pole`.
    pub fn mirror_horizontal(&self, pole: &IntPoint) -> Line {
        let new_a = self.b.mirror_horizontal(pole);
        let new_b = self.a.mirror_horizontal(pole);
        Line::new(new_a, new_b)
    }

    /// Fast equality check.
    pub fn fast_equals(&self, other: &Line) -> bool {
        self == other
    }

    /// Returns true if this and `other` define the same directed line (Java `Line.equals`).
    pub fn is_same_line(&self, other: &Line) -> bool {
        self.direction() == other.direction() && self.side_of(&other.a) == Side::Collinear
    }

    /// Checks if this line and `other` define the same geometric line (same or opposite direction).
    ///
    /// `Line.java:441-444` answers this by collinearity, *not* by anchor
    /// identity: both defining points of `other` must lie on this line. Two
    /// lines that carry the same geometric line but are anchored at different
    /// points — a box's right border versus the abutting box's left border, the
    /// case `TileShape.java:645` depends on — therefore answer `true`.
    /// Evaluated exactly through [`Line::side_of`]; no float is involved.
    pub fn is_equal_or_opposite(&self, other: &Line) -> bool {
        self.side_of(&other.a) == Side::Collinear && self.side_of(&other.b) == Side::Collinear
    }

    /// Returns `Side::OnTheLeft` if this line is on the left of the intersection of `p1` and `p2`,
    /// `Side::OnTheRight` if on the right, and `Side::Collinear` if all 3 lines intersect in 1 point.
    ///
    /// Upstream answers this with a float pre-filter (tolerance 1.0) and only
    /// falls back to the exact `BigInteger` intersection when the filter says
    /// `Collinear`; the filter's error is *not* provably below its own
    /// tolerance, so the float can decide the side outright. Here the whole
    /// predicate is evaluated exactly in `i128` — no allocation, no float — and
    /// the upstream path is kept only for the two cases the exact formula
    /// cannot express: parallel `p1`/`p2` (upstream substitutes the pseudo
    /// point `(i32::MAX, i32::MAX)`) and coordinates so far outside the
    /// envelope that even `i128` overflows.
    pub fn side_of_intersection(&self, p1: &Line, p2: &Line) -> Side {
        if let Some(side) = self.side_of_intersection_exact(p1, p2) {
            return side;
        }
        let intersection_approx = p1.intersection_approx(p2);
        let result = self.side_of_float_tol(&intersection_approx, 1.0);
        if result == Side::Collinear {
            let intersection = p1.intersection(p2);
            self.side_of_point(&intersection)
        } else {
            result
        }
    }

    /// Exact `i128` evaluation of [`Line::side_of_intersection`].
    ///
    /// Writes the intersection of `p1` and `p2` as the homogeneous triple
    /// `(is_x, is_y, det)` with `det > 0` and evaluates this line's side of it
    /// as the sign of `vx * (is_y - a.y * det) - vy * (is_x - a.x * det)`,
    /// negated to match `Line::side_of`. Inside the envelope every intermediate
    /// stays below 2^105. Returns `None` when `p1` and `p2` are parallel or an
    /// intermediate leaves `i128`.
    fn side_of_intersection_exact(&self, p1: &Line, p2: &Line) -> Option<Side> {
        let d1x = i128::from(p1.b.x) - i128::from(p1.a.x);
        let d1y = i128::from(p1.b.y) - i128::from(p1.a.y);
        let d2x = i128::from(p2.b.x) - i128::from(p2.a.x);
        let d2y = i128::from(p2.b.y) - i128::from(p2.a.y);

        // Denominator of the intersection; zero means the lines are parallel.
        let mut det = d2x * d1y - d2y * d1x;
        if det == 0 {
            return None;
        }
        let det1 = i128::from(p1.a.determinant(&p1.b));
        let det2 = i128::from(p2.a.determinant(&p2.b));
        let mut is_x = det1.checked_mul(d2x)?.checked_sub(det2.checked_mul(d1x)?)?;
        let mut is_y = det1.checked_mul(d2y)?.checked_sub(det2.checked_mul(d1y)?)?;
        if det < 0 {
            det = -det;
            is_x = -is_x;
            is_y = -is_y;
        }

        let vx = i128::from(self.b.x) - i128::from(self.a.x);
        let vy = i128::from(self.b.y) - i128::from(self.a.y);
        let rel_x = is_x.checked_sub(i128::from(self.a.x).checked_mul(det)?)?;
        let rel_y = is_y.checked_sub(i128::from(self.a.y).checked_mul(det)?)?;
        let determinant = vx.checked_mul(rel_y)?.checked_sub(vy.checked_mul(rel_x)?)?;
        Some(side_of_exact(determinant).negate())
    }

    /// Side of arbitrary Point.
    pub fn side_of_point(&self, point: &Point) -> Side {
        match point {
            Point::Int(ip) => self.side_of(ip),
            Point::Rational(_) => point.side_of(self).negate(),
        }
    }

    /// Calculates approximation of the function value `y` of this line at `x`, if not vertical.
    pub fn function_value_approx(&self, x: f64) -> f64 {
        let p1 = self.a.to_float();
        let p2 = self.b.to_float();
        let dx = p2.x - p1.x;
        if dx == 0.0 {
            return 0.0;
        }
        let dy = p2.y - p1.y;
        let det = p1.x * p2.y - p2.x * p1.y;
        (dy * x - det) / dx
    }

    /// Calculates approximation of the function value `x` of this line at `y`, if not horizontal.
    pub fn function_in_y_value_approx(&self, y: f64) -> f64 {
        let p1 = self.a.to_float();
        let p2 = self.b.to_float();
        let dy = p2.y - p1.y;
        if dy == 0.0 {
            return 0.0;
        }
        let dx = p2.x - p1.x;
        let det = p1.x * p2.y - p2.x * p1.y;
        (dx * y + det) / dy
    }

    /// Returns the perpendicular direction from `from_point` towards this line.
    pub fn perpendicular_direction(&self, from_point: &Point) -> Option<Direction> {
        let line_side = self.side_of_point(from_point);
        if line_side == Side::Collinear {
            return None;
        }
        let dir1 = self.direction().turn_45_degree(2);
        let dir2 = self.direction().turn_45_degree(6);

        let check_point1 = from_point.translate_by(&dir1.get_vector());
        let side1 = self.side_of_point(&check_point1);
        if side1 != line_side {
            return Some(dir1);
        }
        let check_point2 = from_point.translate_by(&dir2.get_vector());
        let side2 = self.side_of_point(&check_point2);
        if side2 != line_side {
            return Some(dir2);
        }
        let nearest_line_point = from_point.to_float().projection_approx(self);
        if nearest_line_point.distance_square(&check_point1.to_float())
            <= nearest_line_point.distance_square(&check_point2.to_float())
        {
            Some(dir1)
        } else {
            Some(dir2)
        }
    }

    /// Perpendicular projection of `point` onto this line.
    pub fn perpendicular_projection(&self, point: &Point) -> Point {
        point.perpendicular_projection(self)
    }
}

impl PartialOrd for Line {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Line {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.cmp_line(other) {
            1 => std::cmp::Ordering::Greater,
            -1 => std::cmp::Ordering::Less,
            _ => std::cmp::Ordering::Equal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orthogonal_intersections() {
        // Vertical line x = 10, horizontal line y = 20
        let v = Line::new_from_coords(10, 0, 10, 100);
        let h = Line::new_from_coords(0, 20, 100, 20);
        assert_eq!(v.intersection(&h), Point::Int(IntPoint::new(10, 20)));
        assert_eq!(h.intersection(&v), Point::Int(IntPoint::new(10, 20)));
    }

    #[test]
    fn diagonal_intersections() {
        // Line y = x and horizontal line y = 15
        let d = Line::new_from_coords(0, 0, 10, 10);
        let h = Line::new_from_coords(0, 15, 100, 15);
        assert_eq!(d.intersection(&h), Point::Int(IntPoint::new(15, 15)));
    }

    #[test]
    fn equal_or_opposite_is_collinearity_not_anchor_identity() {
        // The right border of the box [0,0,100,100] and the left border of the
        // box [100,0,200,100]: the same geometric line x = 100, opposite
        // directions, anchored at different corners. `Line.java:441-444` must
        // answer true — `TileShape.touching_sides` depends on it.
        let right_border = Line::new_from_coords(100, 0, 100, 100);
        let abutting_left_border = Line::new_from_coords(100, 100, 100, 0);
        assert!(right_border.is_equal_or_opposite(&abutting_left_border));
        assert!(abutting_left_border.is_equal_or_opposite(&right_border));

        // Same line, same direction, different anchors and different lengths.
        let shifted = Line::new_from_coords(100, -50, 100, 7);
        assert!(right_border.is_equal_or_opposite(&shifted));

        // Anchor identity would have said false for both of the above.
        assert!(!right_border.fast_equals(&abutting_left_border));
        assert!(!right_border.fast_equals(&shifted));
    }

    #[test]
    fn equal_or_opposite_rejects_parallel_and_crossing_lines() {
        let line = Line::new_from_coords(100, 0, 100, 100);
        // Parallel but one unit away.
        assert!(!line.is_equal_or_opposite(&Line::new_from_coords(101, 0, 101, 100)));
        // Crossing: shares one point only.
        assert!(!line.is_equal_or_opposite(&Line::new_from_coords(0, 50, 200, 50)));
        // Shares the anchor `a` but not the direction.
        assert!(!line.is_equal_or_opposite(&Line::new_from_coords(100, 0, 200, 100)));
    }

    #[test]
    fn equal_or_opposite_is_exact_far_outside_the_float_envelope() {
        // A diagonal through the origin with a huge slope denominator: the
        // point (1_000_000_003, 999_999_999) is *not* on y = x, but a f64
        // determinant on coordinates of this size still resolves it. Assert
        // the exact predicate separates the two.
        let diagonal = Line::new_from_coords(0, 0, 1_000_000_000, 1_000_000_000);
        assert!(diagonal.is_equal_or_opposite(&Line::new_from_coords(7, 7, -3, -3)));
        assert!(!diagonal.is_equal_or_opposite(&Line::new_from_coords(
            1_000_000_003,
            999_999_999,
            2,
            2
        )));
    }

    #[test]
    fn perpendicularity() {
        let l1 = Line::new_from_coords(0, 0, 1, 0);
        let l2 = Line::new_from_coords(0, 0, 0, 1);
        assert!(l1.is_perpendicular(&l2));
        assert!(!l1.is_parallel(&l2));
    }
}
