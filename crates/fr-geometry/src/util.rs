//! `java.lang.Math` rounding, spelled the way the kernel it is ported from spells it.
//!
//! Every place upstream reaches an integer coordinate from a `double` it goes through
//! `Math.round` or `Math.rint` — a sweep of `geometry/planar/` finds no truncating `(int)` cast of
//! a `double` that is not preceded by one of the two:
//!
//! ```text
//! FloatPoint.java:111        new IntPoint((int) Math.round(x), (int) Math.round(y))
//! FloatPoint.java:126,136    round_to_the_right, the "direction is axis-parallel" arms
//! FloatPoint.java:145,150    round_to_grid: Math.rint(x / grid) * grid, then (int)
//! Circle.java:228,235,269    the offset radii
//! Direction.java:69,70,159   the angle → scaled-vector constructors
//! IntBox.java:482,495,508    offset / shrink / bounding distances
//! IntOctagon.java:326,330    offset, and its sqrt2 diagonal
//! Line.java:375,379          translate(p_dist)
//! LineSegment.java:342-447   the stair approximations
//! Polygon.java:129           winding number
//! ```
//!
//! Neither is Rust's [`f64::round`].
//!
//! * `Math.round` is **round half up** — towards positive infinity. `f64::round` is round half
//!   *away from zero*. They agree everywhere except an exact negative half, where
//!   `Math.round(-2.5) == -2` and `(-2.5f64).round() == -3.0`. That is one board unit, on exactly
//!   the coordinates a KiCad rounded-rectangle pad lands on (arc corners emitted at 0.05 µm on a
//!   `(resolution um 10)` board), which is why 67 of the 76 padstack differences in the first full
//!   G1.1 sweep were this single unit on a negative coordinate.
//! * `Math.rint` is **round half to even**, which is [`f64::round_ties_even`] and again not
//!   `f64::round`: `rint(2.5) == 2`, `(2.5f64).round() == 3.0`.
//!
//! This module is the one definition of both; `fr_io::specctra::jround` re-exports it so the
//! importer and the kernel cannot drift apart.

use crate::planar::{FloatPoint, IntPoint};

/// `Math.round(double)`: `floor(x + 0.5)` with the Java 7 fix, `NaN` ⇒ 0, infinities saturating.
///
/// The `floor` / `value - floor >= 0.5` form (rather than `(value + 0.5).floor()`) is what
/// reproduces the fix for `0.49999999999999994`, the largest double below ½: adding ½ to it
/// rounds up to exactly 1.0 in binary64, so the naive form answers 1 where Java answers 0.
#[inline]
pub fn jround(value: f64) -> i64 {
    if value.is_nan() {
        return 0;
    }
    let floor = value.floor();
    let result = if value - floor >= 0.5 {
        floor + 1.0
    } else {
        floor
    };
    // Rust's float-to-int `as` cast saturates, which is what `Math.round` does at the long bounds.
    result as i64
}

/// `(int) Math.round(double)` — [`jround`] followed by Java's narrowing cast.
///
/// Java's `(int)` cast of a `long` keeps the low 32 bits, and so does Rust's `i64 as i32`. The
/// difference from the `f64 as i32` this replaces is only reachable outside the
/// [`CRIT_INT`](crate::planar::Limits::CRIT_INT) envelope, where saturating and wrapping disagree;
/// wrapping is the upstream answer.
#[inline]
pub fn jround_i32(value: f64) -> i32 {
    jround(value) as i32
}

/// `Math.rint(double)`: round half to **even**, the tie rule `Math.round` does *not* use.
///
/// Only `FloatPoint.round_to_grid` uses it upstream, and it matters there: a coordinate exactly
/// half a grid step from two multiples goes to the even multiple, not away from zero.
#[inline]
pub fn jrint(value: f64) -> f64 {
    // `f64::round_ties_even` is 1.77; the crate's MSRV is 1.75, so spell it out. `floor` is exact,
    // so the fractional test below is exact too.
    if !value.is_finite() {
        return value;
    }
    let floor = value.floor();
    let fract = value - floor;
    if fract > 0.5 {
        floor + 1.0
    } else if fract < 0.5 {
        floor
    } else if (floor * 0.5).fract() == 0.0 {
        // `floor` is the even one of the two neighbours.
        floor
    } else {
        floor + 1.0
    }
}

