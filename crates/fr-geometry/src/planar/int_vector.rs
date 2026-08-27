//! Integer vector, ported from `app.freerouting.geometry.planar.IntVector`.

use crate::planar::{
    direction::Direction,
    float_point::FloatPoint,
    limits::{determinant_exact, scalar_product_exact, side_of_exact, signum_of_exact, Limits},
    side::Side,
};
use fr_datastructures::{BigIntAux, Signum};

/// Implementation of a vector via a tuple of integers.
///
/// # Invariant
///
/// An `IntVector` is the difference of two points inside the coordinate
/// envelope (`|coord| <= Limits::CRIT_INT`), so its own coordinates are
/// contracted to `|coord| <= Limits::CRIT_VECTOR_INT` (`2 * CRIT_INT`, 2^26).
/// That bound is what keeps the fixed-width predicates on this type inside
/// their fast tier: products stay below 2^52 and determinants below 2^53.
///
/// The invariant is enforced at the IO boundary ([`IntVector::try_new`]), not
/// in the hot constructor: the envelope is not closed under the arithmetic this
/// type performs (`add_int` of two legal vectors reaches `4 * CRIT_INT`), so an
/// assertion there would fire in debug builds on values release builds handle
/// fine. Instead every predicate below stays *exact* for the whole `i32` range
/// by promoting to `i128` when the `i64` tier would overflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IntVector {
    pub x: i32,
    pub y: i32,
}

impl IntVector {
    /// Creates an `IntVector` from two integer coordinates, unchecked, as
    /// upstream does; use [`IntVector::try_new`] at the IO boundary.
    #[inline]
    pub const fn new(x: i32, y: i32) -> Self {
        IntVector { x, y }
    }

    /// Checked constructor for the IO boundary: returns `None` if a coordinate
    /// leaves the `2 * CRIT_INT` vector envelope.
    #[inline]
    pub const fn try_new(x: i32, y: i32) -> Option<Self> {
        if Limits::in_vector_envelope(x) && Limits::in_vector_envelope(y) {
            Some(IntVector { x, y })
        } else {
            None
        }
    }

    /// Returns true if both coordinates respect the vector envelope.
    #[inline]
    pub const fn is_in_envelope(&self) -> bool {
        Limits::in_vector_envelope(self.x) && Limits::in_vector_envelope(self.y)
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
    ///
    /// Exact for every pair of `i32` coordinates: both products are bounded by
    /// 2^62 and their difference could only reach 2^63 if all four coordinates
    /// were `i32::MIN`, which forces the two products to have the same sign, so
    /// the subtraction never overflows `i64`.
    #[inline]
    pub fn determinant(&self, other: &IntVector) -> i64 {
        (self.x as i64) * (other.y as i64) - (self.y as i64) * (other.x as i64)
    }

    /// Canonical `sideOf(IntVector)` computation: the sign of
    /// `this.y * other.x - this.x * other.y`, evaluated exactly (`i64` fast
    /// path, `i128` on overflow) so that the orientation never depends on a
    /// rounded `f64`.
    #[inline]
    pub fn side_of_int(&self, other: &IntVector) -> Side {
        // `this.y * other.x - this.x * other.y` is the determinant of
        // (other, this).
        side_of_exact(determinant_exact(
            other.x as i64,
            other.y as i64,
            self.x as i64,
            self.y as i64,
        ))
    }

    /// Canonical `projection(IntVector)` computation: the sign of the scalar
    /// product `this . other`, evaluated exactly. The sum of the two products
    /// reaches 2^63 for `i32::MIN` inputs, so the `i64` path is checked and
    /// promotes to `i128`.
    #[inline]
    pub fn projection_int(&self, other: &IntVector) -> Signum {
        signum_of_exact(scalar_product_exact(
            self.x as i64,
            self.y as i64,
            other.x as i64,
            other.y as i64,
        ))
    }

    /// Canonical `scalarProduct(IntVector)` computation.
    ///
    /// Deliberately `f64`: this is the *magnitude* used by cost and angle
    /// heuristics. Every decision that depends on its sign goes through
    /// [`IntVector::projection_int`], which is exact.
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

    /// The determinant of these two vectors is exactly 1, but both products are
    /// 2^60, where an `f64` has an ulp of 256: the old float predicate answered
    /// `Collinear`. The coordinates are outside the envelope on purpose — the
    /// predicate has to stay exact there, since `new` is unchecked.
    #[test]
    fn side_of_int_is_exact_beyond_the_double_mantissa() {
        let a = IntVector::new(1 << 30, (1 << 30) + 1);
        let b = IntVector::new((1 << 30) - 1, 1 << 30);
        assert_eq!(a.determinant(&b), 1);
        assert_eq!(a.side_of_int(&b), Side::OnTheRight);
        assert_eq!(b.side_of_int(&a), Side::OnTheLeft);
        // The float version of the same expression collapses to zero.
        let float_det = (a.y as f64) * (b.x as f64) - (a.x as f64) * (b.y as f64);
        assert_eq!(float_det, 0.0);
    }

    /// `side_of` is antisymmetric for every pair, including the corners of the
    /// vector envelope where the products reach 2^52.
    #[test]
    fn side_of_int_is_antisymmetric_at_the_envelope_corners() {
        let c = Limits::CRIT_VECTOR_INT;
        let corners = [
            IntVector::new(c, c),
            IntVector::new(c, -c),
            IntVector::new(-c, c),
            IntVector::new(-c, -c),
            IntVector::new(c, c - 1),
            IntVector::new(c - 1, c),
            IntVector::new(0, c),
            IntVector::new(c, 0),
        ];
        for a in &corners {
            for b in &corners {
                assert_eq!(a.side_of_int(b), b.side_of_int(a).negate(), "{a:?} {b:?}");
            }
        }
    }

    #[test]
    fn projection_int_is_exact_and_promotes_on_overflow() {
        // Exact value -1; in f64 both products round to +-2^60 and cancel.
        let a = IntVector::new((1 << 30) + 1, -(1 << 30));
        let b = IntVector::new((1 << 30) - 1, 1 << 30);
        assert_eq!(a.projection_int(&b), Signum::Negative);
        assert_eq!(
            (a.x as f64) * (b.x as f64) + (a.y as f64) * (b.y as f64),
            0.0
        );
        // i32::MIN squared twice is 2^63: the i64 sum overflows and the
        // predicate must promote to i128 instead of wrapping to a negative.
        let m = IntVector::new(i32::MIN, i32::MIN);
        assert_eq!(m.projection_int(&m), Signum::Positive);
    }

    #[test]
    fn try_new_rejects_out_of_envelope_coordinates() {
        assert_eq!(
            IntVector::try_new(Limits::CRIT_VECTOR_INT, -Limits::CRIT_VECTOR_INT),
            Some(IntVector::new(
                Limits::CRIT_VECTOR_INT,
                -Limits::CRIT_VECTOR_INT
            ))
        );
        assert_eq!(IntVector::try_new(Limits::CRIT_VECTOR_INT + 1, 0), None);
        assert_eq!(IntVector::try_new(0, i32::MIN), None);
        assert!(!IntVector::new(i32::MIN, 0).is_in_envelope());
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
