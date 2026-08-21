//! Numerical limits used by planar geometry, ported from
//! `app.freerouting.geometry.planar.Limits`.

use num_bigint::BigInt;

/// Stores numerical limits and values used by planar geometry.
pub struct Limits;

impl Limits {
    /// An upper bound (2^25) so that the product of two integers with absolute
    /// value at most `CRIT_INT` is contained in the mantissa of a double with
    /// some space left for addition.
    pub const CRIT_INT: i32 = 33_554_432;

    /// The biggest double value (2^53), so that all integers smaller than this
    /// value are represented exactly as double values.
    pub const CRIT_DOUBLE: f64 = 9_007_199_254_740_992.0;

    /// `sqrt(2)`, used extensively by diagonal-distance computations.
    pub const SQRT2: f64 = std::f64::consts::SQRT_2;

    /// Returns `CRIT_INT` as a `BigInt` (the Java `Limits.CRIT_INT_BIG`).
    pub fn crit_int_big() -> BigInt {
        BigInt::from(Self::CRIT_INT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_upstream() {
        assert_eq!(Limits::CRIT_INT, 1 << 25);
        assert_eq!(Limits::CRIT_DOUBLE, (1u64 << 53) as f64);
        assert!((Limits::SQRT2 - 1.4142135623730951).abs() < 1e-15);
    }
}
