//! The geometry predicates the shove stack needs that
//! [`fr_geometry`](fr_geometry::planar) does not expose yet, transcribed from
//! `geometry/planar/{TileShape,IntBox,IntOctagon}.java` in the 2.3.0 release
//! tree, plus the clearance-aware tree query
//! (`ShapeSearchTree.overlapping_tree_entries_with_clearance`, `:430-480`).
//!
//! They live here rather than in `fr-geometry` because they are only ever
//! reached from a shove, and because `overlapping_items_with_clearance` needs
//! the board's clearance matrix, which the geometry crate does not know about.

use fr_geometry::planar::{
    FloatPoint, IntBox, IntOctagon, IntPoint, Line, OctR2, Point, Side, Simplex,
};
use fr_geometry::Area;

use crate::board::Board;
use crate::clearance::ClearanceSource;
use crate::item::ItemId;
use crate::shape::TileShape;

/// `TileShape.to_Simplex()` — the shove stack works on one shape flavor.
pub fn tile_to_simplex(shape: &TileShape) -> Simplex {
    match shape {
        TileShape::Box(b) => b.to_simplex(),
        TileShape::Oct(o) => o.to_simplex(),
        TileShape::Simplex(s) => s.clone(),
    }
}

/// `ConvexShape.is_contained_in(IntBox)` for the board bounding box.
///
/// An unbounded simplex is never contained; that is upstream's answer too,
/// because its corners come back as the `Limits::CRIT_INT` sentinel.
pub fn simplex_contained_in(shape: &Simplex, bounds: &IntBox) -> bool {
    if shape.is_empty() {
        // Upstream warns and answers "ok" for an empty check shape
        // (`ShoveTraceAlgo.java:172-175`).
        return true;
    }
    if !shape.is_bounded() {
        return false;
    }
    let shape_box = shape.bounding_box();
    !shape_box.is_empty() && shape_box.is_contained_in(bounds)
}

/// `TileShape.contains_on_border_line_no(Point)` (`TileShape.java`): the index
/// of the border line the point lies *on*, or `None` when the point is outside
/// or strictly inside.
pub fn contains_on_border_line_no(shape: &Simplex, point: &Point) -> Option<usize> {
    let line_count = shape.border_line_count();
    if line_count == 0 {
        return None;
    }
    let mut containing = None;
    for i in 0..line_count {
        match shape.border_line(i).side_of_point(point) {
            Side::OnTheLeft => return None,
            Side::Collinear => containing = Some(i),
            Side::OnTheRight => {}
        }
    }
    containing
}

/// `TileShape.nearest_border_point(Point)`: the nearest corner, improved by any
/// perpendicular border projection that actually lands on its own edge.
pub fn nearest_border_point(shape: &Simplex, from_point: &Point) -> Option<Point> {
    let line_count = shape.border_line_count();
    if line_count == 0 {
        return None;
    }
    let from_point_f = crate::shove::geom::point_to_float(from_point);
    if line_count == 1 {
        return Some(shape.border_line(0).perpendicular_projection(from_point));
    }
    let mut min_dist = f64::MAX;
    let mut min_dist_ind = 0usize;
    for i in 0..line_count {
        let curr_dist = shape.corner_approx(i).distance_square(&from_point_f);
        if curr_dist < min_dist {
            min_dist = curr_dist;
            min_dist_ind = i;
        }
    }
    let mut nearest_point = shape.corner(min_dist_ind);

    let mut prev_ind = line_count - 2;
    let mut curr_ind = line_count - 1;
    for next_ind in 0..line_count {
        let projection = shape
            .border_line(curr_ind)
            .perpendicular_projection(from_point);
        let prev_ok = !shape.corner_is_bounded(curr_ind)
            || shape.border_line(prev_ind).side_of_point(&projection) == Side::OnTheRight;
        let next_ok = !shape.corner_is_bounded(next_ind)
            || shape.border_line(next_ind).side_of_point(&projection) == Side::OnTheRight;
        if prev_ok && next_ok {
            let curr_dist = point_to_float(&projection).distance_square(&from_point_f);
            if curr_dist < min_dist {
                min_dist = curr_dist;
                nearest_point = projection;
            }
        }
        prev_ind = curr_ind;
        curr_ind = next_ind;
    }
    Some(nearest_point)
}

