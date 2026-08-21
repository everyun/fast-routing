//! Rational point, ported from `app.freerouting.geometry.planar.RationalPoint`.
//!
//! Represents points in the projective plane via 3 coordinates `(x, y, z)` where
//! `z >= 0`. The affine point with rational coordinates is `(x / z, y / z)`.

use num_bigint::{BigInt, Sign};
use num_traits::{Signed, ToPrimitive, Zero};

use crate::planar::{
    float_point::FloatPoint, int_box::IntBox, int_octagon::IntOctagon, int_point::IntPoint,
    limits::Limits, line::Line, point::Point, rational_vector::RationalVector, side::Side,
    vector::Vector,
};
use fr_datastructures::BigIntAux;

/// A point in the projective plane with integer coordinates `(x, y, z)` and `z >= 0`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RationalPoint {
    pub x: BigInt,
    pub y: BigInt,
    pub z: BigInt,
}

impl RationalPoint {
    /// Creates a `RationalPoint` from 3 `BigInt`s. Panics if `z < 0`.
    pub fn new(x: BigInt, y: BigInt, z: BigInt) -> Self {
        assert!(z.sign() != Sign::Minus, "RationalPoint: z must be >= 0");
        RationalPoint { x, y, z }
    }

    /// Creates a `RationalPoint` from an `IntPoint`.
    pub fn from_int_point(point: &IntPoint) -> Self {
        RationalPoint {
            x: BigInt::from(point.x),
            y: BigInt::from(point.y),
            z: BigInt::from(1),
        }
    }

    /// Approximates coordinates by float coordinates.
    pub fn to_float(&self) -> FloatPoint {
        let zd = self.z.to_f64().unwrap_or(f64::MAX);
        if zd == 0.0 {
            FloatPoint::new(f32::MAX as f64, f32::MAX as f64)
        } else {
            let xd = self.x.to_f64().unwrap_or(f64::MAX);
            let yd = self.y.to_f64().unwrap_or(f64::MAX);
            FloatPoint::new(xd / zd, yd / zd)
        }
    }

