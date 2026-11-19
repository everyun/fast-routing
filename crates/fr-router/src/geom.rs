//! The `TileShape` / `FloatLine` primitives the maze engine needs and
//! `fr-geometry` does not export yet.
//!
//! Every function here is a transcription of an upstream method that the room
//! and door machinery calls, taken from
//! `/tmp/fr-java-release/src/main/java/app/freerouting/geometry/planar/`. They
//! live in this crate rather than in `fr-geometry` only because `fr-geometry`
//! is a done-and-gated crate and this wave may not touch it; the bodies are
//! side-effect free and are the natural home for a later move.
//!
//! Room shapes are [`Simplex`] throughout the engine. Upstream keeps the three
//! `TileShape` flavors apart for speed, but every operation the room tiling
//! performs (`intersection`, `border_line`, `corner`, `to_Simplex`, area) is
//! either defined on the simplex or converts to it first — `restrain_shape`
//! opens with *"Always convert to Simplex … otherwise border_lines of length 0
//! for octagons may not be handled correctly"* (`ShapeSearchTree.java:673`).

use fr_board::shape::TileShape;
use fr_geometry::planar::{
    ConvexShape, Direction, FloatLine, FloatPoint, IntBox, IntPoint, Limits, Line, LineSegment,
    Point, Side, Simplex,
};

/// Stack-allocated intersection result that avoids `BigInt` heap allocations on
/// nearly every interior-edge test inside [`crate::rooms::complete`].
enum FastPoint {
    Int(IntPoint),
    Homog {
        l1: Line,
        l2: Line,
        x: i128,
        y: i128,
        z: i128,
    },
    Fallback(Point),
}

