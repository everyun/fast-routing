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
//! The definition lives in [`fr_geometry::util`], because
//! [`FloatPoint::round`](fr_geometry::planar::FloatPoint::round) rounds this way too — the kernel
//! and the importer share one `Math.round`, so they cannot drift apart. This module is the
//! importer's name for it.

pub use fr_geometry::util::{jround, JavaRound};

#[cfg(test)]
mod tests {
    use super::*;
    use fr_geometry::planar::FloatPoint;

    #[test]
    fn the_importer_sees_java_round_semantics() {
        // Guard rail on the re-export: the trait the call sites use is the half-up one.
        assert_eq!(jround(-2437.5), -2437);
        assert_eq!((-2437.5f64).round() as i64, -2438);
        assert_eq!((-2437.5f64).jround(), -2437);
        let point = FloatPoint::new(-2437.5, 2437.5).jround();
        assert_eq!((point.x, point.y), (-2437, 2438));
    }

    #[test]
    fn float_point_round_is_the_same_operation_now() {
        // The reason this module is a re-export: `FloatPoint::round` used to disagree with it.
        for (x, y) in [(-2.5, 2.5), (-0.5, 0.5), (-2437.5, 2437.5), (3.7, -3.7)] {
            let point = FloatPoint::new(x, y);
            assert_eq!(point.round(), point.jround(), "({x}, {y})");
        }
    }
}
