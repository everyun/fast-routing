//! `PolygonShape.split_to_convex` — a simple polygon cut into convex tiles.
//!
//! Upstream's `Area` interface derives every consumer-visible tile through `splitToConvex()`:
//! the shape search tree indexes the pieces, the connectivity scan overlaps against them, and
//! `Structure.separateHoles` tests containment with them. The decomposition is therefore a
//! *geometry* operation computed on demand from a stored polygon — `ObstacleArea` keeps the
//! original `relativeArea` and never bakes the split into it (`ObstacleArea.java:84-91`), which
//! is why this lives here rather than in the importer.
//!
//! Without it, a concave area can only be indexed by its **bounding octagon**, which is safe for
//! the router (it never misses an obstacle) and wrong for the DRC: every concave notch becomes
//! copper. Issue022, Issue029 and Issue054 each reported clearance violations against traces that
//! upstream places in the notch of an L-shaped keepout — 52 records between them.
//!
//! # The algorithm
//!
//! `PolygonShape.split_to_convex_recu` (`geometry/planar/PolygonShape.java:534-616`): find a
//! concave corner; from it, shoot the shortest axis-parallel ray that lands on the border and
//! leaves a convex corner behind (`DivisionPoint`, `:621-746`); cut the polygon in two along it;
//! recurse on both halves. A polygon with no concave corner is already a tile.
//!
//! Two deliberate departures, neither observable in the union of the pieces — which is all any
//! consumer reads, since overlap is tested piece by piece and upstream's own division points are
//! rounded to the integer grid anyway:
//!
//! * **No random start.** Upstream seeds a `java.util.Random` with 99 and starts the concave-corner
//!   scan at `random.nextInt(corner_count)`, "to get reproducible result" — reproducible, but
//!   arbitrary, and it only decides *which* concave corner is cut first. Starting at corner 0 is
//!   just as reproducible and does not put a Java PRNG in the kernel.
//! * **A work list, not recursion.** Upstream recurses once per cut, so a 2000-corner plane can
//!   nest 2000 frames deep. The pieces are independent, so a `Vec` used as a stack does the same
//!   work without the stack depth.
//!
//! Both sides can fail — a self-intersecting corner list has concave corners whose ray hits
//! nothing — and both answer `None` there, which puts the caller back on the bounding-octagon
//! fallback that was the only behaviour before.

use crate::planar::{FloatPoint, IntPoint, Line, Point, Polygon, Side, Simplex};
use crate::util::JavaRound;

/// Upper bound on the number of pieces one polygon may be cut into.
///
/// Every cut replaces an `n`-corner piece by an `a`-corner and a `b`-corner one with
/// `a + b = n + 3`, so a terminating split of an `n`-gon yields fewer than `n` pieces. Upstream
/// has no bound and trusts the geometry; a corner list that is not a simple polygon can defeat
/// that trust, and a caller that hangs on a malformed file is worse than one that falls back.
const MAX_PIECES: usize = 4;

impl Polygon {
    /// [`split_to_convex`] on this polygon's corners.
    ///
    /// `None` for a polygon with a rational corner: a board area's corners are always integral,
    /// and the split is defined on the integer grid.
    pub fn split_to_convex(&self) -> Option<Vec<Simplex>> {
        let mut corners = Vec::with_capacity(self.corner_array().len());
        for point in self.corner_array() {
            match point {
                Point::Int(int_point) => corners.push(*int_point),
                Point::Rational(_) => return None,
            }
        }
        split_to_convex(&corners)
    }
}

/// Splits a closed corner list into convex [`Simplex`] tiles whose union is the polygon.
///
/// `None` means the split did not terminate in a convex decomposition — a degenerate corner list
/// (fewer than three distinct, non-collinear corners) or one whose border crosses itself.
pub fn split_to_convex(corners: &[IntPoint]) -> Option<Vec<Simplex>> {
    let start = normalize(corners)?;
    let limit = MAX_PIECES * start.len() + 16;
    let mut pending = vec![start];
    let mut convex: Vec<Vec<IntPoint>> = Vec::new();
    while let Some(piece) = pending.pop() {
        if convex.len() + pending.len() > limit {
            return None;
        }
        match concave_corner(&piece) {
            None => convex.push(piece),
            Some(corner_no) => {
                let (first, last) = divide(&piece, corner_no)?;
                // A cut that leaves either half degenerate is a cut that made no progress.
                pending.push(normalize(&first)?);
                pending.push(normalize(&last)?);
            }
        }
    }
    convex.iter().map(|piece| tile(piece)).collect()
}