/// `Point.to_float()`.
pub fn point_to_float(point: &Point) -> FloatPoint {
    match point {
        Point::Int(p) => p.to_float(),
        Point::Rational(r) => r.to_float(),
    }
}

/// `TileShape.get_instance(Line)` — the half plane right of `line`.
pub fn half_plane(line: &Line) -> Simplex {
    Simplex::from_slice(&[*line])
}

/// `IntBox.nearest_border_projections` (`IntBox.java`), capped at two results.
pub fn box_nearest_border_projections(
    shape: &IntBox,
    point: &IntPoint,
    max_result_points: usize,
) -> Vec<IntPoint> {
    if max_result_points == 0 {
        return Vec::new();
    }
    let count = max_result_points.min(2);
    let lower_x_diff = point.x - shape.ll.x;
    let upper_x_diff = shape.ur.x - point.x;
    let lower_y_diff = point.y - shape.ll.y;
    let upper_y_diff = shape.ur.y - point.y;

    let mut nearest = (point.x, point.y);
    let mut second = (point.x, point.y);
    let (mut min_diff, mut second_min_diff);
    if lower_x_diff <= upper_x_diff {
        min_diff = lower_x_diff;
        second_min_diff = upper_x_diff;
        nearest.0 = shape.ll.x;
        second.0 = shape.ur.x;
    } else {
        min_diff = upper_x_diff;
        second_min_diff = lower_x_diff;
        nearest.0 = shape.ur.x;
        second.0 = shape.ll.x;
    }
    if lower_y_diff < min_diff {
        second_min_diff = min_diff;
        min_diff = lower_y_diff;
        second = nearest;
        nearest = (point.x, shape.ll.y);
    } else if lower_y_diff < second_min_diff {
        second_min_diff = lower_y_diff;
        second = (point.x, shape.ll.y);
    }
    if upper_y_diff < min_diff {
        second = nearest;
        nearest = (point.x, shape.ur.y);
    } else if upper_y_diff < second_min_diff {
        second = (point.x, shape.ur.y);
    }
    let mut result = Vec::with_capacity(count);
    result.push(IntPoint::new(nearest.0, nearest.1));
    if count > 1 {
        result.push(IntPoint::new(second.0, second.1));
    }
    result
}

/// `IntOctagon.border_point(IntPoint, FortyfiveDegreeDirection)` for the eight
/// 45° directions, in upstream's enum order.
fn octagon_border_point(shape: &IntOctagon, point: &IntPoint, dir: usize) -> IntPoint {
    let (x, y) = (point.x, point.y);
    let (rx, ly, lx, uy) = (shape.right_x, shape.bottom_y, shape.left_x, shape.top_y);
    let ulx = shape.upper_left_diagonal_x;
    let lrx = shape.lower_right_diagonal_x;
    let llx = shape.lower_left_diagonal_x;
    let urx = shape.upper_right_diagonal_x;
    match dir {
        // RIGHT
        0 => IntPoint::new(rx.min(urx - y).min(lrx + y), y),
        // RIGHT45
        1 => {
            let mut result_x = (0.5 * (x - y + urx) as f64).ceil() as i32;
            result_x = result_x.min(rx).min(x - y + uy);
            IntPoint::new(result_x, y - x + result_x)
        }
        // UP
        2 => IntPoint::new(x, uy.min(x - ulx).min(urx - x)),
        // UP45
        3 => {
            let mut result_x = (0.5 * (x + y + ulx) as f64).floor() as i32;
            result_x = result_x.max(lx).max(x + y - uy);
            IntPoint::new(result_x, y + x - result_x)
        }
        // LEFT
        4 => IntPoint::new(lx.max(ulx + y).max(llx - y), y),
        // LEFT45
        5 => {
            let mut result_x = (0.5 * (x - y + llx) as f64).floor() as i32;
            result_x = result_x.max(lx).max(x - y + ly);
            IntPoint::new(result_x, y - x + result_x)
        }
        // DOWN
        6 => IntPoint::new(x, ly.max(llx - x).max(x - lrx)),
        // DOWN45
        _ => {
            let mut result_x = (0.5 * (x + y + lrx) as f64).ceil() as i32;
            result_x = result_x.min(rx).min(x + y - ly);
            IntPoint::new(result_x, y + x - result_x)
        }
    }
}

