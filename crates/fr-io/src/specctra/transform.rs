//! `io/CoordinateTransform.java` and the scale-factor derivation that feeds it.
//!
//! Every board coordinate in the router is `dsn_value * scale_factor`, so this file decides the
//! unit every cost constant in the maze engine, the shove engine and the search tree is expressed
//! in. `dsn-board-semantics.md` §17.1 puts it first on the "bake into tests" list.

use fr_geometry::planar::FloatPoint;

use super::lex::CRIT_INT;

/// The DSN ↔ board coordinate transform.
///
/// `base_x` / `base_y` exist upstream but are always zero: both construction sites pass
/// `new CoordinateTransform(scaleFactor, 0, 0)`. They are kept so the arithmetic reads like the
/// original.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoordinateTransform {
    scale_factor: f64,
    base_x: f64,
    base_y: f64,
}

impl CoordinateTransform {
    /// Builds a transform from an already-derived scale factor.
    pub fn new(scale_factor: f64, base_x: f64, base_y: f64) -> Self {
        CoordinateTransform {
            scale_factor,
            base_x,
            base_y,
        }
    }

    /// The scale factor, i.e. `dsn_to_board(1.0)`.
    pub fn scale_factor(&self) -> f64 {
        self.scale_factor
    }

    /// Scales a length from DSN units into board units.
    pub fn dsn_to_board(&self, value: f64) -> f64 {
        value * self.scale_factor
    }

    /// Scales a length from board units back into DSN units.
    pub fn board_to_dsn(&self, value: f64) -> f64 {
        value / self.scale_factor
    }

    /// Transforms an absolute point into board coordinates.
    pub fn dsn_to_board_point(&self, x: f64, y: f64) -> FloatPoint {
        FloatPoint::new(
            (x - self.base_x) * self.scale_factor,
            (y - self.base_y) * self.scale_factor,
        )
    }

    /// Transforms a relative point (padstack, package pin offset) — no base translation.
    pub fn dsn_to_board_rel(&self, x: f64, y: f64) -> FloatPoint {
        FloatPoint::new(x * self.scale_factor, y * self.scale_factor)
    }
}

/// The outcome of [`derive_scale_factor`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScaleFactor {
    /// A usable factor.
    Ok(f64),
    /// The bounding box degenerated to a point: `maxCoor == 0`. Upstream sets
    /// `boardOutlineOk = false` and aborts the board construction.
    Degenerate,
}

/// Upstream `Structure.createBoard`'s scale-factor derivation (`Structure.java:1189-1205`).
///
/// ```text
/// scaleFactor = max(resolution, 1)
/// maxCoor     = max over the 4 bbox coords of |coor[i] * resolution|
/// if maxCoor == 0: abort
/// while 5 * maxCoor >= Limits.CRIT_INT:      // CRIT_INT = 2^25
///     scaleFactor /= 10                      // INTEGER division
///     maxCoor    /= 10                       // double division
/// ```
///
/// The two divisions are **not** the same operation: `scaleFactor` is an `int` local seeded from
/// `Math.max(resolution, 1)` and reduced by integer division, while `maxCoor` is a `double`. On a
/// board large enough to need more than `log10(resolution)` iterations the factor collapses to 0
/// and every board coordinate becomes 0. That is reproduced here rather than clamped, because
/// the golden comparator diffs against a jar that behaves this way; a caller that wants the
/// sane behaviour can inspect the returned factor.
///
/// The `5 *` headroom is deliberate slack for intermediate products in the geometry kernel — it
/// is what keeps `Limits.CRIT_INT_BIG` (the `BigInteger` fallback) out of the hot path.
pub fn derive_scale_factor(bounding_box_dsn: [f64; 4], resolution: i32) -> ScaleFactor {
    let mut scale_factor: i64 = i64::from(resolution.max(1));
    let mut max_coor = 0.0f64;
    for value in bounding_box_dsn {
        max_coor = max_coor.max((value * f64::from(resolution)).abs());
    }
    if max_coor == 0.0 {
        return ScaleFactor::Degenerate;
    }
    while 5.0 * max_coor >= CRIT_INT {
        scale_factor /= 10;
        max_coor /= 10.0;
        if scale_factor == 0 && max_coor < 1.0 {
            // Upstream would spin forever only if maxCoor never shrank; it always does. This
            // guard exists so a pathological input cannot hang the importer.
            break;
        }
    }
    ScaleFactor::Ok(scale_factor as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_board_keeps_the_resolution_as_the_scale_factor() {
        // Issue026: (resolution um 10), outline x in [106045, 153035], y in [-98425, -73025].
        // 5 * 153035 * 10 = 7_651_750 < 2^25, so the loop never runs.
        let factor = derive_scale_factor([106045.0, -98425.0, 153035.0, -73025.0], 10);
        assert_eq!(factor, ScaleFactor::Ok(10.0));
    }

    #[test]
    fn large_board_reduces_the_factor_by_integer_division() {
        // 5 * 1e6 * 100 = 5e8 >= 2^25 -> divide once: 5e7 still >= 2^25 -> twice: 5e6 < 2^25.
        // scaleFactor 100 -> 10 -> 1.
        let factor = derive_scale_factor([0.0, 0.0, 1_000_000.0, 1_000_000.0], 100);
        assert_eq!(factor, ScaleFactor::Ok(1.0));
    }

    #[test]
    fn integer_division_can_collapse_the_factor_to_zero() {
        // Three reductions are needed but the factor only survives two.
        let factor = derive_scale_factor([0.0, 0.0, 1_000_000_000.0, 1_000_000_000.0], 10);
        assert_eq!(factor, ScaleFactor::Ok(0.0));
    }

    #[test]
    fn degenerate_bounding_box_is_reported() {
        assert_eq!(
            derive_scale_factor([0.0, 0.0, 0.0, 0.0], 10),
            ScaleFactor::Degenerate
        );
    }

    #[test]
    fn transform_round_trips() {
        let transform = CoordinateTransform::new(10.0, 0.0, 0.0);
        assert_eq!(transform.dsn_to_board(12345.0), 123_450.0);
        assert_eq!(transform.board_to_dsn(123_450.0), 12345.0);
        let point = transform.dsn_to_board_point(1.5, -2.5);
        assert_eq!(point.x, 15.0);
        assert_eq!(point.y, -25.0);
    }
}
