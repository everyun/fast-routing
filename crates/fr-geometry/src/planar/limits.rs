//! Numerical limits used by planar geometry, ported from
//! `app.freerouting.geometry.planar.Limits`.
//!
//! # The coordinate envelope and the arithmetic tiers
//!
//! Every integer coordinate that enters the kernel is required to satisfy
//! `|coord| <= CRIT_INT` (2^25). The envelope is enforced at the IO boundary
//! (`IntPoint::try_new` / `IntVector::try_new` / `Line::try_new`), never by a
//! release-mode check inside the hot constructors.
//!
//! The envelope is what makes the fixed-width fast paths exact:
//!
//! | quantity                                 | bound | tier |
//! |------------------------------------------|-------|------|
//! | point coordinate                         | 2^25  | i32  |
//! | vector coordinate (difference of points) | 2^26  | i32  |
//! | product of two vector coordinates        | 2^52  | i64  |
//! | 2x2 determinant of vector coordinates    | 2^53  | i64  |
//! | line-intersection numerator              | 2^78  | i128 |
//! | side-of-intersection determinant         | 2^105 | i128 |
//!
//! Predicates therefore run an `i64` (or `i128`) fast path and promote to the
//! next tier only when a `checked_*` operation reports overflow, which inside
//! the envelope never happens. Heap bignums (`BigInt`) stay the cold-path
//! fallback for rational coordinates and for out-of-envelope input; they are
//! never reached from a hot query loop.

use num_bigint::BigInt;

use crate::planar::side::Side;
use fr_datastructures::Signum;

/// Stores numerical limits and values used by planar geometry.
pub struct Limits;

impl Limits {
    /// An upper bound (2^25) so that the product of two integers with absolute
    /// value at most `CRIT_INT` is contained in the mantissa of a double with
    /// some space left for addition.
    pub const CRIT_INT: i32 = 33_554_432;

    /// `CRIT_INT` widened to `i64`, for exact fixed-width predicates.
    pub const CRIT_INT_I64: i64 = Self::CRIT_INT as i64;

    /// Bound on a *vector* coordinate (2^26). A vector is the difference of two
    /// points inside the coordinate envelope, so its coordinates may reach
    /// twice `CRIT_INT` without leaving the contract.
    pub const CRIT_VECTOR_INT: i32 = 2 * Self::CRIT_INT;

    /// The biggest double value (2^53), so that all integers smaller than this
    /// value are represented exactly as double values.
    pub const CRIT_DOUBLE: f64 = 9_007_199_254_740_992.0;

    /// `sqrt(2)`, used extensively by diagonal-distance computations.
    pub const SQRT2: f64 = std::f64::consts::SQRT_2;

    /// Returns true if `value` is a legal *point* coordinate, i.e.
    /// `|value| <= CRIT_INT`. Uses `unsigned_abs`, so `i32::MIN` is rejected
    /// instead of overflowing.
    #[inline]
    pub const fn in_point_envelope(value: i32) -> bool {
        value.unsigned_abs() <= Self::CRIT_INT as u32
    }

    /// Returns true if `value` is a legal *vector* coordinate, i.e.
    /// `|value| <= 2 * CRIT_INT`.
    #[inline]
    pub const fn in_vector_envelope(value: i32) -> bool {
        value.unsigned_abs() <= Self::CRIT_VECTOR_INT as u32
    }

    /// Returns `CRIT_INT` as a `BigInt` (the Java `Limits.CRIT_INT_BIG`).
    pub fn crit_int_big() -> BigInt {
        BigInt::from(Self::CRIT_INT)
    }
}

/// Maps the sign of an exact integer determinant onto a [`Side`], without going
/// through `f64` (the float overload [`Side::of`] is only exact below
/// `CRIT_DOUBLE`).
#[inline]
pub fn side_of_exact(determinant: i128) -> Side {
    match determinant.cmp(&0) {
        std::cmp::Ordering::Greater => Side::OnTheLeft,
        std::cmp::Ordering::Less => Side::OnTheRight,
        std::cmp::Ordering::Equal => Side::Collinear,
    }
}

/// Maps the sign of an exact integer value onto a [`Signum`], without going
/// through `f64`.
#[inline]
pub fn signum_of_exact(value: i128) -> Signum {
    match value.cmp(&0) {
        std::cmp::Ordering::Greater => Signum::Positive,
        std::cmp::Ordering::Less => Signum::Negative,
        std::cmp::Ordering::Equal => Signum::Zero,
    }
}