/// `IntOctagon.nearest_border_projections`: the eight 45° border points sorted
/// by distance, truncated to `max_result_points`.
pub fn octagon_nearest_border_projections(
    shape: &IntOctagon,
    point: &IntPoint,
    max_result_points: usize,
) -> Vec<IntPoint> {
    if max_result_points == 0 || !shape.contains_point(&Point::Int(*point)) {
        return Vec::new();
    }
    let count = max_result_points.min(8);
    let inside = point.to_float();
    let mut candidates: Vec<(f64, IntPoint)> = Vec::with_capacity(8);
    for dir in 0..8 {
        let border_point = octagon_border_point(shape, point, dir);
        candidates.push((
            inside.distance_square(&border_point.to_float()),
            border_point,
        ));
    }
    // Stable so that equal distances keep upstream's direction order.
    candidates.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    candidates.truncate(count);
    candidates.into_iter().map(|(_, p)| p).collect()
}

/// `TileShape.nearest_relative_outside_locations(TileShape, int)`: for each
/// border line, the translation that pushes `other` out over it, keeping the
/// `count` smallest.
pub fn nearest_relative_outside_locations(
    shape: &Simplex,
    other: &Simplex,
    count: usize,
) -> Vec<FloatPoint> {
    let line_count = shape.border_line_count();
    if count == 0 || line_count < 3 || !shape.intersects(other) {
        return Vec::new();
    }
    let result_count = count.min(line_count);
    let mut translate_coors = vec![FloatPoint::ZERO; result_count];
    let mut min_dists = vec![f64::MAX; result_count];
    let other_line_count = other.border_line_count();

    let mut curr_ind = line_count - 1;
    for next_ind in 0..line_count {
        let mut curr_max_dist = 0.0f64;
        let mut curr_translate_coor = FloatPoint::ZERO;
        let border_line = shape.border_line(curr_ind);
        for corner_no in 0..other_line_count {
            let curr_corner = other.corner_approx(corner_no);
            if border_line.side_of_float(&curr_corner) == Side::OnTheRight {
                let projection = curr_corner.projection_approx(&border_line);
                let curr_dist = projection.distance_square(&curr_corner);
                if curr_dist > curr_max_dist {
                    curr_max_dist = curr_dist;
                    curr_translate_coor = projection.subtract(&curr_corner);
                }
            }
        }
        for j in 0..result_count {
            if curr_max_dist < min_dists[j] {
                for k in (j + 1..result_count).rev() {
                    min_dists[k] = min_dists[k - 1];
                    translate_coors[k] = translate_coors[k - 1];
                }
                min_dists[j] = curr_max_dist;
                translate_coors[j] = curr_translate_coor;
                break;
            }
        }
        curr_ind = next_ind;
    }
    translate_coors
}

