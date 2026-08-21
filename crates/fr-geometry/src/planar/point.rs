//! Abstract point enum, ported from `app.freerouting.geometry.planar.Point`.

use num_bigint::{BigInt, Sign};
use num_traits::{Signed, Zero};

use crate::planar::{
    direction::Direction, float_point::FloatPoint, int_box::IntBox, int_octagon::IntOctagon,
    int_point::IntPoint, limits::Limits, line::Line, rational_point::RationalPoint, side::Side,
    vector::Vector,
};

/// An abstract point in the plane, either an integer point (fast path) or an
/// exact rational point.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Point {
    Int(IntPoint),
    Rational(RationalPoint),
}

impl Point {
    /// Standard zero point.
    pub const ZERO: Point = Point::Int(IntPoint::ZERO);

    /// Factory method: returns `Point::Int` if within `CRIT_INT`, else `Point::Rational`.
    pub fn get_instance(x: i32, y: i32) -> Self {
        let ip = IntPoint::new(x, y);
        if x.abs() > Limits::CRIT_INT || y.abs() > Limits::CRIT_INT {
            Point::Rational(RationalPoint::from_int_point(&ip))
        } else {
            Point::Int(ip)
        }
    }

    /// Factory method for creating a `Point` from 3 `BigInt`s (x / z, y / z).
    pub fn get_instance_bigint(mut x: BigInt, mut y: BigInt, mut z: BigInt) -> Self {
        if z.sign() == Sign::Minus {
            x = -x;
            y = -y;
            z = -z;
        }
        if (&x % &z).is_zero() && (&y % &z).is_zero() {
            x = &x / &z;
            y = &y / &z;
            z = BigInt::from(1);
        }
        if z == BigInt::from(1)
            && x.abs() <= Limits::crit_int_big()
            && y.abs() <= Limits::crit_int_big()
        {
            let xi = i32::try_from(&x).expect("x fits i32");
            let yi = i32::try_from(&y).expect("y fits i32");
            return Point::Int(IntPoint::new(xi, yi));
        }
        Point::Rational(RationalPoint::new(x, y, z))
    }

    /// Returns true if this is a point at infinity.
    pub fn is_infinite(&self) -> bool {
        match self {
            Point::Int(_) => false,
            Point::Rational(rp) => rp.is_infinite(),
        }
    }

    /// Smallest `IntBox` containing this point.
    pub fn surrounding_box(&self) -> IntBox {
        match self {
            Point::Int(ip) => ip.surrounding_box(),
            Point::Rational(rp) => rp.surrounding_box(),
        }
    }

    /// Smallest `IntOctagon` containing this point.
    pub fn surrounding_octagon(&self) -> IntOctagon {
        match self {
            Point::Int(ip) => ip.surrounding_octagon(),
            Point::Rational(rp) => rp.surrounding_octagon(),
        }
    }

    /// Returns true if this point is in the interior or border of `box_`.
    pub fn is_contained_in(&self, box_: &IntBox) -> bool {
        match self {
            Point::Int(ip) => ip.is_contained_in(box_),
            Point::Rational(rp) => rp.is_contained_in(box_),
        }
    }

    /// Translates this point by `vector`.
    pub fn translate_by(&self, vector: &Vector) -> Point {
        match self {
            Point::Int(ip) => ip.translate_by(vector),
            Point::Rational(rp) => rp.translate_by(vector),
        }
    }

    /// Difference vector `self - other`.
    pub fn difference_by(&self, other: &Point) -> Vector {
        match (self, other) {
            (Point::Int(a), Point::Int(b)) => Vector::Int(a.difference_by_int(b)),
            (Point::Int(a), Point::Rational(b)) => {
                let ra = RationalPoint::from_int_point(a);
                Vector::Rational(ra.difference_by_rational(b))
            }
            (Point::Rational(a), Point::Int(b)) => {
                let rb = RationalPoint::from_int_point(b);
                Vector::Rational(a.difference_by_rational(&rb))
            }
            (Point::Rational(a), Point::Rational(b)) => Vector::Rational(a.difference_by_rational(b)),
        }
    }