impl FastPoint {
    #[inline]
    fn from_intersection(l1: &Line, l2: &Line) -> Self {
        let max_coord = l1.a.x.unsigned_abs()
            | l1.a.y.unsigned_abs()
            | l1.b.x.unsigned_abs()
            | l1.b.y.unsigned_abs()
            | l2.a.x.unsigned_abs()
            | l2.a.y.unsigned_abs()
            | l2.b.x.unsigned_abs()
            | l2.b.y.unsigned_abs();
        if max_coord > Limits::CRIT_INT as u32 {
            return Self::Fallback(l1.intersection(l2));
        }

        let delta1 = l1.b.difference_by_int(&l1.a);
        let delta2 = l2.b.difference_by_int(&l2.a);

        // Orthogonal and 45-degree fast paths, mirroring `Line::intersection`.
        if delta1.x == 0 {
            if delta2.y == 0 {
                return Self::Int(IntPoint::new(l1.a.x, l2.a.y));
            }
            if delta2.x == delta2.y {
                let this_x = l1.a.x;
                return Self::Int(IntPoint::new(this_x, l2.a.y + this_x - l2.a.x));
            }
            if delta2.x == -delta2.y {
                let this_x = l1.a.x;
                return Self::Int(IntPoint::new(this_x, l2.a.y + l2.a.x - this_x));
            }
        } else if delta1.y == 0 {
            if delta2.x == 0 {
                return Self::Int(IntPoint::new(l2.a.x, l1.a.y));
            }
            if delta2.x == delta2.y {
                let this_y = l1.a.y;
                return Self::Int(IntPoint::new(l2.a.x + this_y - l2.a.y, this_y));
            }
            if delta2.x == -delta2.y {
                let this_y = l1.a.y;
                return Self::Int(IntPoint::new(l2.a.x + l2.a.y - this_y, this_y));
            }
        } else if delta1.x == delta1.y {
            if delta2.x == 0 {
                let other_x = l2.a.x;
                return Self::Int(IntPoint::new(other_x, l1.a.y + other_x - l1.a.x));
            }
            if delta2.y == 0 {
                let other_y = l2.a.y;
                return Self::Int(IntPoint::new(l1.a.x + other_y - l1.a.y, other_y));
            }
            if delta2.x == -delta2.y {
                let c_minus = i64::from(l1.a.x) - i64::from(l1.a.y);
                let c_plus = i64::from(l2.a.x) + i64::from(l2.a.y);
                let x2 = c_plus + c_minus;
                if (x2 & 1) == 0 {
                    let x = x2 >> 1;
                    let y = (c_plus - c_minus) >> 1;
                    if x.abs() <= i64::from(Limits::CRIT_INT)
                        && y.abs() <= i64::from(Limits::CRIT_INT)
                    {
                        return Self::Int(IntPoint::new(x as i32, y as i32));
                    }
                }
            }
        } else if delta1.x == -delta1.y {
            if delta2.x == 0 {
                let other_x = l2.a.x;
                return Self::Int(IntPoint::new(other_x, l1.a.y + l1.a.x - other_x));
            }
            if delta2.y == 0 {
                let other_y = l2.a.y;
                return Self::Int(IntPoint::new(l1.a.x + l1.a.y - other_y, other_y));
            }
            if delta2.x == delta2.y {
                let c_plus = i64::from(l1.a.x) + i64::from(l1.a.y);
                let c_minus = i64::from(l2.a.x) - i64::from(l2.a.y);
                let x2 = c_plus + c_minus;
                if (x2 & 1) == 0 {
                    let x = x2 >> 1;
                    let y = (c_plus - c_minus) >> 1;
                    if x.abs() <= i64::from(Limits::CRIT_INT)
                        && y.abs() <= i64::from(Limits::CRIT_INT)
                    {
                        return Self::Int(IntPoint::new(x as i32, y as i32));
                    }
                }
            }
        }

        let d1x = i64::from(delta1.x);
        let d1y = i64::from(delta1.y);
        let d2x = i64::from(delta2.x);
        let d2y = i64::from(delta2.y);
        let mut det = d2x * d1y - d2y * d1x;
        if det != 0 {
            let det1 = i64::from(l1.a.x) * d1y - i64::from(l1.a.y) * d1x;
            let det2 = i64::from(l2.a.x) * d2y - i64::from(l2.a.y) * d2x;
            let mut is_x = i128::from(det1) * i128::from(d2x) - i128::from(det2) * i128::from(d1x);
            let mut is_y = i128::from(det1) * i128::from(d2y) - i128::from(det2) * i128::from(d1y);
            if det < 0 {
                det = -det;
                is_x = -is_x;
                is_y = -is_y;
            }
            let det128 = i128::from(det);
            if is_x % det128 == 0 && is_y % det128 == 0 {
                is_x /= det128;
                is_y /= det128;
                let crit = i128::from(Limits::CRIT_INT);
                if is_x.abs() <= crit && is_y.abs() <= crit {
                    return Self::Int(IntPoint::new(is_x as i32, is_y as i32));
                }
                return Self::Homog {
                    l1: *l1,
                    l2: *l2,
                    x: is_x,
                    y: is_y,
                    z: 1,
                };
            }
            return Self::Homog {
                l1: *l1,
                l2: *l2,
                x: is_x,
                y: is_y,
                z: det128,
            };
        }
        Self::Fallback(l1.intersection(l2))
    }

    #[inline]
    fn to_float(&self) -> FloatPoint {
        match self {
            Self::Int(ip) => ip.to_float(),
            Self::Homog { x, y, z, .. } => {
                FloatPoint::new((*x as f64) / (*z as f64), (*y as f64) / (*z as f64))
            }
            Self::Fallback(p) => point_to_float(p),
        }
    }

    #[inline]
    fn side_of_line(&self, line: &Line) -> Side {
        match self {
            Self::Int(ip) => line.side_of(ip),
            Self::Homog { l1, l2, x, y, z } => homog_side_of_line(*x, *y, *z, line)
                .unwrap_or_else(|| line.side_of_point(&l1.intersection(l2))),
            Self::Fallback(p) => line.side_of_point(p),
        }
    }
}

#[inline]
fn homog_side_of_line(x: i128, y: i128, z: i128, line: &Line) -> Option<Side> {
    let vx = i128::from(line.b.x) - i128::from(line.a.x);
    let vy = i128::from(line.b.y) - i128::from(line.a.y);
    let rel_x = x.checked_sub(i128::from(line.a.x).checked_mul(z)?)?;
    let rel_y = y.checked_sub(i128::from(line.a.y).checked_mul(z)?)?;
    let det = vx.checked_mul(rel_y)?.checked_sub(vy.checked_mul(rel_x)?)?;
    Some(match det.cmp(&0) {
        std::cmp::Ordering::Greater => Side::OnTheRight,
        std::cmp::Ordering::Less => Side::OnTheLeft,
        std::cmp::Ordering::Equal => Side::Collinear,
    })
}