/// `ShapeSearchTree.overlapping_items_with_clearance(shape, layer, ignore_nets,
/// cl_class)` (`:430-480, :510-521`).
///
/// The compensated branch is the plain overlap query on the class's tree; the
/// uncompensated branch — the one the headless router actually takes, because
/// `SearchTreeManager.clearance_compensation_used` starts `false` and only the
/// GUI ever flips it — enlarges *both* shapes by half the pairwise clearance,
/// "to create symmetry".
///
/// `out` comes back in descending item id, which is upstream's `TreeSet<Item>`
/// order and therefore the order every downstream tie-break depends on.
pub fn overlapping_items_with_clearance_tile(
    board: &Board,
    shape: &TileShape,
    layer: i32,
    ignore_nets: &[i32],
    cl_class: i32,
    out: &mut Vec<ItemId>,
) {
    out.clear();
    let compensated = board.trees().clearance_compensation_used();
    if compensated {
        board.overlapping_items(shape, Some(layer), ignore_nets, out);
        return;
    }

    let max_clearance = max_clearance_of(board, cl_class, layer);
    if max_clearance == 0 {
        board.overlapping_items(shape, Some(layer), ignore_nets, out);
        return;
    }
    let reach = (1.2 * max_clearance as f64).ceil() as i32 + 4;
    let broad_bounds = match shape {
        TileShape::Box(b) => TileShape::Box(b.offset(f64::from(reach))).bounds(),
        TileShape::Oct(o) => TileShape::Oct(o.offset(f64::from(reach))).bounds(),
        TileShape::Simplex(s) => match s.bounding_octagon() {
            Some(oct) => TileShape::Oct(oct.offset(f64::from(reach))).bounds(),
            None => TileShape::Simplex(s.offset(f64::from(reach))).bounds(),
        },
    };
    board.overlapping_items_bounds_only(&broad_bounds, Some(layer), ignore_nets, out);

    let tree = board.default_tree();
    let shape_bbox = shape.bounded_bounding_box();
    let mut curr_half_clearance = -1i32;
    let mut curr_offset_tile = shape.clone();
    out.retain(|id| {
        let Some(slot) = board.store().slot_of(*id) else {
            return false;
        };
        let Some(item) = board.store().item_at_slot(slot) else {
            return false;
        };
        let clearance = board
            .clearances()
            .value(cl_class, item.clearance_class, layer);
        let half_clearance = clearance / 2;
        tree.item_shapes(slot, *id).iter().any(|tile| {
            if tile.layer != layer {
                return false;
            }
            let Some(item_shape) = tile.shape.as_ref() else {
                return false;
            };
            if let (Some(sb), Some(tb)) = (shape_bbox, item_shape.bounded_bounding_box()) {
                if !within_padded_bounds(sb, tb, clearance) {
                    return false;
                }
            }
            if half_clearance != curr_half_clearance {
                curr_half_clearance = half_clearance;
                curr_offset_tile = shape.enlarge(half_clearance);
            }
            item_shape
                .enlarge(half_clearance)
                .intersects(&curr_offset_tile)
        })
    });
}

pub fn overlapping_items_with_clearance(
    board: &Board,
    shape: &Simplex,
    layer: i32,
    ignore_nets: &[i32],
    cl_class: i32,
    out: &mut Vec<ItemId>,
) {
    out.clear();
    let compensated = board.trees().clearance_compensation_used();
    let query_shape = TileShape::Simplex(shape.clone());
    if compensated {
        board.overlapping_items(&query_shape, Some(layer), ignore_nets, out);
        return;
    }

    // The broad phase: everything within the largest clearance this class asks
    // of anybody. Upstream offsets the *bounding* shape by `1.2 * max_clearance`
    // (`ShapeSearchTree.java:439-446`); the exact per-item test below is what
    // decides.
    let max_clearance = max_clearance_of(board, cl_class, layer);
    if max_clearance == 0 {
        board.overlapping_items(&query_shape, Some(layer), ignore_nets, out);
        return;
    }
    let reach = (1.2 * max_clearance as f64).ceil() as i32 + 4;
    let broad_bounds = match shape.bounding_octagon() {
        Some(oct) => TileShape::Oct(oct.offset(f64::from(reach))).bounds(),
        None => TileShape::Simplex(shape.offset(f64::from(reach))).bounds(),
    };
    board.overlapping_items_bounds_only(&broad_bounds, Some(layer), ignore_nets, out);

    let tree = board.default_tree();
    let shape_bbox = shape.bounded_bounding_box();
    let mut curr_half_clearance = -1i32;
    let mut curr_offset_oct: Option<OctR2> = None;
    let mut curr_offset_tile: Option<(i32, TileShape)> = None;
    // Upstream sorts by clearance so the offset shape is rebuilt once per
    // distinct clearance; the same trick, without the sort, is to cache the
    // last one — clearance classes repeat heavily in a single query.
    out.retain(|id| {
        let Some(slot) = board.store().slot_of(*id) else {
            return false;
        };
        let Some(item) = board.store().item_at_slot(slot) else {
            return false;
        };
        let clearance = board
            .clearances()
            .value(cl_class, item.clearance_class, layer);
        let half_clearance = clearance / 2;
        let hulls = tree.item_hulls(slot, *id);
        tree.item_shapes(slot, *id)
            .iter()
            .enumerate()
            .any(|(idx, tile)| {
                if tile.layer != layer {
                    return false;
                }
                let Some(tile_shape) = tile.shape.as_ref() else {
                    return false;
                };
                let tb = hulls
                    .get(idx)
                    .copied()
                    .flatten()
                    .or_else(|| tile_shape.bounded_bounding_box());
                if let (Some(sb), Some(tb)) = (shape_bbox, tb) {
                    if !within_padded_bounds(sb, tb, clearance) {
                        return false;
                    }
                }
                if half_clearance != curr_half_clearance {
                    curr_half_clearance = half_clearance;
                    curr_offset_oct = shape.enlarge_oct_r2(f64::from(half_clearance));
                }
                if let (Some(qa), Some(qb)) =
                    (curr_offset_oct, tile_shape.enlarge_oct_r2(half_clearance))
                {
                    return qa.intersects_r2(&qb);
                }
                let offset_tile = match &curr_offset_tile {
                    Some((cached_half, tile)) if *cached_half == half_clearance => tile,
                    _ => {
                        curr_offset_tile = Some((
                            half_clearance,
                            TileShape::Simplex(shape.enlarge(half_clearance as f64)),
                        ));
                        &curr_offset_tile.as_ref().expect("just stored").1
                    }
                };
                tile_shape.enlarge(half_clearance).intersects(offset_tile)
            })
    });
}

