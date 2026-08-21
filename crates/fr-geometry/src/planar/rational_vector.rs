//! Rational vector, ported from `app.freerouting.geometry.planar.RationalVector`.
//!
//! A `RationalVector` represents the vector `(x / z, y / z)` using arbitrary
//! precision integers.

use num_bigint::{BigInt, Sign};
use num_traits::{Signed, ToPrimitive, Zero};

use crate::planar::{
    direction::Direction, float_point::FloatPoint, int_vector::IntVector, limits::Limits,
    rational_point::RationalPoint, side::Side,
};
use fr_datastructures::{BigIntAux, Signum};

/// A 2-dimensional vector with rational coordinates `(x / z, y / z)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RationalVector {
    pub x: BigInt,
    pub y: BigInt,
    /// The denominator; always non-negative after construction.
    pub z: BigInt,
}

impl RationalVector {
    /// Creates a `RationalVector` from `x`, `y` and `z`, normalizing the sign so
    /// that the denominator is non-negative.
    pub fn new(x: BigInt, y: BigInt, z: BigInt) -> Self {
        if z.sign() != Sign::Minus {
            RationalVector { x, y, z }
        } else {
            RationalVector {
                x: -x,
                y: -y,
                z: -z,
            }
        }
    }

    /// Creates a `RationalVector` from an `IntVector`.
    pub fn from_int_vector(vector: &IntVector) -> Self {
        RationalVector {
            x: BigInt::from(vector.x),
            y: BigInt::from(vector.y),
            z: BigInt::from(1),
        }
    }

    /// Returns true if the x and y coordinates of this vector are 0.
    pub fn is_zero(&self) -> bool {
        self.x.is_zero() && self.y.is_zero()
    }

    /// Returns the vector such that `this + this.negate()` is zero.
    pub fn negate(&self) -> RationalVector {
        RationalVector::new(-self.x.clone(), -self.y.clone(), self.z.clone())
    }

    /// Canonical `sideOf(RationalVector)`: the sign of
    /// `this.y * other.x - this.x * other.y`.
    pub fn side_of_rational(&self, other: &RationalVector) -> Side {
        let tmp1 = &self.y * &other.x;
        let tmp2 = &self.x * &other.y;
        let determinant = tmp1 - tmp2;
        let sign = match determinant.sign() {
            Sign::Plus => 1.0,
            Sign::Minus => -1.0,
            Sign::NoSign => 0.0,
        };
        Side::of(sign)
    }

    /// Canonical `projection(RationalVector)`: the sign of the scalar product.
    pub fn projection_rational(&self, other: &RationalVector) -> Signum {
        let tmp1 = &self.x * &other.x;
        let tmp2 = &self.y * &other.y;
        let tmp3 = tmp1 + tmp2;
        let sign = match tmp3.sign() {
            Sign::Plus => 1.0,
            Sign::Minus => -1.0,
            Sign::NoSign => 0.0,
        };
        Signum::of(sign)
    }

    /// Approximated scalar product with another rational vector.
    pub fn scalar_product_rational(&self, other: &RationalVector) -> f64 {
        let v1 = self.to_float();
        let v2 = other.to_float();
        v1.x * v2.x + v1.y * v2.y
    }

    /// Adds another rational vector.
    pub fn add_rational(&self, other: &RationalVector) -> RationalVector {
        let (x, y, z) = BigIntAux::add_rational_coordinates(
            (&self.x, &self.y, &self.z),
            (&other.x, &other.y, &other.z),
        );
        RationalVector::new(x, y, z)
    }

    /// Adds this vector to an `IntPoint`, producing a `RationalPoint`.
    pub fn add_to_int_point(&self, point: &IntVector) -> RationalPoint {
        let mut new_x = &self.z * point.x;
        new_x += &self.x;
        let mut new_y = &self.z * point.y;
        new_y += &self.y;
        RationalPoint::new(new_x, new_y, self.z.clone())
    }

    /// Adds this vector to a `RationalPoint`, producing a `RationalPoint`.
    pub fn add_to_rational_point(&self, point: &RationalPoint) -> RationalPoint {
        let (x, y, z) = BigIntAux::add_rational_coordinates(
            (&self.x, &self.y, &self.z),
            (&point.x, &point.y, &point.z),
        );
        RationalPoint::new(x, y, z)
    }

    /// Approximates the coordinates by float coordinates.
    pub fn to_float(&self) -> FloatPoint {
        let xd = self.x.to_f64().unwrap_or(f64::MAX);
        let yd = self.y.to_f64().unwrap_or(f64::MAX);
        let zd = self.z.to_f64().unwrap_or(f64::MAX);
        FloatPoint::new(xd / zd, yd / zd)
    }

    /// Turns this vector by `factor` times 90 degrees.
    pub fn turn_90_degree(&self, factor: i32) -> RationalVector {
        let mut n = factor;
        while n < 0 {
            n += 4;
        }
        while n >= 4 {
            n -= 4;
        }
        let (new_x, new_y) = match n {
            0 => (self.x.clone(), self.y.clone()),
            1 => (-self.y.clone(), self.x.clone()),
            2 => (-self.x.clone(), -self.y.clone()),
            3 => (self.y.clone(), -self.x.clone()),
            _ => unreachable!("factor reduced modulo 4"),
        };
        RationalVector::new(new_x, new_y, self.z.clone())
    }

    /// Mirrors this vector at the y axis.
    pub fn mirror_at_y_axis(&self) -> RationalVector {
        RationalVector::new(-self.x.clone(), self.y.clone(), self.z.clone())
    }

    /// Mirrors this vector at the x axis.
    pub fn mirror_at_x_axis(&self) -> RationalVector {
        RationalVector::new(self.x.clone(), -self.y.clone(), self.z.clone())
    }

    /// Returns the normalized direction of this vector.
    ///
    /// `BigIntDirection` (directions whose coordinates exceed the `CRIT_INT`
    /// bound) is not yet ported; such directions are rejected explicitly.
    pub fn to_normalized_direction(&self) -> Direction {
        use num_integer::Integer;
        let gcd = self.x.gcd(&self.y);
        let dx = &self.x / &gcd;
        let dy = &self.y / &gcd;
        if dx.abs() <= Limits::crit_int_big() && dy.abs() <= Limits::crit_int_big() {
            let dx = i32::try_from(&dx).expect("x within CRIT_INT");
            let dy = i32::try_from(&dy).expect("y within CRIT_INT");
            Direction::new(dx, dy)
        } else {
            panic!("BigIntDirection is not yet ported");
        }
    }
}