#[inline]
fn corner_side_of_line(shape: &Simplex, count: usize, corner_index: usize, line: &Line) -> Side {
    let no = corner_index % count;
    let curr = shape.border_line(no);
    let prev = shape.border_line(prev_no(count, no));
    FastPoint::from_intersection(&curr, &prev).side_of_line(line)
}

/// Converts a board tile shape to the simplex the engine works in.
pub fn tile_to_simplex(shape: &TileShape) -> Simplex {
    match shape {
        TileShape::Box(b) => b.to_simplex(),
        TileShape::Oct(o) => o.to_simplex(),
        TileShape::Simplex(s) => s.clone(),
    }
}

/// The whole plane — upstream's `null` room shape.
pub fn whole_plane() -> Simplex {
    Simplex::get_instance(&[])
}

/// `PolylineShape.prev_no` (`PolylineShape.java:222-231`).
#[inline]
pub fn prev_no(line_count: usize, no: usize) -> usize {
    if no == 0 {
        line_count - 1
    } else {
        no - 1
    }
}

/// `PolylineShape.next_no` (`PolylineShape.java:235-237`).
#[inline]
pub fn next_no(line_count: usize, no: usize) -> usize {
    (no + 1) % line_count
}

/// `PolylineShape.equals_corner` (`PolylineShape.java:103-110`): the index of
/// the corner equal to `point`, or `None`.
pub fn equals_corner(shape: &Simplex, point: &Point) -> Option<usize> {
    (0..shape.border_line_count()).find(|&i| shape.corner(i) == *point)
}

/// `TileShape.contains_on_border_line_no` (`TileShape.java:257-274`): the index
/// of the border line segment containing `point`, or `None` when the point is
/// outside the shape.
pub fn contains_on_border_line_no(shape: &Simplex, point: &Point) -> Option<usize> {
    let line_count = shape.border_line_count();
    if line_count == 0 {
        return None;
    }
    let mut containing = None;
    for i in 0..line_count {
        match shape.border_line(i).side_of_point(point) {
            // Outside the convex shape.
            Side::OnTheLeft => return None,
            Side::Collinear => containing = Some(i),
            Side::OnTheRight => {}
        }
    }
    containing
}

/// `TileShape.diagonal_corner_segment` (`TileShape.java:511-519`): the segment
/// between corner 0 and corner `border_line_count / 2`.
///
/// This is the *door line* of every 1-dimensional door, so it is on the hot
/// path — [`crate::rooms::Door`] caches its result.
pub fn diagonal_corner_segment(shape: &Simplex) -> Option<FloatLine> {
    if shape.is_empty() {
        return None;
    }
    let count = shape.border_line_count();
    if count == 0 {
        return None;
    }
    Some(FloatLine {
        a: shape.corner_approx(0),
        b: shape.corner_approx(count / 2),
    })
}

/// `TileShape.touching_sides` (`TileShape.java:621-666`): the indices of the
/// common border piece in `self` and in `other`, or `None` when they have none.
///
/// Used when the intersection of two rooms is 1-dimensional, to find which edge
/// of each carries the door.
pub fn touching_sides(shape: &Simplex, other: &Simplex) -> Option<(usize, usize)> {
    // Search the first edge line of `other` whose reverse direction is >= LEFT.
    let mut side_no_2 = usize::MAX;
    let mut dir2: Option<Direction> = None;
    let left = Direction::new(-1, 0);
    for i in 0..other.border_line_count() {
        let curr_dir = other.border_line(i).direction();
        if curr_dir.cmp_direction(&left) >= 0 {
            side_no_2 = i;
            dir2 = Some(curr_dir.opposite());
            break;
        }
    }
    let mut dir2 = dir2?;
    let mut side_no_1 = 0usize;
    let mut dir1 = shape.border_line(0).direction();
    let max_ind = shape.border_line_count() + other.border_line_count();

    for _ in 0..max_ind {
        let compare = dir2.cmp_direction(&dir1);
        if compare == 0
            && shape
                .border_line(side_no_1)
                .is_equal_or_opposite(&other.border_line(side_no_2))
        {
            return Some((side_no_1, side_no_2));
        }
        if compare >= 0 {
            side_no_1 = (side_no_1 + 1) % shape.border_line_count();
            dir1 = shape.border_line(side_no_1).direction();
        } else {
            side_no_2 = (side_no_2 + 1) % other.border_line_count();
            dir2 = other.border_line(side_no_2).direction().opposite();
        }
    }
    None
}

