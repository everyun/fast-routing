//! Integer vector, ported from `app.freerouting.geometry.planar.IntVector`.

use crate::planar::{direction::Direction, float_point::FloatPoint, side::Side};
use fr_datastructures::{BigIntAux, Signum};

/// Implementation of a vector via a tuple of integers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IntVector {
    pub x: i32,
    pub y: i32,
}

impl IntVector {
    /// Creates an `IntVector` from two integer coordinates. The range check is
    /// omitted for performance reasons (matching upstream).
    #[inline]
    pub const fn new(x: i32, y: i32) -> Self {
        IntVector { x, y }
    }

    /// The zero vector.
    pub const ZERO: IntVector = IntVector::new(0, 0);

    /// Returns true if both coordinates of this vector are 0.
    #[inline]
    pub fn is_zero(&self) -> bool {
        self.x == 0 && self.y == 0
    }

    /// Returns true if the vector is horizontal or vertical.
    #[inline]
    pub fn is_orthogonal(&self) -> bool {
        self.x == 0 || self.y == 0
    }

    /// Returns true if the vector is diagonal (45° multiple).
    #[inline]
    pub fn is_diagonal(&self) -> bool {
        self.x.abs() == self.y.abs()
    }

    /// Returns true if the vector is orthogonal or diagonal.
    #[inline]
    pub fn is_multiple_of_45_degree(&self) -> bool {
        self.is_orthogonal() || self.is_diagonal()
    }

    /// Calculates the determinant of the matrix consisting of `this` and `other`.
    #[inline]
    pub fn determinant(&self, other: &IntVector) -> i64 {
        (self.x as i64) * (other.y as i64) - (self.y as i64) * (other.x as i64)
    }

    /// Canonical `sideOf(IntVector)` computation: the sign of
    /// `this.y * other.x - this.x * other.y`.
    #[inline]
    pub fn side_of_int(&self, other: &IntVector) -> Side {
        let determinant = (self.y as f64) * (other.x as f64) - (self.x as f64) * (other.y as f64);
        Side::of(determinant)
    }

    /// Canonical `projection(IntVector)` computation: the sign of the scalar
    /// product `this . other`.
    #[inline]
    pub fn projection_int(&self, other: &IntVector) -> Signum {
        let tmp = (self.x as f64) * (other.x as f64) + (self.y as f64) * (other.y as f64);
        Signum::of(tmp)
    }

    /// Canonical `scalarProduct(IntVector)` computation.
    #[inline]
    pub fn scalar_product_int(&self, other: &IntVector) -> f64 {
        (self.x as f64) * (other.x as f64) + (self.y as f64) * (other.y as f64)
    }

    /// Adds `other` to this vector (integer fast path).
    #[inline]
    pub fn add_int(&self, other: &IntVector) -> IntVector {
        IntVector::new(self.x + other.x, self.y + other.y)
    }

    /// Turns this vector by `factor` times 90 degrees.
    pub fn turn_90_degree_int(&self, factor: i32) -> IntVector {
        let mut n = factor;
        while n < 0 {
            n += 4;
        }
        while n >= 4 {
            n -= 4;
        }
        let (new_x, new_y) = match n {
            0 => (self.x, self.y),
            1 => (-self.y, self.x),
            2 => (-self.x, -self.y),
            3 => (self.y, -self.x),
            _ => unreachable!("factor reduced modulo 4"),
        };
        IntVector::new(new_x, new_y)
    }

    /// Mirrors this vector at the y axis.
    #[inline]
    pub fn mirror_at_y_axis_int(&self) -> IntVector {
        IntVector::new(-self.x, self.y)
    }

    /// Mirrors this vector at the x axis.
    #[inline]
    pub fn mirror_at_x_axis_int(&self) -> IntVector {
        IntVector::new(self.x, -self.y)
    }

    /// Converts this vector to a `FloatPoint`.
    #[inline]
    pub fn to_float(&self) -> FloatPoint {
        FloatPoint::new(self.x as f64, self.y as f64)
    }

    /// Returns the normalized direction of this vector.
    pub fn to_normalized_direction(&self) -> Direction {
        let mut dx = self.x;
        let mut dy = self.y;
        let gcd = BigIntAux::binary_gcd(dx.abs(), dy.abs());
        if gcd > 1 {
            dx /= gcd;
            dy /= gcd;
        }
        Direction::new(dx, dy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn determinant_value() {
        let a = IntVector::new(2, 3);
        let b = IntVector::new(4, 5);
        assert_eq!(a.determinant(&b), 2 * 5 - 3 * 4); // -2
    }

    #[test]
    fn side_of_int_sign_convention() {
        // a = (1, 0), b = (0, 1): a.y*b.x - a.x*b.y = 0*0 - 1*1 = -1 -> OnTheRight
        let a = IntVector::new(1, 0);
        let b = IntVector::new(0, 1);
        assert_eq!(a.side_of_int(&b), Side::OnTheRight);
        assert_eq!(b.side_of_int(&a), Side::OnTheLeft);
    }

    #[test]
    fn projection_int_sign() {
        let a = IntVector::new(1, 0);
        let b = IntVector::new(1, 0);
        assert_eq!(a.projection_int(&b), Signum::Positive);
        let c = IntVector::new(-1, 0);
        assert_eq!(a.projection_int(&c), Signum::Negative);
        let d = IntVector::new(0, 1);
        assert_eq!(a.projection_int(&d), Signum::Zero);
    }

    #[test]
    fn turn_90_degree_cycles() {
        let v = IntVector::new(1, 2);
        assert_eq!(v.turn_90_degree_int(1), IntVector::new(-2, 1));
        assert_eq!(v.turn_90_degree_int(4), v);
        assert_eq!(v.turn_90_degree_int(-1), IntVector::new(2, -1));
    }

    #[test]
    fn to_normalized_direction_reduces() {
        let v = IntVector::new(6, 3);
        assert_eq!(v.to_normalized_direction(), Direction::new(2, 1));
    }
}