/// `new PolygonShape(Point[])` (`PolygonShape.java:26-88`): counter-clockwise, no repeated or
/// collinear corners, and rotated so that the lowest corner — lowest `y`, then lowest `x` — comes
/// first.
///
/// The rotation is upstream's and is kept because the cut indices below are relative to it; the
/// orientation is load-bearing, because "concave" is `Side::OnTheRight` only for a
/// counter-clockwise border.
fn normalize(corners: &[IntPoint]) -> Option<Vec<IntPoint>> {
    let points: Vec<Point> = corners.iter().map(|c| Point::Int(*c)).collect();
    let polygon = Polygon::new(&points);
    let polygon = if polygon.winding_number_after_closing() < 0 {
        polygon.revert_corners()
    } else {
        polygon
    };
    let mut arr: Vec<IntPoint> = Vec::with_capacity(polygon.corner_array().len());
    for point in polygon.corner_array() {
        match point {
            Point::Int(int_point) => arr.push(*int_point),
            // Every corner a board area carries is integral; a rational one cannot be a polygon
            // corner in this model.
            Point::Rational(_) => return None,
        }
    }
    let mut last = arr.len().checked_sub(1)?;
    if last > 0 && arr[0] == arr[last] {
        last -= 1;
    }
    if last >= 2 && side_of(&arr[last], &arr[last - 1], &arr[0]) == Side::Collinear {
        last -= 1;
    }
    let mut first = 0;
    if last >= 2 && side_of(&arr[0], &arr[1], &arr[last]) == Side::Collinear {
        first = 1;
    }
    if last < first + 2 {
        return None;
    }
    let mut start = first;
    for index in (first + 1)..=last {
        let corner = arr[index];
        let best = arr[start];
        if corner.y < best.y || (corner.y == best.y && corner.x < best.x) {
            start = index;
        }
    }
    let mut result = Vec::with_capacity(last - first + 1);
    result.extend_from_slice(&arr[start..=last]);
    result.extend_from_slice(&arr[first..start]);
    Some(result)
}

/// `p_point.side_of(p_from, p_to)`, i.e. which side of the ray `from → to` the point is on.
fn side_of(point: &IntPoint, from: &IntPoint, to: &IntPoint) -> Side {
    Point::Int(*point).side_of_points(&Point::Int(*from), &Point::Int(*to))
}

/// The first corner at which the border turns clockwise, scanning from corner 0.
///
/// Upstream starts the scan at a seeded-random corner; see the module docs.
fn concave_corner(corners: &[IntPoint]) -> Option<usize> {
    let count = corners.len();
    (0..count).find(|&index| {
        let previous = corners[(index + count - 1) % count];
        let next = corners[(index + 1) % count];
        side_of(&next, &previous, &corners[index]) == Side::OnTheRight
    })
}

/// The two halves the polygon is cut into at `concave_corner_no`.
///
/// `PolygonShape.split_to_convex_recu` (`:576-616`): the first half runs from the concave corner
/// forward to the corner before the projection and closes on the projection; the second runs from
/// the projection forward to the concave corner.
fn divide(
    corners: &[IntPoint],
    concave_corner_no: usize,
) -> Option<(Vec<IntPoint>, Vec<IntPoint>)> {
    let count = corners.len();
    let (projection, after_projection) = division_point(corners, concave_corner_no)?;
    let projection = projection.jround();

    let mut corner_count = (after_projection + count - concave_corner_no) % count + 1;
    let mut first = Vec::with_capacity(corner_count);
    let mut index = concave_corner_no;
    for _ in 0..corner_count - 1 {
        first.push(corners[index]);
        index = (index + 1) % count;
    }
    first.push(projection);

    corner_count = (concave_corner_no + count - after_projection) % count + 2;
    let mut last = Vec::with_capacity(corner_count);
    last.push(projection);
    index = after_projection;
    for _ in 1..corner_count {
        last.push(corners[index]);
        index = (index + 1) % count;
    }
    Some((first, last))
}