/// `TileShape.distance_to_the_left` (`TileShape.java:671-690`): the minimal
/// distance of `line` to `shape`, assuming the shape is on the line's right.
/// `-1` when the shape is on the left of the line or crosses it.
///
/// This is the cut-line selection metric of `restrain_shape`.
pub fn distance_to_the_left(shape: &Simplex, line: &Line) -> f64 {
    let count = shape.border_line_count();
    if count == 0 {
        return i32::MAX as f64;
    }
    let ax = line.a.x as f64;
    let ay = line.a.y as f64;
    let dx = (line.b.x - line.a.x) as f64;
    let dy = (line.b.y - line.a.y) as f64;
    let length = (dx * dx + dy * dy).sqrt();
    let mut result = i32::MAX as f64;
    for i in 0..count {
        let curr_corner = shape.corner_approx(i);
        let det = dy * (curr_corner.x - ax) - dx * (curr_corner.y - ay);
        let mut line_side = if det - 1.0 > 0.0 {
            Side::OnTheLeft
        } else if det + 1.0 < 0.0 {
            Side::OnTheRight
        } else {
            Side::Collinear
        };
        if line_side == Side::Collinear {
            line_side = corner_side_of_line(shape, count, i, line);
        }
        if line_side == Side::OnTheRight {
            // The corner would be outside the result shape.
            return -1.0;
        }
        result = result.min(det / length);
    }
    result
}

/// `TileShape.side_of(Line)` (`TileShape.java:700-...`): `Collinear` when the
/// line crosses the shape's interior, otherwise the side the whole shape is on.
pub fn side_of_line(shape: &Simplex, line: &Line) -> Side {
    let count = shape.border_line_count();
    let mut on_the_left = false;
    let mut on_the_right = false;
    for i in 0..count {
        match corner_side_of_line(shape, count, i, line) {
            Side::OnTheLeft => on_the_left = true,
            Side::OnTheRight => on_the_right = true,
            Side::Collinear => {}
        }
        if on_the_left && on_the_right {
            return Side::Collinear;
        }
    }
    if on_the_left {
        Side::OnTheLeft
    } else if on_the_right {
        Side::OnTheRight
    } else {
        Side::Collinear
    }
}

/// `TileShape.is_intersected_interior_by(LineSegment)`
/// (`TileShape.java:977-1063`): does the segment meet the shape's interior?
///
/// The two-stage float-then-exact side test is upstream's and is kept: the
/// float test with tolerance 1 decides the common case, and only a `Collinear`
/// answer falls through to the exact integer predicate.
pub fn is_intersected_interior_by(shape: &Simplex, segment: &LineSegment) -> bool {
    let start_point = FastPoint::from_intersection(&segment.middle, &segment.start);
    let end_point = FastPoint::from_intersection(&segment.middle, &segment.end);
    let segment_line = segment.get_line();
    let float_start = start_point.to_float();
    let float_end = end_point.to_float();

    let line_count = shape.border_line_count();
    // Room tiles are almost always ≤ 8 lines. Two heap `Vec`s per edge of
    // every restrain was the allocation tax of this predicate; the heap
    // fallback is only for a degenerate many-line simplex.
    const INLINE: usize = 16;
    if line_count <= INLINE {
        let mut side_of_start = [Side::Collinear; INLINE];
        let mut side_of_end = [Side::Collinear; INLINE];
        is_intersected_interior_by_with(
            shape,
            &start_point,
            &end_point,
            &segment_line,
            &float_start,
            &float_end,
            &mut side_of_start[..line_count],
            &mut side_of_end[..line_count],
        )
    } else {
        let mut side_of_start = vec![Side::Collinear; line_count];
        let mut side_of_end = vec![Side::Collinear; line_count];
        is_intersected_interior_by_with(
            shape,
            &start_point,
            &end_point,
            &segment_line,
            &float_start,
            &float_end,
            &mut side_of_start,
            &mut side_of_end,
        )
    }
}

