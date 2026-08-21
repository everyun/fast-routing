//! The mathematical sign function, ported from
//! `app.freerouting.datastructures.Signum`.

/// Implements the mathematical signum function as a three-valued enum.
///
/// This mirrors `app.freerouting.datastructures.Signum`, which has exactly three
/// instances: `POSITIVE`, `NEGATIVE` and `ZERO`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Signum {
    Positive,
    Negative,
    Zero,
}

impl Signum {
    /// Returns the sign of `value`: `Positive` if `value > 0`, `Negative` if
    /// `value < 0`, and `Zero` otherwise.
    pub fn of(value: f64) -> Self {
        if value > 0.0 {
            Signum::Positive
        } else if value < 0.0 {
            Signum::Negative
        } else {
            Signum::Zero
        }
    }

    /// Returns the sign of `value` as an `i32`: `1`, `0` or `-1`.
    pub fn as_int(value: f64) -> i32 {
        if value > 0.0 {
            1
        } else if value < 0.0 {
            -1
        } else {
            0
        }
    }

    /// Returns the opposite sign; `Zero` is its own opposite.
    pub fn negate(self) -> Self {
        match self {
            Signum::Positive => Signum::Negative,
            Signum::Negative => Signum::Positive,
            Signum::Zero => Signum::Zero,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn of_positive_negative_zero() {
        assert_eq!(Signum::of(3.5), Signum::Positive);
        assert_eq!(Signum::of(-0.001), Signum::Negative);
        assert_eq!(Signum::of(0.0), Signum::Zero);
    }

    #[test]
    fn as_int_values() {
        assert_eq!(Signum::as_int(10.0), 1);
        assert_eq!(Signum::as_int(-10.0), -1);
        assert_eq!(Signum::as_int(0.0), 0);
    }

    #[test]
    fn negate_is_involution() {
        assert_eq!(Signum::Positive.negate(), Signum::Negative);
        assert_eq!(Signum::Negative.negate(), Signum::Positive);
        assert_eq!(Signum::Zero.negate(), Signum::Zero);
    }
}