#[inline]
fn within_padded_bounds(a: IntBox, b: IntBox, clearance: i32) -> bool {
    let pad = i64::from(clearance.max(0)) + 4;
    i64::from(a.ur.x) + pad >= i64::from(b.ll.x)
        && i64::from(b.ur.x) + pad >= i64::from(a.ll.x)
        && i64::from(a.ur.y) + pad >= i64::from(b.ll.y)
        && i64::from(b.ur.y) + pad >= i64::from(a.ll.y)
}

/// The largest clearance `cl_class` requires of any class on `layer`, upstream's
/// `ClearanceMatrix.max_value(cl_class, layer)`.
pub fn max_clearance_of(board: &Board, cl_class: i32, layer: i32) -> i32 {
    let clearances = board.clearances();
    let mut max = 0;
    for other in 0..clearances.class_count() as i32 {
        max = max.max(clearances.value(cl_class, other, layer));
    }
    max
}

/// `net_nos_equal` (`ShapeTraceEntries.java:107-124`) — set equality, not
/// sequence equality: upstream compares two `int[]` by membership in both
/// directions of the length check.
pub fn net_nos_equal(a: &[i32], b: &[i32]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().all(|net| b.contains(net))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_box(lx: i32, ly: i32, rx: i32, uy: i32) -> Simplex {
        IntBox::new(lx, ly, rx, uy).to_simplex()
    }

    #[test]
    fn border_line_number_is_none_for_interior_and_exterior_points() {
        let shape = unit_box(0, 0, 10, 10);
        assert!(contains_on_border_line_no(&shape, &Point::Int(IntPoint::new(5, 5))).is_none());
        assert!(contains_on_border_line_no(&shape, &Point::Int(IntPoint::new(20, 5))).is_none());
        assert!(contains_on_border_line_no(&shape, &Point::Int(IntPoint::new(0, 5))).is_some());
    }

    #[test]
    fn the_nearest_border_point_of_an_interior_point_is_on_the_border() {
        let shape = unit_box(0, 0, 100, 100);
        let point = Point::Int(IntPoint::new(10, 50));
        let nearest = nearest_border_point(&shape, &point).expect("bounded shape has a border");
        assert!(contains_on_border_line_no(&shape, &nearest).is_some());
    }

    #[test]
    fn box_border_projections_are_ordered_nearest_first() {
        let shape = IntBox::new(0, 0, 100, 40);
        let projections = box_nearest_border_projections(&shape, &IntPoint::new(10, 20), 2);
        assert_eq!(projections.len(), 2);
        assert_eq!(projections[0], IntPoint::new(0, 20));
    }

    #[test]
    fn net_number_equality_ignores_order() {
        assert!(net_nos_equal(&[3, 7], &[7, 3]));
        assert!(!net_nos_equal(&[3, 7], &[3, 3]));
        assert!(!net_nos_equal(&[3], &[3, 7]));
    }

    #[test]
    fn octagon_border_projections_stay_on_the_octagon() {
        let oct = IntBox::new(-20, -20, 20, 20).to_int_octagon();
        let projections = octagon_nearest_border_projections(&oct, &IntPoint::new(0, 0), 4);
        assert_eq!(projections.len(), 4);
        let simplex = oct.to_simplex();
        for point in projections {
            assert!(
                simplex.contains(&Point::Int(point)),
                "{point:?} left the octagon"
            );
        }
    }
}