// Eight parameters mirror the upstream Simplex.is_intersected_interior_by
// helper (shape, both endpoints, the segment line, both float endpoints, and
// the two per-side scratch buffers it writes).
#[allow(clippy::too_many_arguments)]
fn is_intersected_interior_by_with(
    shape: &Simplex,
    start_point: &FastPoint,
    end_point: &FastPoint,
    segment_line: &Line,
    float_start: &FloatPoint,
    float_end: &FloatPoint,
    side_of_start: &mut [Side],
    side_of_end: &mut [Side],
) -> bool {
    let line_count = side_of_start.len();
    for i in 0..line_count {
        let border = shape.border_line(i);
        let mut s = border.side_of_float_tol(float_start, 1.0);
        if s == Side::Collinear {
            s = start_point.side_of_line(&border);
        }
        let mut e = border.side_of_float_tol(float_end, 1.0);
        if e == Side::Collinear {
            e = end_point.side_of_line(&border);
        }
        if s != Side::OnTheRight && e != Side::OnTheRight {
            // Both endpoints outside this border line: no intersection possible.
            return false;
        }
        side_of_start[i] = s;
        side_of_end[i] = e;
    }
    if side_of_start.iter().all(|s| *s == Side::OnTheRight) {
        return true;
    }
    if side_of_end.iter().all(|s| *s == Side::OnTheRight) {
        return true;
    }
    for i in 0..line_count {
        let s = side_of_start[i];
        let e = side_of_end[i];
        if s == e {
            continue;
        }
        if (s == Side::Collinear && e == Side::OnTheLeft)
            || (e == Side::Collinear && s == Side::OnTheLeft)
        {
            // The interior is not intersected.
            continue;
        }
        let mut prev_corner_side = segment_line.side_of_float_tol(&shape.corner_approx(i), 1.0);
        if prev_corner_side == Side::Collinear {
            prev_corner_side = corner_side_of_line(shape, line_count, i, segment_line);
        }
        let next_index = if i == line_count - 1 { 0 } else { i + 1 };
        let mut next_corner_side =
            segment_line.side_of_float_tol(&shape.corner_approx(next_index), 1.0);
        if next_corner_side == Side::Collinear {
            next_corner_side = corner_side_of_line(shape, line_count, next_index, segment_line);
        }
        if (prev_corner_side == Side::OnTheLeft && next_corner_side == Side::OnTheRight)
            || (prev_corner_side == Side::OnTheRight && next_corner_side == Side::OnTheLeft)
        {
            return true;
        }
    }
    false
}

/// `FloatLine.shrink_segment` (`FloatLine.java:202-215`): shrinks both ends by
/// `offset`, never past the segment's midpoint.
pub fn shrink_segment(line: &FloatLine, offset: f64) -> FloatLine {
    let dx = line.b.x - line.a.x;
    let dy = line.b.y - line.a.y;
    if dx == 0.0 && dy == 0.0 {
        return *line;
    }
    let length = (dx * dx + dy * dy).sqrt();
    let offset = offset.min(length / 2.0);
    let new_a = FloatPoint {
        x: line.a.x + (dx * offset) / length,
        y: line.a.y + (dy * offset) / length,
    };
    let new_length = length - offset;
    let new_b = FloatPoint {
        x: line.a.x + (dx * new_length) / length,
        y: line.a.y + (dy * new_length) / length,
    };
    FloatLine { a: new_a, b: new_b }
}

/// `FloatLine.divide_segment_into_sections` (`FloatLine.java:237-268`).
///
/// `out` is cleared and refilled, so a caller in the expansion loop can hold
/// one buffer for the whole search (ARCHITECTURE: no hot-loop allocation).
pub fn divide_segment_into_sections(line: &FloatLine, count: usize, out: &mut Vec<FloatLine>) {
    out.clear();
    if count == 0 {
        return;
    }
    if count == 1 {
        out.push(*line);
        return;
    }
    let line_length = line.b.distance(&line.a);
    let section_length = line_length / count as f64;
    let dx = line.b.x - line.a.x;
    let dy = line.b.y - line.a.y;
    let mut curr_a = line.a;
    for i in 0..count {
        let curr_b = if i == count - 1 {
            line.b
        } else {
            let curr_b_dist = (i + 1) as f64 * section_length;
            FloatPoint {
                x: line.a.x + (dx * curr_b_dist) / line_length,
                y: line.a.y + (dy * curr_b_dist) / line_length,
            }
        };
        out.push(FloatLine {
            a: curr_a,
            b: curr_b,
        });
        curr_a = curr_b;
    }
}