    /// Stable identifier for deterministic tie-breaking.
    pub fn get_id_no(&self) -> i32 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish() as i32
    }

    /// Returns true if this is a point at infinity (`z == 0`).
    pub fn is_infinite(&self) -> bool {
        self.z.is_zero()
    }

    /// Smallest `IntBox` containing this point.
    pub fn surrounding_box(&self) -> IntBox {
        let fp = self.to_float();
        IntBox::new(
            fp.x.floor() as i32,
            fp.y.floor() as i32,
            fp.x.ceil() as i32,
            fp.y.ceil() as i32,
        )
    }

    /// Smallest `IntOctagon` containing this point.
    pub fn surrounding_octagon(&self) -> IntOctagon {
        let fp = self.to_float();
        let lx = fp.x.floor() as i32;
        let ly = fp.y.floor() as i32;
        let rx = fp.x.ceil() as i32;
        let uy = fp.y.ceil() as i32;

        let tmp = fp.x - fp.y;
        let ulx = tmp.floor() as i32;
        let lrx = tmp.ceil() as i32;

        let tmp = fp.x + fp.y;
        let llx = tmp.floor() as i32;
        let urx = tmp.ceil() as i32;
        IntOctagon::new(lx, ly, rx, uy, ulx, lrx, llx, urx)
    }

    /// Returns true if this point is in the interior or on the border of `box_`.
    pub fn is_contained_in(&self, box_: &IntBox) -> bool {
        let tmp = BigInt::from(box_.ll.x) * &self.z;
        if self.x < tmp {
            return false;
        }
        let tmp = BigInt::from(box_.ll.y) * &self.z;
        if self.y < tmp {
            return false;
        }
        let tmp = BigInt::from(box_.ur.x) * &self.z;
        if self.x > tmp {
            return false;
        }
        let tmp = BigInt::from(box_.ur.y) * &self.z;
        self.y <= tmp
    }

    /// Translates this point by `vector`.
    pub fn translate_by(&self, vector: &Vector) -> Point {
        if vector.is_zero() {
            Point::Rational(self.clone())
        } else {
            vector.add_to_rational_point(self)
        }
    }

    /// Difference vector `self - other`.
    pub fn difference_by_rational(&self, other: &RationalPoint) -> RationalVector {
        let (x, y, z) = BigIntAux::add_rational_coordinates(
            (&self.x, &self.y, &self.z),
            (&(-other.x.clone()), &(-other.y.clone()), &other.z),
        );
        RationalVector::new(x, y, z)
    }

    /// Side of line through `p1` and `p2`.
    pub fn side_of_points(&self, p1: &Point, p2: &Point) -> Side {
        let v1 = match p1 {
            Point::Int(ip) => {
                let rp = RationalPoint::from_int_point(ip);
                Vector::Rational(self.difference_by_rational(&rp))
            }
            Point::Rational(rp) => Vector::Rational(self.difference_by_rational(rp)),
        };
        let v2 = p2.difference_by(p1);
        v1.side_of(&v2)
    }

    /// Side of `line`.
    pub fn side_of(&self, line: &Line) -> Side {
        self.side_of_points(&Point::Int(line.a_int()), &Point::Int(line.b_int()))
    }

    /// Perpendicular projection onto `line`.
    pub fn perpendicular_projection(&self, line: &Line) -> Point {
        let v = line.b_int().difference_by_int(&line.a_int());
        let vxvx = BigInt::from((v.x as i64) * (v.x as i64));
        let vyvy = BigInt::from((v.y as i64) * (v.y as i64));
        let vxvy = BigInt::from((v.x as i64) * (v.y as i64));
        let mut denominator = &vxvx + &vyvy;
        let det = BigInt::from(line.a_int().determinant(&line.b_int()));

        let tmp1 = &vxvx * &self.x;
        let tmp2 = &vxvy * &self.y;
        let tmp1 = tmp1 + tmp2;
        let tmp2 = &det * BigInt::from(v.y) * &self.z;
        let mut proj_x = tmp1 + tmp2;

        let tmp1 = &vxvy * &self.x;
        let tmp2 = &vyvy * &self.y;
        let tmp1 = tmp1 + tmp2;
        let tmp2 = &det * BigInt::from(v.x) * &self.z;
        let mut proj_y = tmp1 - tmp2;

        if !denominator.is_zero() {
            if denominator.sign() == Sign::Minus {
                denominator = -denominator;
                proj_x = -proj_x;
                proj_y = -proj_y;
            }
            if (&proj_x % &denominator).is_zero() && (&proj_y % &denominator).is_zero() {
                proj_x = &proj_x / &denominator;
                proj_y = &proj_y / &denominator;
                if proj_x.abs() <= Limits::crit_int_big() && proj_y.abs() <= Limits::crit_int_big()
                {
                    let xi = i32::try_from(&proj_x).expect("proj_x fits i32");
                    let yi = i32::try_from(&proj_y).expect("proj_y fits i32");
                    return Point::Int(IntPoint::new(xi, yi));
                }
                denominator = BigInt::from(1);
            }
        }
        Point::Rational(RationalPoint::new(proj_x, proj_y, denominator))
    }

    /// Compares x coordinate with `other` rational point.
    pub fn compare_x_rational(&self, other: &RationalPoint) -> i32 {
        let tmp1 = &self.x * &other.z;
        let tmp2 = &other.x * &self.z;
        tmp1.cmp(&tmp2) as i32
    }

    /// Compares x coordinate with `other` int point.
    pub fn compare_x_int(&self, other: &IntPoint) -> i32 {
        let tmp1 = &self.z * other.x;
        self.x.cmp(&tmp1) as i32
    }

    /// Compares y coordinate with `other` rational point.
    pub fn compare_y_rational(&self, other: &RationalPoint) -> i32 {
        let tmp1 = &self.y * &other.z;
        let tmp2 = &other.y * &self.z;
        tmp1.cmp(&tmp2) as i32
    }

    /// Compares y coordinate with `other` int point.
    pub fn compare_y_int(&self, other: &IntPoint) -> i32 {
        let tmp1 = &self.z * other.y;
        self.y.cmp(&tmp1) as i32
    }
}