/// `PolygonShape.DivisionPoint` (`:621-746`): from the concave corner, the nearest point on the
/// border reachable by an axis-parallel ray that leaves a convex corner behind, plus the index of
/// the border corner just after it.
///
/// The `search_*` flags are what "leaves a convex corner behind" means: a ray may only be shot
/// into a quadrant the two edges at the concave corner actually open onto, or the cut would fold
/// the piece back over itself where the border runs back into itself.
fn division_point(corners: &[IntPoint], concave_corner_no: usize) -> Option<(FloatPoint, usize)> {
    let count = corners.len();
    let concave = corners[concave_corner_no].to_float();
    let before = corners[(concave_corner_no + count - 1) % count].to_float();
    let after = corners[(concave_corner_no + 1) % count].to_float();

    let search_right = before.y > concave.y || concave.y > after.y;
    let search_left = before.y < concave.y || concave.y < after.y;
    let search_up = before.x < concave.x || concave.x < after.x;
    let search_down = before.x > concave.x || concave.x > after.x;

    // `Integer.MAX_VALUE`, as the sentinel upstream compares against.
    let mut min_distance = f64::from(i32::MAX);
    let mut min_projection: Option<FloatPoint> = None;
    let mut corner_after_min = 0usize;

    let mut after_current = (concave_corner_no + 2) % count;
    let mut before_current = corners[(after_current + count - 1) % count];
    let mut before_approx = before_current.to_float();

    for _ in 0..count - 2 {
        let after_current_corner = corners[after_current];
        let after_approx = after_current_corner.to_float();
        let line = Line::try_new(before_current, after_current_corner)?;

        if before_approx.y != after_approx.y {
            // a horizontal division
            let (min_y, max_y) = min_max(before_approx.y, after_approx.y);
            if concave.y >= min_y && concave.y <= max_y {
                let x_intersect = line.function_in_y_value_approx(concave.y);
                let distance = (x_intersect - concave.x).abs();
                let usable = distance < min_distance
                    && (search_right && x_intersect > concave.x && concave.y <= after_approx.y
                        || search_left && x_intersect < concave.x && concave.y >= after_approx.y);
                if usable {
                    min_distance = distance;
                    corner_after_min = after_current;
                    min_projection = Some(FloatPoint::new(x_intersect, concave.y));
                }
            }
        }

        if before_approx.x != after_approx.x {
            // a vertical division
            let (min_x, max_x) = min_max(before_approx.x, after_approx.x);
            if concave.x >= min_x && concave.x <= max_x {
                let y_intersect = line.function_value_approx(concave.x);
                let distance = (y_intersect - concave.y).abs();
                let usable = distance < min_distance
                    && (search_up && y_intersect > concave.y && concave.x >= after_approx.x
                        || search_down && y_intersect < concave.y && concave.x <= after_approx.x);
                if usable {
                    min_distance = distance;
                    corner_after_min = after_current;
                    min_projection = Some(FloatPoint::new(concave.x, y_intersect));
                }
            }
        }

        before_current = after_current_corner;
        before_approx = after_approx;
        after_current = (after_current + 1) % count;
    }
    min_projection.map(|projection| (projection, corner_after_min))
}

fn min_max(first: f64, second: f64) -> (f64, f64) {
    if second > first {
        (first, second)
    } else {
        (second, first)
    }
}