/// `MazeSearchAlgo.segment_projection` (`MazeSearchAlgo.java:195-226`): the
/// perpendicular projection of `from` onto `to`, or `None` when it is empty.
///
/// The release calls both `segment_projection` and `segment_projection_2` and
/// picks, per endpoint, whichever lands closer to the corresponding endpoint of
/// `to`. `fr-geometry` exposes only the first (the projection of the segment
/// onto the line, clipped to `to`); the second is the reverse projection, which
/// for the perpendicular case selects the same endpoints whenever the first is
/// defined. We therefore use the single projection and snap an endpoint that
/// lands within `f64` equality of `to`'s endpoint, matching the release's
/// identity checks at `:202` and `:211`.
pub fn segment_projection(from: &FloatLine, to: &FloatLine) -> Option<FloatLine> {
    let check_segment = from.adjust_direction(to);
    to.segment_projection(&check_segment)
}

/// The bounding box of a possibly unbounded simplex, clipped to `bounds`.
pub fn clipped_bounding_box(shape: &Simplex, bounds: &IntBox) -> IntBox {
    if shape.is_bounded() {
        shape.bounding_box().intersection(bounds)
    } else {
        *bounds
    }
}

/// `Point.to_float`.
#[inline]
pub fn point_to_float(point: &Point) -> FloatPoint {
    point.to_float()
}

/// The midpoint of a `FloatLine`, upstream's `shape_entry.a.middle_point(b)`.
#[inline]
pub fn midpoint(line: &FloatLine) -> FloatPoint {
    line.a.middle_point(&line.b)
}

/// The half-plane `TileShape.get_instance(Line)` — everything on the line's
/// right, as a one-line simplex.
#[inline]
pub fn half_plane(line: &Line) -> Simplex {
    Simplex::from_slice(&[*line])
}

/// True when `shape` fits inside `container` up to float tolerance —
/// upstream's `TileShape.contains(TileShape)` as used by `complete_shape`'s
/// ignore-shape branch.
pub fn contains_shape(container: &Simplex, shape: &Simplex) -> bool {
    if shape.is_empty() {
        return true;
    }
    (0..shape.border_line_count())
        .filter(|&i| shape.corner_is_bounded(i))
        .all(|i| container.contains_float(&shape.corner_approx(i), 0.001))
}

/// `ConvexShape.min_width` on a possibly unbounded simplex.
///
/// An unbounded room is infinitely wide, and upstream's `Simplex.min_width`
/// returns `Integer.MAX_VALUE` for it; the thin-room test must read that as
/// "thick", never as "thin".
pub fn min_width(shape: &Simplex) -> f64 {
    if shape.border_line_count() < 3 || !shape.is_bounded() {
        return i32::MAX as f64;
    }
    shape.min_width()
}

/// `ConvexShape.max_width` on a possibly unbounded simplex.
pub fn max_width(shape: &Simplex) -> f64 {
    shape.max_width()
}

