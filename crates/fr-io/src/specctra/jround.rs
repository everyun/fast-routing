//! `java.lang.Math.round(double)` — the rounding every DSN → board coordinate goes through.
//!
//! Every `transformToBoard*` in `io/specctra/parser/` reaches an integer through one of two
//! spellings, and both are the same operation:
//!
//! ```text
//! Rectangle.java:48       (int) Math.round(p_coordinate_transform.dsn_to_board(this.coor[i]))
//! Polygon.java:45-46      (int) Math.round(p_coordinate_transform.dsn_to_board(coor[2 * i]))
//! Circle.java:39          (int) Math.round(p_coordinate_transform.dsn_to_board(coor[0]) / 2)
//! Polygon.java:33         p_coordinate_transform.dsn_to_board(curr_point).round()
//! Rectangle.java:71       new IntBox(lower_left.round(), upper_right.round())
//! PolygonPath.java:79     rounded_corner_arr[i] = corner_arr[i].round()
//! ```
//!
//! …because `FloatPoint.round()` is itself `new IntPoint((int) Math.round(x), (int) Math.round(y))`
//! (`geometry/planar/FloatPoint.java:111`). A sweep of `io/specctra/parser/` finds no truncating
//! `(int)` cast of a `double` at all: every one goes through `Math.round` first.
//!
//! `Math.round` is **round half up** — towards positive infinity — while Rust's [`f64::round`] is
//! round half *away from zero*. They agree on every value except an exact negative half, where
//! `Math.round(-2.5) == -2` and `(-2.5f64).round() == -3.0`. That is one board unit, and only on
//! coordinates that land exactly on a half unit — which is most KiCad rounded-rectangle pads,
//! whose corner arcs are emitted at 0.05 µm on a `(resolution um 10)` board. 67 of the 76 padstack
//! differences in the first full G1.1 sweep were this single unit on a negative coordinate.
//!
//! [`fr_geometry::planar::FloatPoint::round`] still rounds half away from zero; it is the geometry
//! kernel's own entry point and outside this crate. The importer therefore rounds through
//! [`JavaRound`] and never through `FloatPoint::round`, so the two can be reconciled
//! independently.

use fr_geometry::planar::{FloatPoint, IntPoint};

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

/// `Math.round` for the two shapes the importer rounds: a scalar and a [`FloatPoint`].
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
        IntPoint::new(jround(self.x) as i32, jround(self.y) as i32)
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
}