/// `TileShape.get_instance(Point[])` (`TileShape.java:29-36`): the convex polygon as the
/// intersection of the half planes left of its edges.
fn tile(corners: &[IntPoint]) -> Option<Simplex> {
    let count = corners.len();
    let mut lines = Vec::with_capacity(count);
    for index in 0..count {
        lines.push(Line::try_new(corners[index], corners[(index + 1) % count])?);
    }
    let simplex = Simplex::get_instance(&lines);
    (!simplex.is_empty()).then_some(simplex)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn points(coords: &[(i32, i32)]) -> Vec<IntPoint> {
        coords.iter().map(|(x, y)| IntPoint::new(*x, *y)).collect()
    }

    fn covers(pieces: &[Simplex], x: i32, y: i32) -> bool {
        let point = Point::Int(IntPoint::new(x, y));
        pieces.iter().any(|piece| piece.contains(&point))
    }

    #[test]
    fn a_convex_polygon_is_one_piece() {
        let pieces = split_to_convex(&points(&[(0, 0), (100, 0), (100, 100), (0, 100)])).unwrap();
        assert_eq!(pieces.len(), 1);
        assert!(covers(&pieces, 50, 50));
        assert!(!covers(&pieces, 150, 50));
    }

    /// The shape of the keepouts that made this module necessary: the notch must stay empty.
    #[test]
    fn an_l_shape_leaves_its_notch_uncovered() {
        let l = points(&[(0, 0), (100, 0), (100, 40), (40, 40), (40, 100), (0, 100)]);
        let pieces = split_to_convex(&l).unwrap();
        assert!(pieces.len() > 1);
        assert!(covers(&pieces, 20, 20));
        assert!(covers(&pieces, 80, 20));
        assert!(covers(&pieces, 20, 80));
        // The notch — inside the bounding box, outside the polygon.
        assert!(!covers(&pieces, 80, 80));
    }

    /// Orientation is normalised, so the same corners the other way round decompose the same.
    #[test]
    fn a_clockwise_corner_list_is_reverted_first() {
        let mut l = points(&[(0, 0), (100, 0), (100, 40), (40, 40), (40, 100), (0, 100)]);
        let counter_clockwise = split_to_convex(&l).unwrap();
        l.reverse();
        let clockwise = split_to_convex(&l).unwrap();
        assert_eq!(clockwise.len(), counter_clockwise.len());
        assert!(covers(&clockwise, 20, 20));
        assert!(!covers(&clockwise, 80, 80));
    }

    /// A staircase: several concave corners, each needing its own cut.
    #[test]
    fn every_concave_corner_gets_cut() {
        let stair = points(&[
            (0, 0),
            (90, 0),
            (90, 30),
            (60, 30),
            (60, 60),
            (30, 60),
            (30, 90),
            (0, 90),
        ]);
        let pieces = split_to_convex(&stair).unwrap();
        assert!(covers(&pieces, 10, 10));
        assert!(covers(&pieces, 80, 10));
        assert!(covers(&pieces, 10, 80));
        assert!(!covers(&pieces, 80, 80));
        assert!(!covers(&pieces, 50, 70));
        assert!(covers(&pieces, 50, 20));
    }

    #[test]
    fn a_degenerate_corner_list_has_no_decomposition() {
        assert!(split_to_convex(&points(&[(0, 0), (100, 0)])).is_none());
        assert!(split_to_convex(&points(&[(0, 0), (50, 0), (100, 0)])).is_none());
        assert!(split_to_convex(&[]).is_none());
    }

    /// The closing corner upstream drops, and the collinear one it drops with it.
    #[test]
    fn a_repeated_closing_corner_is_dropped() {
        let closed = points(&[(0, 0), (100, 0), (100, 100), (0, 100), (0, 0)]);
        let pieces = split_to_convex(&closed).unwrap();
        assert_eq!(pieces.len(), 1);
        assert!(covers(&pieces, 50, 50));
    }

    /// The [`Polygon`] method is the free function on the polygon's own corners — which is what
    /// makes it safe for a consumer to store the polygon and split on demand, the way upstream's
    /// `ObstacleArea` does.
    #[test]
    fn the_polygon_method_agrees_with_the_free_function() {
        let l = points(&[(0, 0), (100, 0), (100, 40), (40, 40), (40, 100), (0, 100)]);
        let polygon = Polygon::new(&l.iter().map(|p| Point::Int(*p)).collect::<Vec<_>>());
        assert_eq!(polygon.split_to_convex(), split_to_convex(&l));
    }
}