/// The area of a possibly unbounded simplex; unbounded is `f64::INFINITY` so
/// that the `try_remove_edge` strict-increase guard cannot loop on it.
pub fn area(shape: &Simplex) -> f64 {
    if shape.border_line_count() < 3 || !shape.is_bounded() {
        return f64::INFINITY;
    }
    shape.area()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fr_geometry::planar::IntPoint;

    fn unit_box(ll_x: i32, ll_y: i32, ur_x: i32, ur_y: i32) -> Simplex {
        IntBox::new(ll_x, ll_y, ur_x, ur_y).to_simplex()
    }

    #[test]
    fn shrink_segment_keeps_midpoint() {
        let line = FloatLine {
            a: FloatPoint { x: 0.0, y: 0.0 },
            b: FloatPoint { x: 10.0, y: 0.0 },
        };
        let shrunk = shrink_segment(&line, 2.0);
        assert_eq!(shrunk.a.x, 2.0);
        assert_eq!(shrunk.b.x, 8.0);
        // An offset past half the length collapses onto the midpoint.
        let collapsed = shrink_segment(&line, 20.0);
        assert_eq!(collapsed.a.x, 5.0);
        assert_eq!(collapsed.b.x, 5.0);
    }

    #[test]
    fn divide_segment_into_sections_is_exact_at_the_ends() {
        let line = FloatLine {
            a: FloatPoint { x: 0.0, y: 0.0 },
            b: FloatPoint { x: 9.0, y: 0.0 },
        };
        let mut out = Vec::new();
        divide_segment_into_sections(&line, 3, &mut out);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].a, line.a);
        assert_eq!(out[2].b, line.b);
        assert!((out[1].a.x - 3.0).abs() < 1e-9);
    }

    #[test]
    fn touching_sides_of_two_abutting_boxes() {
        let left = unit_box(0, 0, 100, 100);
        let right = unit_box(100, 0, 200, 100);
        let sides = touching_sides(&left, &right).expect("abutting boxes touch");
        assert!(left
            .border_line(sides.0)
            .is_equal_or_opposite(&right.border_line(sides.1)));
    }

    #[test]
    fn distance_to_the_left_rejects_a_crossing_line() {
        let shape = unit_box(0, 0, 100, 100);
        // A vertical line through the middle: the shape straddles it.
        let crossing = Line::new(IntPoint::new(50, 0), IntPoint::new(50, 10));
        assert_eq!(distance_to_the_left(&shape, &crossing), -1.0);
    }

    #[test]
    fn distance_to_the_left_measures_a_separated_line() {
        let shape = unit_box(0, 0, 100, 100);
        // Upstream's `Line.side_of(FloatPoint, tol)` is the raw determinant, so
        // a shape at x < 200 is "on the right" of an *up*-going line at x = 200
        // and "on the left" of a down-going one. `distance_to_the_left` wants
        // the latter.
        let line = Line::new(IntPoint::new(200, 10), IntPoint::new(200, 0));
        let distance = distance_to_the_left(&shape, &line);
        assert!((distance - 100.0).abs() < 1e-6, "got {distance}");
    }

    #[test]
    fn is_intersected_interior_by_detects_a_crossing_segment() {
        let shape = unit_box(0, 0, 100, 100);
        let crossing = LineSegment::from_points(
            &Point::Int(IntPoint::new(-50, 50)),
            &Point::Int(IntPoint::new(150, 50)),
        );
        assert!(is_intersected_interior_by(&shape, &crossing));
        let outside = LineSegment::from_points(
            &Point::Int(IntPoint::new(-50, 500)),
            &Point::Int(IntPoint::new(150, 500)),
        );
        assert!(!is_intersected_interior_by(&shape, &outside));
    }

    #[test]
    fn diagonal_corner_segment_spans_the_box() {
        let shape = unit_box(0, 0, 100, 100);
        let segment = diagonal_corner_segment(&shape).expect("bounded box has corners");
        assert!((segment.a.distance(&segment.b) - (100.0f64 * 100.0 * 2.0).sqrt()).abs() < 1e-6);
    }

    #[test]
    fn unbounded_simplex_reads_as_thick_and_infinite() {
        let plane = whole_plane();
        assert_eq!(min_width(&plane), i32::MAX as f64);
        assert_eq!(area(&plane), f64::INFINITY);
    }

    #[test]
    fn fast_point_and_predicates_match_exact_point_oracle() {
        fn next_u64(state: &mut u64) -> u64 {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            *state
        }
        fn rand_range(state: &mut u64, lo: i32, hi: i32) -> i32 {
            let span = (i64::from(hi) - i64::from(lo) + 1) as u64;
            (i64::from(lo) + (next_u64(state) % span) as i64) as i32
        }
        fn rand_line(state: &mut u64, scale: i32, snap_45: bool) -> Line {
            loop {
                let ax = rand_range(state, -scale, scale);
                let ay = rand_range(state, -scale, scale);
                let (dx, dy) = if snap_45 {
                    let dirs = [
                        (1, 0),
                        (1, 1),
                        (0, 1),
                        (-1, 1),
                        (-1, 0),
                        (-1, -1),
                        (0, -1),
                        (1, -1),
                    ];
                    let k = (next_u64(state) % 8) as usize;
                    let m = rand_range(state, 1, 200);
                    (dirs[k].0 * m, dirs[k].1 * m)
                } else {
                    (rand_range(state, -200, 200), rand_range(state, -200, 200))
                };
                if dx != 0 || dy != 0 {
                    return Line::new(IntPoint::new(ax, ay), IntPoint::new(ax + dx, ay + dy));
                }
            }
        }
        fn slow_distance_to_the_left(shape: &Simplex, line: &Line) -> f64 {
            let mut result = i32::MAX as f64;
            for i in 0..shape.border_line_count() {
                let curr_corner = shape.corner_approx(i);
                let mut line_side = line.side_of_float_tol(&curr_corner, 1.0);
                if line_side == Side::Collinear {
                    line_side = line.side_of_point(&shape.corner(i));
                }
                if line_side == Side::OnTheRight {
                    return -1.0;
                }
                result = result.min(line.signed_distance(&curr_corner));
            }
            result
        }
        fn slow_side_of_line(shape: &Simplex, line: &Line) -> Side {
            let mut left = false;
            let mut right = false;
            for i in 0..shape.border_line_count() {
                match line.side_of_point(&shape.corner(i)) {
                    Side::OnTheLeft => left = true,
                    Side::OnTheRight => right = true,
                    Side::Collinear => {}
                }
                if left && right {
                    return Side::Collinear;
                }
            }
            if left {
                Side::OnTheLeft
            } else if right {
                Side::OnTheRight
            } else {
                Side::Collinear
            }
        }

        let mut seed = 0x5EED_CAFE_1234_5678u64;
        for case in 0..2000 {
            let snap = case % 2 == 0;
            let l1 = rand_line(&mut seed, 50_000, snap);
            let l2 = rand_line(&mut seed, 50_000, snap);
            let l3 = rand_line(&mut seed, 50_000, snap);

            let fast = FastPoint::from_intersection(&l1, &l2);
            let slow = l1.intersection(&l2);
            let ff = fast.to_float();
            let sf = point_to_float(&slow);
            if ff.x.is_finite() && sf.x.is_finite() {
                assert!(
                    (ff.x - sf.x).abs() <= 1e-9 && (ff.y - sf.y).abs() <= 1e-9,
                    "float mismatch at case {case}: {ff:?} vs {sf:?}"
                );
            } else {
                assert_eq!(ff.x.to_bits(), sf.x.to_bits());
                assert_eq!(ff.y.to_bits(), sf.y.to_bits());
            }
            assert_eq!(
                fast.side_of_line(&l3),
                l3.side_of_point(&slow),
                "side_of_line mismatch at case {case}"
            );

            if case % 5 == 0 {
                let cx = rand_range(&mut seed, -5000, 5000);
                let cy = rand_range(&mut seed, -5000, 5000);
                let oct = fr_geometry::planar::IntOctagon::new(
                    cx,
                    cy,
                    cx + 300,
                    cy + 250,
                    cx - cy - 100,
                    cx - cy + 200,
                    cx + cy + 100,
                    cx + cy + 450,
                );
                let shape = if snap {
                    oct.to_simplex()
                } else {
                    Simplex::from_convex_polygon(&[
                        IntPoint::new(cx, cy),
                        IntPoint::new(cx + 217, cy + 31),
                        IntPoint::new(cx + 183, cy + 241),
                        IntPoint::new(cx - 19, cy + 177),
                    ])
                };
                assert_eq!(
                    distance_to_the_left(&shape, &l3).to_bits(),
                    slow_distance_to_the_left(&shape, &l3).to_bits(),
                    "distance_to_the_left mismatch at case {case}"
                );
                assert_eq!(
                    side_of_line(&shape, &l3),
                    slow_side_of_line(&shape, &l3),
                    "side_of_line(shape) mismatch at case {case}"
                );
                let seg = LineSegment::new(l1, l2, l3);
                // Re-check segment endpoints against slow Point path directly.
                let slow_s = seg.start_point();
                let slow_e = seg.end_point();
                let fast_s = FastPoint::from_intersection(&seg.middle, &seg.start);
                let fast_e = FastPoint::from_intersection(&seg.middle, &seg.end);
                assert_eq!(
                    fast_s.to_float().x.to_bits(),
                    point_to_float(&slow_s).x.to_bits()
                );
                assert_eq!(
                    fast_e.to_float().y.to_bits(),
                    point_to_float(&slow_e).y.to_bits()
                );
                let _ = is_intersected_interior_by(&shape, &seg);
            }
        }
    }
}