    /// Approximates coordinates by float coordinates.
    pub fn to_float(&self) -> FloatPoint {
        match self {
            Point::Int(ip) => ip.to_float(),
            Point::Rational(rp) => rp.to_float(),
        }
    }

    /// Returns a unique ID for this point for deterministic tie-breaking.
    pub fn get_id_no(&self) -> i32 {
        match self {
            Point::Int(ip) => ip.get_id_no(),
            Point::Rational(rp) => rp.get_id_no(),
        }
    }

    /// Side of `line`.
    pub fn side_of(&self, line: &Line) -> Side {
        match self {
            Point::Int(ip) => ip.side_of(line),
            Point::Rational(rp) => rp.side_of(line),
        }
    }

    /// Side of line through `p1` and `p2`.
    pub fn side_of_points(&self, p1: &Point, p2: &Point) -> Side {
        let v1 = self.difference_by(p1);
        let v2 = p2.difference_by(p1);
        v1.side_of(&v2)
    }

    /// Perpendicular projection onto `line`.
    pub fn perpendicular_projection(&self, line: &Line) -> Point {
        match self {
            Point::Int(ip) => ip.perpendicular_projection(line),
            Point::Rational(rp) => rp.perpendicular_projection(line),
        }
    }

    /// Perpendicular direction to `line`. Returns `Direction::NULL` if on line.
    pub fn perpendicular_direction(&self, line: &Line) -> Direction {
        let side = self.side_of(line);
        if side == Side::Collinear {
            return Direction::NULL;
        }
        if side == Side::OnTheRight {
            line.direction().turn_45_degree(2)
        } else {
            line.direction().turn_45_degree(6)
        }
    }

    /// Compares x coordinate with `other`.
    pub fn compare_x(&self, other: &Point) -> i32 {
        match (self, other) {
            (Point::Int(a), Point::Int(b)) => a.compare_x(b),
            (Point::Int(a), Point::Rational(b)) => -b.compare_x_int(a),
            (Point::Rational(a), Point::Int(b)) => a.compare_x_int(b),
            (Point::Rational(a), Point::Rational(b)) => a.compare_x_rational(b),
        }
    }

    /// Compares y coordinate with `other`.
    pub fn compare_y(&self, other: &Point) -> i32 {
        match (self, other) {
            (Point::Int(a), Point::Int(b)) => a.compare_y(b),
            (Point::Int(a), Point::Rational(b)) => -b.compare_y_int(a),
            (Point::Rational(a), Point::Int(b)) => a.compare_y_int(b),
            (Point::Rational(a), Point::Rational(b)) => a.compare_y_rational(b),
        }
    }

    /// Compares x then y.
    pub fn compare_xy(&self, other: &Point) -> i32 {
        let r = self.compare_x(other);
        if r == 0 {
            self.compare_y(other)
        } else {
            r
        }
    }

    /// Turns this point by `factor` times 90° around `pole`.
    pub fn turn_90_degree(&self, factor: i32, pole: &Point) -> Point {
        let v = self.difference_by(pole);
        let turned = v.turn_90_degree(factor);
        pole.translate_by(&turned)
    }

    /// Mirrors this point at the vertical line through `pole`.
    pub fn mirror_vertical(&self, pole: &Point) -> Point {
        let v = self.difference_by(pole);
        let mirrored = v.mirror_at_y_axis();
        pole.translate_by(&mirrored)
    }

    /// Mirrors this point at the horizontal line through `pole`.
    pub fn mirror_horizontal(&self, pole: &Point) -> Point {
        let v = self.difference_by(pole);
        let mirrored = v.mirror_at_x_axis();
        pole.translate_by(&mirrored)
    }
}