/// Exact 2x2 determinant `ax * by - ay * bx` of `i64` operands: an `i64` fast
/// path with checked promotion to `i128`.
///
/// Inside the coordinate envelope the operands are at most 2^26 and the `i64`
/// path always succeeds; the `i128` branch only runs for out-of-envelope input.
#[inline]
pub fn determinant_exact(ax: i64, ay: i64, bx: i64, by: i64) -> i128 {
    match (ax.checked_mul(by), ay.checked_mul(bx)) {
        (Some(p1), Some(p2)) => match p1.checked_sub(p2) {
            Some(determinant) => determinant as i128,
            None => p1 as i128 - p2 as i128,
        },
        _ => (ax as i128) * (by as i128) - (ay as i128) * (bx as i128),
    }
}

/// Exact scalar product `ax * bx + ay * by` of `i64` operands: an `i64` fast
/// path with checked promotion to `i128`.
#[inline]
pub fn scalar_product_exact(ax: i64, ay: i64, bx: i64, by: i64) -> i128 {
    match (ax.checked_mul(bx), ay.checked_mul(by)) {
        (Some(p1), Some(p2)) => match p1.checked_add(p2) {
            Some(sum) => sum as i128,
            None => p1 as i128 + p2 as i128,
        },
        _ => (ax as i128) * (bx as i128) + (ay as i128) * (by as i128),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_upstream() {
        assert_eq!(Limits::CRIT_INT, 1 << 25);
        assert_eq!(Limits::CRIT_DOUBLE, (1u64 << 53) as f64);
        // Checked by its defining property rather than by a decimal literal,
        // which clippy's `approx_constant` lint (deny by default) rejects.
        assert!((Limits::SQRT2 * Limits::SQRT2 - 2.0).abs() < 1e-15);
        assert_eq!(Limits::SQRT2, std::f64::consts::SQRT_2);
    }

    #[test]
    fn envelope_predicates_are_symmetric_and_saturate() {
        assert!(Limits::in_point_envelope(Limits::CRIT_INT));
        assert!(Limits::in_point_envelope(-Limits::CRIT_INT));
        assert!(!Limits::in_point_envelope(Limits::CRIT_INT + 1));
        assert!(!Limits::in_point_envelope(-Limits::CRIT_INT - 1));
        // i32::MIN has no positive counterpart; `unsigned_abs` must not panic.
        assert!(!Limits::in_point_envelope(i32::MIN));
        assert!(!Limits::in_vector_envelope(i32::MIN));
        assert!(Limits::in_vector_envelope(Limits::CRIT_VECTOR_INT));
        assert!(!Limits::in_vector_envelope(Limits::CRIT_VECTOR_INT + 1));
    }

    #[test]
    fn determinant_promotes_to_i128_on_overflow() {
        // Envelope corner: still on the i64 path, and exact.
        let c = Limits::CRIT_INT_I64 * 2;
        assert_eq!(determinant_exact(c, -c, c, c), 2 * (c as i128) * (c as i128));
        // The i64 products overflow here; the i128 path must stay exact.
        let big = i64::MAX / 2;
        assert_eq!(
            determinant_exact(big, -big, big, big),
            2 * (big as i128) * (big as i128)
        );
        assert_eq!(determinant_exact(0, 0, 0, 0), 0);
    }

    #[test]
    fn scalar_product_promotes_to_i128_on_overflow() {
        let big = i64::MAX;
        assert_eq!(
            scalar_product_exact(big, big, big, big),
            2 * (big as i128) * (big as i128)
        );
        assert_eq!(scalar_product_exact(3, 4, 5, 6), 39);
    }

    #[test]
    fn sign_mappings_match_the_float_overloads() {
        assert_eq!(side_of_exact(7), Side::OnTheLeft);
        assert_eq!(side_of_exact(-7), Side::OnTheRight);
        assert_eq!(side_of_exact(0), Side::Collinear);
        assert_eq!(signum_of_exact(i128::MAX), Signum::Positive);
        assert_eq!(signum_of_exact(i128::MIN), Signum::Negative);
        assert_eq!(signum_of_exact(0), Signum::Zero);
    }
}