/// `Math.round` for the two shapes that get rounded: a scalar and a [`FloatPoint`].
///
/// Written as a trait so the call sites keep reading like the Java they mirror — `x.jround()` for
/// `Math.round(x)`, `p.jround()` for `p.round()` — and so a stray `f64::round` stands out in a
/// diff. `f64::jround` returns `i64` and the call sites cast with `as i32`, which is exactly
/// Java's `(int) Math.round(…)`: `Math.round` yields a `long` and the `(int)` cast truncates.
pub trait JavaRound {
    /// The integral form of `Self`.
    type Output;

    /// `Math.round` applied to every coordinate.
    fn jround(self) -> Self::Output;
}

impl JavaRound for f64 {
    type Output = i64;

    #[inline]
    fn jround(self) -> i64 {
        jround(self)
    }
}

impl JavaRound for FloatPoint {
    type Output = IntPoint;

    #[inline]
    fn jround(self) -> IntPoint {
        IntPoint::new(jround_i32(self.x), jround_i32(self.y))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_halves_round_towards_positive_infinity() {
        // The whole point of the module: `f64::round` disagrees on exactly these.
        assert_eq!(jround(-2.5), -2);
        assert_eq!((-2.5f64).round() as i64, -3);
        assert_eq!(jround(-0.5), 0);
        assert_eq!(jround(-1.5), -1);
        assert_eq!(jround(-2437.5), -2437);
        assert_eq!(jround(2437.5), 2438);
    }

    #[test]
    fn positive_halves_and_ordinary_values_agree_with_f64_round() {
        for value in [0.0, 0.5, 1.5, 2.4999, -2.4999, 1e6, -1e6, 3.7, -3.7] {
            assert_eq!(jround(value), value.round() as i64, "value {value}");
        }
    }

    #[test]
    fn the_java_7_half_fix_is_reproduced() {
        // `0.49999999999999994 + 0.5 == 1.0` in binary64, so `floor(x + 0.5)` would answer 1.
        assert_eq!(jround(0.499_999_999_999_999_94), 0);
        assert_eq!(jround(-0.499_999_999_999_999_94), 0);
    }

    #[test]
    fn non_finite_input_matches_math_round() {
        assert_eq!(jround(f64::NAN), 0);
        assert_eq!(jround(f64::INFINITY), i64::MAX);
        assert_eq!(jround(f64::NEG_INFINITY), i64::MIN);
    }

    #[test]
    fn points_round_coordinate_by_coordinate() {
        let point = FloatPoint::new(-2437.5, 2437.5).jround();
        assert_eq!(point.x, -2437);
        assert_eq!(point.y, 2438);
    }

    #[test]
    fn rint_rounds_halves_to_even_both_ways() {
        for (value, expected) in [
            (0.5, 0.0),
            (1.5, 2.0),
            (2.5, 2.0),
            (3.5, 4.0),
            (-0.5, 0.0),
            (-1.5, -2.0),
            (-2.5, -2.0),
            (-3.5, -4.0),
            (2.4, 2.0),
            (2.6, 3.0),
            (-2.4, -2.0),
            (-2.6, -3.0),
        ] {
            assert_eq!(jrint(value), expected, "rint({value})");
        }
        // `jrint` is not `jround`: on a tie they agree only when rounding up already lands on the
        // even neighbour.
        assert_ne!(jrint(2.5) as i64, jround(2.5));
        assert_ne!(jrint(-1.5) as i64, jround(-1.5));
        assert_eq!(jrint(-2.5) as i64, jround(-2.5));
    }
}
