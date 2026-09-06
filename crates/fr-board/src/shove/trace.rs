//! `board/ShoveTraceAlgo.java` (640 L) — the trace shover.
//!
//! Three entry points, all ported here:
//!
//! * [`check_shove_segment`] — the **static** maze-time primitive (`:37-161`).
//!   Returns the maximum length from the segment start for which the shove
//!   succeeds, [`FULL_SHOVE`] for "all of it", `<= 0` for "none of it". The
//!   partial-length arithmetic is what lets the maze expand a *shortened*
//!   shove instead of giving up (spec §10.2).
//! * [`ShoveCheck::check`] — the **instance** check (`:166-292`), the full
//!   non-destructive verification run right before an insert.
//! * [`ShoveInsert::insert`] — the destructive twin (`:297-418`), here wrapped
//!   by [`ShoveInsert::insert_transaction`] so a failure rolls back.
//!
//! plus [`ShoveCheck::spring_over`] / [`ShoveCheck::spring_over_obstacles`]
//! (`:437-640`), which wrap a polyline *around* an unshovable obstacle instead
//! of pushing it.
//!
//! # Direction filtering
//!
//! Both checks filter substitute segments by direction — the static one by
//! "same direction as the original line" (`is_in_front`), the instance one by
//! the `p_dir` parameter that *"is used internally to prevent the check from
//! bouncing back"*. `shove.md` §15 flags removing it as causing infinite mutual
//! shoving, so both filters are transcribed literally.

use fr_geometry::planar::{
    Direction, FloatPoint, IntBox, Line, LineSegment, Point, Polyline, Simplex,
};

use super::entries::{
    compensated_half_width, contains_trace_tails, tree_shape_on_layer, ShapeTraceEntries,
    SubstitutePiece,
};
use super::from_side::FromSide;
use super::geom;
use super::shape_and_from_side::ShapeAndFromSide;
use super::{ShoveCheck, ShoveInsert, FULL_SHOVE};
use crate::board::Board;
use crate::clearance::ClearanceSource;
use crate::item::{FixedState, ItemId, ItemKind, ObstacleKind};
use crate::net_nos::NetNos;

/// `ShoveTraceAlgo.check(board, line_segment, …)` (`:37-161`) — the static
/// maze-time primitive.
///
/// Returns the maximum length from the start of `line_segment` for which the
/// shove succeeds; [`FULL_SHOVE`] means the whole segment, `0` means nothing.
#[allow(clippy::too_many_arguments)]
pub fn check_shove_segment(
    board: &Board,
    bounds: &IntBox,
    line_segment: &LineSegment,
    shove_to_the_left: bool,
    layer: i32,
    net_no_arr: &NetNos,
    trace_half_width: i32,
    cl_type: i32,
    max_recursion_depth: i32,
    max_via_recursion_depth: i32,
) -> f64 {
    let compensation_used = board.trees().clearance_compensation_used();
    let trace_half_width = if compensation_used {
        trace_half_width + board.clearances().compensation_value(cl_type, layer)
    } else {
        trace_half_width
    };

    let polyline = line_segment.to_polyline();
    let trace_shapes = polyline.offset_shapes(trace_half_width);
    if trace_shapes.len() != 1 {
        // "trace_shape count 1 expected".
        return 0.0;
    }
    let trace_shape = trace_shapes.into_iter().next().expect("length checked");
    if trace_shape.is_empty() || !geom::simplex_contained_in(&trace_shape, bounds) {
        return 0.0;
    }

    let from_side = FromSide::from_line_segment(line_segment, &trace_shape, shove_to_the_left);
    let mut shape_entries = ShapeTraceEntries::new(
        trace_shape.clone(),
        layer,
        net_no_arr.clone(),
        cl_type,
        Some(from_side),
    );
    let mut obstacles = Vec::new();
    geom::overlapping_items_with_clearance(
        board,
        &trace_shape,
        layer,
        &[],
        cl_type,
        &mut obstacles,
    );
    let obstacles_shovable = shape_entries.store_items(board, &obstacles, false, true);
    if !obstacles_shovable || shape_entries.trace_tails_in_shape() {
        return 0.0;
    }
    let trace_piece_count = shape_entries.substitute_trace_count();
    if shape_entries.stack_depth() > 1 || (trace_piece_count > 0 && max_recursion_depth <= 0) {
        return 0.0;
    }

    let start_corner_approx = line_segment.start_point_approx();
    let end_corner_approx = line_segment.end_point_approx();
    let segment_length = end_corner_approx.distance(&start_corner_approx);
    if segment_length == 0.0 {
        return 0.0;
    }

    let mut result = FULL_SHOVE;

    // Can the obstacle vias be shoved? (`:80-135`)
    let shove_vias: Vec<ItemId> = shape_entries.shove_via_list.clone();
    for via_id in shove_vias {
        let Some(via) = board.store().get(via_id) else {
            continue;
        };
        if via.nets.shares_any(net_no_arr) {
            continue;
        }
        let via_cl_class = via.clearance_class;
        let mut shove_via_ok = false;
        if max_via_recursion_depth > 0 {
            let new_via_centers = super::drill::try_shove_via_points(
                board,
                &trace_shape,
                layer,
                via_id,
                cl_type,
                false,
            );
            let Some(&first) = new_via_centers.first() else {
                return 0.0;
            };
            let ItemKind::Via(via_payload) = &via.kind else {
                continue;
            };
            let delta = first.difference_by_int(&via_payload.center);
            let mut checker = ShoveCheck::new(board, *bounds);
            let mut ignore_items = Vec::new();
            shove_via_ok = checker.check_move_drill_item(
                via_id,
                &delta,
                max_recursion_depth,
                max_via_recursion_depth - 1,
                &mut ignore_items,
            );
        }
        if !shove_via_ok {
            let Some(via) = board.store().get(via_id) else {
                continue;
            };
            let ItemKind::Via(via_payload) = &via.kind else {
                continue;
            };
            let via_center_approx = via_payload.center.to_float();
            let mut projection =
                start_corner_approx.scalar_product(&end_corner_approx, &via_center_approx);
            projection /= segment_length;
            let via_box = tree_shape_on_layer(board, via_id, layer)
                .map(|shape| shape.bounding_box())
                .unwrap_or(IntBox::EMPTY);
            let via_radius = 0.5 * via_box.max_width();
            let mut curr_ok_length = projection - via_radius - trace_half_width as f64;
            if !compensation_used {
                curr_ok_length -= board.clearances().value(cl_type, via_cl_class, layer) as f64;
            }
            if curr_ok_length <= 0.0 {
                return 0.0;
            }
            result = result.min(curr_ok_length);
        }
    }

    if trace_piece_count == 0 {
        return result;
    }
    if max_recursion_depth <= 0 {
        return 0.0;
    }

    let line_direction = line_segment.get_line().direction();
    while let Some(piece) = shape_entries.next_substitute_trace_piece(board) {
        for i in 0..piece.tile_shape_count() {
            let Some(mut curr_line_segment) = LineSegment::from_polyline(&piece.polyline, i + 1)
            else {
                continue;
            };
            if shove_to_the_left {
                // Swap the segment so the shove length compares against the
                // same end (`:124-128`).
                curr_line_segment = curr_line_segment.opposite();
            }
            let is_in_front = curr_line_segment.get_line().direction() == line_direction;
            if !is_in_front {
                continue;
            }
            let shove_ok_length = check_shove_segment(
                board,
                bounds,
                &curr_line_segment,
                shove_to_the_left,
                layer,
                &piece.nets,
                piece.half_width,
                piece.cl_class,
                max_recursion_depth - 1,
                max_via_recursion_depth,
            );
            if shove_ok_length < FULL_SHOVE {
                if shove_ok_length <= 0.0 {
                    return 0.0;
                }
                let mut projection =
                    start_corner_approx
                        .scalar_product(&end_corner_approx, &curr_line_segment.start_point_approx())
                        .min(start_corner_approx.scalar_product(
                            &end_corner_approx,
                            &curr_line_segment.end_point_approx(),
                        ));
                projection /= segment_length;
                let mut curr_ok_length = shove_ok_length + projection
                    - trace_half_width as f64
                    - piece.half_width as f64;
                // Conformance appendix: the substitute branch subtracts the
                // *tree compensation* when compensation is on, not the
                // asymmetric matrix value (`:145-149`).
                if compensation_used {
                    curr_ok_length -=
                        board
                            .clearances()
                            .tree_compensation(piece.cl_class, cl_type, layer)
                            as f64;
                } else {
                    curr_ok_length -=
                        board.clearances().value(cl_type, piece.cl_class, layer) as f64;
                }
                if curr_ok_length <= 0.0 {
                    return 0.0;
                }
                result = curr_ok_length.min(result);
            }
            // Only the first in-front segment per piece (`:157`).
            break;
        }
    }
    result
}

/// `RoutingBoard.check_trace_segment(line_segment, …)` (`RoutingBoard.java:250-306`).
///
/// The length along the segment that fits without conflict, or [`FULL_SHOVE`]
/// when the whole of it does. `only_not_shovable_obstacles` skips routable,
/// non-shove-fixed items — the maze shover passes `true`, because those are
/// exactly the ones the shove itself will handle.
///
/// Pure geometry: no `ShapeTraceEntries`, no recursion.
#[allow(clippy::too_many_arguments)]
pub fn check_trace_segment(
    board: &Board,
    line_segment: &LineSegment,
    layer: i32,
    net_no_arr: &NetNos,
    trace_half_width: i32,
    cl_class_no: i32,
    only_not_shovable_obstacles: bool,
) -> f64 {
    let check_polyline = line_segment.to_polyline();
    if check_polyline.arr.len() != 3 {
        return 0.0;
    }
    let Some(shape_to_check) = check_polyline.offset_shape(trace_half_width, 0) else {
        return 0.0;
    };
    let from_point = line_segment.start_point_approx();
    let to_point = line_segment.end_point_approx();
    let line_length = to_point.distance(&from_point);
    if line_length == 0.0 {
        return 0.0;
    }
    let mut ok_length = FULL_SHOVE;
    let compensation_used = board.trees().clearance_compensation_used();
    let tree = board.default_tree();

    let mut obstacles = Vec::new();
    geom::overlapping_items_with_clearance(
        board,
        &shape_to_check,
        layer,
        net_no_arr.as_slice(),
        cl_class_no,
        &mut obstacles,
    );
    for id in obstacles {
        let Some(item) = board.store().get(id) else {
            continue;
        };
        if only_not_shovable_obstacles && item.is_routable() && !item.is_shove_fixed() {
            continue;
        }
        let Some(slot) = board.store().slot_of(id) else {
            continue;
        };
        let (curr_offset_shape, shorten_value) = if compensation_used {
            (
                shape_to_check.clone(),
                trace_half_width as f64
                    + board
                        .clearances()
                        .compensation_value(item.clearance_class, layer)
                        as f64,
            )
        } else {
            let clearance_value =
                board
                    .clearances()
                    .value(item.clearance_class, cl_class_no, layer);
            (
                shape_to_check.offset(clearance_value as f64),
                trace_half_width as f64 + clearance_value as f64,
            )
        };
        for tile in tree.item_shapes(slot, id) {
            if tile.layer != layer {
                continue;
            }
            let Some(obstacle_shape) = tile.shape.as_ref().map(geom::tile_to_simplex) else {
                continue;
            };
            let intersection = obstacle_shape.intersection(&curr_offset_shape);
            if intersection.is_empty() {
                continue;
            }
            let nearest = intersection.nearest_point_approx(&from_point);
            let projection = from_point.scalar_product(&to_point, &nearest) / line_length;
            let projection = (projection - shorten_value - 1.0).max(0.0);
            if projection < ok_length {
                ok_length = projection;
                if ok_length <= 0.0 {
                    return 0.0;
                }
            }
        }
    }
    ok_length
}

impl ShoveCheck<'_> {
    /// `ShoveTraceAlgo.check(trace_shape, from_side, dir, …)` (`:166-292`) — the
    /// instance check.
    #[allow(clippy::too_many_arguments)]
    pub fn check(
        &mut self,
        trace_shape: &Simplex,
        from_side: Option<FromSide>,
        dir: Option<Direction>,
        layer: i32,
        net_no_arr: &NetNos,
        cl_type: i32,
        max_recursion_depth: i32,
        max_via_recursion_depth: i32,
        mut max_spring_over_recursion_depth: i32,
    ) -> bool {
        if trace_shape.is_empty() {
            // Upstream warns and answers true.
            return true;
        }
        if !geom::simplex_contained_in(trace_shape, &self.bounds) {
            self.failing_obstacle = self.board_outline();
            return false;
        }
        let board = self.board;
        let mut shape_entries = ShapeTraceEntries::new(
            trace_shape.clone(),
            layer,
            net_no_arr.clone(),
            cl_type,
            from_side,
        );
        let mut obstacles = self.scratch.take();
        geom::overlapping_items_with_clearance(
            board,
            trace_shape,
            layer,
            &[],
            cl_type,
            &mut obstacles,
        );
        remove_ignore_items_at_tie_pins(board, trace_shape, layer, net_no_arr, &mut obstacles);
        let stored = shape_entries.store_items(board, &obstacles, false, true);
        // The obstacle list is dead from here on; hand the buffer back before
        // recursing so the next level reuses it instead of allocating.
        self.scratch.give(obstacles);
        if !stored {
            let blame = shape_entries.found_obstacle();
            self.blame(blame);
            return false;
        }
        let trace_piece_count = shape_entries.substitute_trace_count();
        if shape_entries.stack_depth() > 1 || (max_recursion_depth <= 0 && trace_piece_count > 0) {
            let blame = shape_entries.found_obstacle();
            self.blame(blame);
            return false;
        }
        let shape_radius = 0.5 * trace_shape.bounding_box().min_width();

        let shove_vias: Vec<ItemId> = shape_entries.shove_via_list.clone();
        for via_id in shove_vias {
            let Some(via) = board.store().get(via_id) else {
                continue;
            };
            if via.nets.shares_any(net_no_arr) {
                continue;
            }
            if max_via_recursion_depth <= 0 {
                self.failing_obstacle = Some(via_id);
                return false;
            }
            let ItemKind::Via(via_payload) = &via.kind else {
                continue;
            };
            let via_center = via_payload.center;
            let curr_shove_via_center = via_center.to_float();
            let try_via_centers = super::drill::try_shove_via_points(
                board,
                trace_shape,
                layer,
                via_id,
                cl_type,
                true,
            );
            let max_dist = 0.5
                * tree_shape_on_layer(board, via_id, layer)
                    .map(|shape| shape.bounding_box())
                    .unwrap_or(IntBox::EMPTY)
                    .max_width()
                + shape_radius;
            let max_dist_square = max_dist * max_dist;
            let mut shove_via_ok = false;
            for (i, candidate) in try_via_centers.iter().enumerate() {
                if i == 0
                    || curr_shove_via_center.distance_square(&candidate.to_float())
                        <= max_dist_square
                {
                    let delta = candidate.difference_by_int(&via_center);
                    let mut ignore_items = Vec::new();
                    if self.check_move_drill_item(
                        via_id,
                        &delta,
                        max_recursion_depth,
                        max_via_recursion_depth - 1,
                        &mut ignore_items,
                    ) {
                        shove_via_ok = true;
                        break;
                    }
                }
            }
            if !shove_via_ok {
                return false;
            }
        }

        if trace_piece_count == 0 {
            return true;
        }
        if max_recursion_depth <= 0 {
            let blame = shape_entries.found_obstacle();
            self.blame(blame);
            return false;
        }

        let orthogonal = self.orthogonal;
        while let Some(mut piece) = shape_entries.next_substitute_trace_piece(board) {
            if max_spring_over_recursion_depth > 0 {
                let half_width =
                    compensated_half_width(board, piece.half_width, piece.cl_class, piece.layer);
                let Some(new_polyline) = self.spring_over(
                    &piece.polyline,
                    half_width,
                    layer,
                    &piece.nets,
                    piece.cl_class,
                    false,
                    max_spring_over_recursion_depth,
                ) else {
                    return false;
                };
                if new_polyline != piece.polyline {
                    max_spring_over_recursion_depth -= 1;
                    piece.polyline = new_polyline;
                }
            }
            for i in 0..piece.tile_shape_count() {
                let curr_dir = piece.polyline.arr[i + 1].direction();
                let is_in_front = dir.map(|d| d == curr_dir).unwrap_or(true);
                if !is_in_front {
                    continue;
                }
                let Some(curr) = ShapeAndFromSide::new(board, &piece, i, orthogonal, true) else {
                    continue;
                };
                if !self.check(
                    &curr.shape,
                    curr.from_side,
                    Some(curr_dir),
                    layer,
                    &piece.nets,
                    piece.cl_class,
                    max_recursion_depth - 1,
                    max_via_recursion_depth,
                    max_spring_over_recursion_depth,
                ) {
                    return false;
                }
            }
        }
        true
    }

    /// `RoutingBoard.get_outline()` as an id, for the blame field.
    pub(crate) fn board_outline(&self) -> Option<ItemId> {
        self.board
            .store()
            .iter()
            .find(|(_, item)| matches!(item.kind, ItemKind::BoardOutline(_)))
            .map(|(id, _)| id)
    }

    /// `ShoveTraceAlgo.spring_over` (`:437-610`): wrap `polyline` around the
    /// first unshovable obstacle in counterclockwise sense.
    ///
    /// Returns `polyline` unchanged when there is no obstacle, `None` when the
    /// wrap is impossible.
    #[allow(clippy::too_many_arguments)]
    pub fn spring_over(
        &mut self,
        polyline: &Polyline,
        half_width: i32,
        layer: i32,
        net_no_arr: &NetNos,
        cl_type: i32,
        over_connected_pins: bool,
        recursion_depth: i32,
    ) -> Option<Polyline> {
        let board = self.board;
        let mut found_obstacle: Option<ItemId> = None;
        let mut found_obstacle_bounding_box = IntBox::EMPTY;
        // `p_contact_pins == null` on every call site the router reaches, so
        // the check net array is the own net array and own-net pins are never
        // obstacles (`:441-446`).
        let check_net_no_arr: Vec<i32> = net_no_arr.as_slice().to_vec();

        let mut obstacles = Vec::new();
        for i in 0..polyline.arr.len().saturating_sub(2) {
            let Some(curr_shape) = polyline.offset_shape(half_width, i) else {
                continue;
            };
            geom::overlapping_items_with_clearance(
                board,
                &curr_shape,
                layer,
                &check_net_no_arr,
                cl_type,
                &mut obstacles,
            );
            for &curr_id in obstacles.iter() {
                let Some(curr_item) = board.store().get(curr_id) else {
                    continue;
                };
                let is_obstacle = if curr_item.nets.shares_any(net_no_arr) {
                    // To avoid acid traps: with no contact-pin set, own-net
                    // items are never obstacles.
                    false
                } else {
                    match &curr_item.kind {
                        ItemKind::ConductionArea(area) => area.is_obstacle,
                        ItemKind::ObstacleArea(area) => {
                            !matches!(
                                area.kind,
                                ObstacleKind::ViaKeepout | ObstacleKind::ComponentKeepout
                            ) && !curr_item.is_routable()
                        }
                        ItemKind::Trace(trace) => {
                            if curr_item.is_shove_fixed() {
                                // A shove-fixed trace exit stub has to be
                                // ignored at a tie pin (`:462-470`).
                                let mut result = true;
                                let mut contacts = Vec::new();
                                for corner in [trace.first_corner(), trace.last_corner()] {
                                    board.normal_contacts_at(curr_id, &corner, &mut contacts);
                                    for &contact_id in contacts.iter() {
                                        if board
                                            .store()
                                            .get(contact_id)
                                            .is_some_and(|c| c.nets.shares_any(net_no_arr))
                                        {
                                            result = false;
                                        }
                                    }
                                }
                                result
                            } else {
                                // An unfixed trace can be pushed aside.
                                false
                            }
                        }
                        _ => !curr_item.is_routable(),
                    }
                };
                if !is_obstacle {
                    continue;
                }
                match found_obstacle {
                    None => {
                        found_obstacle = Some(curr_id);
                        found_obstacle_bounding_box = item_bounding_box(board, curr_id);
                    }
                    Some(existing) if existing != curr_id => {
                        // Keep the bigger of two overlapping obstacles; fail
                        // when neither contains the other (`:483-497`).
                        let curr_box = item_bounding_box(board, curr_id);
                        if found_obstacle_bounding_box.intersects(&curr_box) {
                            if curr_box.contains_box(&found_obstacle_bounding_box) {
                                found_obstacle = Some(curr_id);
                                found_obstacle_bounding_box = curr_box;
                            } else if !found_obstacle_bounding_box.contains_box(&curr_box) {
                                return None;
                            }
                        }
                    }
                    _ => {}
                }
            }
            if found_obstacle.is_some() {
                break;
            }
        }

        let Some(obstacle_id) = found_obstacle else {
            // No obstacle in the way, nothing to do.
            return Some(polyline.clone());
        };
        let Some(obstacle) = board.store().get(obstacle_id) else {
            return Some(polyline.clone());
        };
        let obstacle_is_unfixed_trace =
            matches!(obstacle.kind, ItemKind::Trace(_)) && !obstacle.is_shove_fixed();
        if recursion_depth <= 0
            || matches!(obstacle.kind, ItemKind::BoardOutline(_))
            || obstacle_is_unfixed_trace
        {
            self.failing_obstacle = Some(obstacle_id);
            return None;
        }

        let mut try_spring_over = true;
        if !over_connected_pins {
            // Don't wrap around a pad that already carries a trace (`:517-527`).
            let mut contacts = Vec::new();
            for point in connection_points(board, obstacle_id) {
                board.normal_contacts_at(obstacle_id, &point, &mut contacts);
                for &contact_id in contacts.iter() {
                    if let Some(contact) = board.store().get(contact_id) {
                        if matches!(contact.kind, ItemKind::Trace(_)) && contact.is_on_layer(layer)
                        {
                            try_spring_over = false;
                        }
                    }
                }
            }
        }

        let mut obstacle_shape = None;
        if try_spring_over {
            let slot = board.store().slot_of(obstacle_id);
            match &obstacle.kind {
                ItemKind::ObstacleArea(_) | ItemKind::ConductionArea(_) | ItemKind::Trace(_) => {
                    let shapes = slot
                        .map(|slot| board.default_tree().item_shapes(slot, obstacle_id))
                        .unwrap_or(&[]);
                    if shapes.len() == 1 {
                        obstacle_shape = shapes[0].shape.as_ref().map(geom::tile_to_simplex);
                    } else {
                        try_spring_over = false;
                    }
                }
                ItemKind::Pin(_) | ItemKind::Via(_) => {
                    obstacle_shape = tree_shape_on_layer(board, obstacle_id, layer)
                        .map(|s| geom::tile_to_simplex(&s));
                }
                _ => try_spring_over = false,
            }
        }
        let Some(obstacle_shape) = obstacle_shape.filter(|_| try_spring_over) else {
            self.failing_obstacle = Some(obstacle_id);
            return None;
        };

        let offset = (half_width + 1) as f64;
        let offset_shape = if board.trees().clearance_compensation_used() {
            obstacle_shape.enlarge(offset)
        } else {
            // Two half steps, for symmetry (`:539-548`).
            let half_cl_offset = 0.5
                * board
                    .clearances()
                    .value(obstacle.clearance_class, cl_type, layer) as f64;
            obstacle_shape
                .enlarge(offset + half_cl_offset)
                .enlarge(half_cl_offset)
        };
        let offset_shape = if self.orthogonal {
            offset_shape.bounding_box().to_simplex()
        } else {
            match offset_shape.bounding_octagon() {
                Some(octagon) => octagon.to_simplex(),
                None => offset_shape,
            }
        };

        if offset_shape.contains_inside(&polyline.first_corner())
            || offset_shape.contains_inside(&polyline.last_corner())
        {
            // Can happen with clearance compensation off, because of asymmetry
            // in the offset-shape arithmetic (`:556-561`).
            self.failing_obstacle = Some(obstacle_id);
            return None;
        }
        let entries = offset_shape.entrance_points(polyline);
        if entries.is_empty() {
            return Some(polyline.clone());
        }
        if entries.len() < 2 {
            self.failing_obstacle = Some(obstacle_id);
            return None;
        }
        let pieces = offset_shape.cutout_polyline(polyline);
        let border_line_count = offset_shape.border_line_count() as i32;
        let first_intersection_side_no = entries[0].1 as i32;
        let last_intersection_side_no = entries[entries.len() - 1].1 as i32;
        let first_intersection_line_no = entries[0].0;
        let last_intersection_line_no = entries[entries.len() - 1].0;
        let mut side_diff = last_intersection_side_no - first_intersection_side_no;
        if side_diff < 0 {
            side_diff += border_line_count;
        } else if side_diff == 0 {
            let compare_corner = offset_shape.corner_approx(first_intersection_side_no as usize);
            let first_intersection = polyline.arr[first_intersection_line_no].intersection_approx(
                &offset_shape.border_line(first_intersection_side_no as usize),
            );
            let second_intersection = polyline.arr[last_intersection_line_no]
                .intersection_approx(&offset_shape.border_line(last_intersection_side_no as usize));
            if compare_corner.distance(&second_intersection)
                < compare_corner.distance(&first_intersection)
            {
                side_diff += border_line_count;
            }
        }
        let mut substitute_lines: Vec<Line> = Vec::with_capacity(side_diff as usize + 3);
        substitute_lines.push(polyline.arr[first_intersection_line_no]);
        let mut curr_edge_line_no = first_intersection_side_no as usize;
        for _ in 0..=side_diff {
            substitute_lines.push(offset_shape.border_line(curr_edge_line_no));
            curr_edge_line_no = if curr_edge_line_no == border_line_count as usize - 1 {
                0
            } else {
                curr_edge_line_no + 1
            };
        }
        substitute_lines.push(polyline.arr[last_intersection_line_no]);
        let substitute_polyline = Polyline::new_from_lines(&substitute_lines);
        let mut result = substitute_polyline;
        if !pieces.is_empty() {
            result = pieces[0].combine(&result);
        }
        if pieces.len() > 1 {
            result = result.combine(&pieces[1]);
        }
        self.spring_over(
            &result,
            half_width,
            layer,
            net_no_arr,
            cl_type,
            over_connected_pins,
            recursion_depth - 1,
        )
    }

    /// `ShoveTraceAlgo.spring_over_obstacles` (`:617-640`): try counterclockwise
    /// and clockwise, keep the shorter result.
    ///
    /// `c_max_spring_over_recursion_depth = 20` (`:618`), which is a *different*
    /// constant from `AutorouteControl.max_spring_over_recursion_depth = 5`.
    pub fn spring_over_obstacles(
        &mut self,
        polyline: &Polyline,
        half_width: i32,
        layer: i32,
        net_no_arr: &NetNos,
        cl_type: i32,
    ) -> Option<Polyline> {
        const C_MAX_SPRING_OVER_RECURSION_DEPTH: i32 = 20;
        let counter_clock_wise_result = self.spring_over(
            polyline,
            half_width,
            layer,
            net_no_arr,
            cl_type,
            true,
            C_MAX_SPRING_OVER_RECURSION_DEPTH,
        );
        if counter_clock_wise_result.as_ref() == Some(polyline) {
            // No obstacle.
            return Some(polyline.clone());
        }
        let clock_wise_result = self
            .spring_over(
                &reverse_polyline(polyline),
                half_width,
                layer,
                net_no_arr,
                cl_type,
                true,
                C_MAX_SPRING_OVER_RECURSION_DEPTH,
            )
            .map(|line| reverse_polyline(&line));
        match (clock_wise_result, counter_clock_wise_result) {
            (Some(clock_wise), Some(counter)) => {
                if clock_wise.length_total() <= counter.length_total() {
                    Some(clock_wise)
                } else {
                    Some(counter)
                }
            }
            (Some(clock_wise), None) => Some(clock_wise),
            (None, Some(counter)) => Some(counter),
            (None, None) => None,
        }
    }
}

impl ShoveInsert<'_> {
    /// [`ShoveInsert::insert`] wrapped in a board transaction.
    ///
    /// Upstream's contract is *"if the shove does not work, the database may be
    /// damaged"* (`:295-297`); here a failure restores the board exactly, which
    /// is the `shove.md` §15 restructuring. Callers keep the same boolean
    /// protocol, they just no longer have to own an undo.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_transaction(
        &mut self,
        trace_shape: &Simplex,
        from_side: Option<FromSide>,
        layer: i32,
        net_no_arr: &NetNos,
        cl_type: i32,
        ignore_items: &[ItemId],
        max_recursion_depth: i32,
        max_via_recursion_depth: i32,
        max_spring_over_recursion_depth: i32,
    ) -> bool {
        let mark = self.board.begin();
        let ok = self.insert(
            trace_shape,
            from_side,
            layer,
            net_no_arr,
            cl_type,
            ignore_items,
            max_recursion_depth,
            max_via_recursion_depth,
            max_spring_over_recursion_depth,
        );
        if ok {
            self.board.commit(mark);
        } else {
            self.board.rollback_to(mark);
        }
        ok
    }

    /// `ShoveTraceAlgo.insert(trace_shape, from_side, …)` (`:297-418`).
    ///
    /// Note the recursion order: space is cleared *behind* each substitute
    /// piece before the piece itself goes in, and the stack depth is not
    /// re-checked — the preceding `check` is the guard.
    #[allow(clippy::too_many_arguments)]
    pub fn insert(
        &mut self,
        trace_shape: &Simplex,
        from_side: Option<FromSide>,
        layer: i32,
        net_no_arr: &NetNos,
        cl_type: i32,
        ignore_items: &[ItemId],
        max_recursion_depth: i32,
        max_via_recursion_depth: i32,
        mut max_spring_over_recursion_depth: i32,
    ) -> bool {
        if trace_shape.is_empty() {
            return true;
        }
        if !geom::simplex_contained_in(trace_shape, &self.bounds) {
            self.failing_obstacle = self.checker().board_outline();
            return false;
        }
        if !self.shove_vias(
            trace_shape,
            from_side,
            layer,
            net_no_arr,
            cl_type,
            ignore_items,
            max_recursion_depth,
            max_via_recursion_depth,
            true,
        ) {
            return false;
        }

        let mut shape_entries = ShapeTraceEntries::new(
            trace_shape.clone(),
            layer,
            net_no_arr.clone(),
            cl_type,
            from_side,
        );
        let mut obstacles = self.scratch.take();
        geom::overlapping_items_with_clearance(
            self.board,
            trace_shape,
            layer,
            &[],
            cl_type,
            &mut obstacles,
        );
        remove_ignore_items_at_tie_pins(self.board, trace_shape, layer, net_no_arr, &mut obstacles);
        let obstacles_shovable = shape_entries.store_items(self.board, &obstacles, false, true);
        if let Some(&via_id) = shape_entries.shove_via_list.first() {
            // Every via should have been moved by `shove_vias` already.
            self.failing_obstacle = Some(via_id);
            self.scratch.give(obstacles);
            return false;
        }
        if !obstacles_shovable {
            let blame = shape_entries.found_obstacle();
            self.blame(blame);
            self.scratch.give(obstacles);
            return false;
        }
        let trace_piece_count = shape_entries.substitute_trace_count();
        if trace_piece_count == 0 {
            self.scratch.give(obstacles);
            return true;
        }
        if max_recursion_depth <= 0 {
            let blame = shape_entries.found_obstacle();
            self.blame(blame);
            self.scratch.give(obstacles);
            return false;
        }

        let tails_exist_before = contains_trace_tails(self.board, &obstacles, net_no_arr);
        shape_entries.cutout_traces(self.board, &obstacles);
        // Everything below only walks the substitute pieces.
        self.scratch.give(obstacles);

        let orthogonal = self.orthogonal;
        while let Some(mut piece) = shape_entries.next_substitute_trace_piece(self.board) {
            if piece.polyline.first_corner() == piece.polyline.last_corner() {
                continue;
            }
            if max_spring_over_recursion_depth > 0 {
                let half_width = compensated_half_width(
                    self.board,
                    piece.half_width,
                    piece.cl_class,
                    piece.layer,
                );
                let new_polyline = {
                    let mut checker = self.checker();
                    let result = checker.spring_over(
                        &piece.polyline,
                        half_width,
                        layer,
                        &piece.nets,
                        piece.cl_class,
                        false,
                        max_spring_over_recursion_depth,
                    );
                    let blame = checker.failing_obstacle;
                    self.blame(blame);
                    result
                };
                let Some(new_polyline) = new_polyline else {
                    return false;
                };
                if new_polyline != piece.polyline {
                    max_spring_over_recursion_depth -= 1;
                    piece.polyline = new_polyline;
                }
            }
            for i in 0..piece.tile_shape_count() {
                let Some(curr) = ShapeAndFromSide::new(self.board, &piece, i, orthogonal, false)
                else {
                    continue;
                };
                if !self.insert(
                    &curr.shape,
                    curr.from_side,
                    layer,
                    &piece.nets,
                    piece.cl_class,
                    ignore_items,
                    max_recursion_depth - 1,
                    max_via_recursion_depth,
                    max_spring_over_recursion_depth,
                ) {
                    return false;
                }
            }
            if !self.materialize_piece(&piece, layer, tails_exist_before) {
                return false;
            }
        }
        true
    }

    /// The tail of `ShoveTraceAlgo.insert` (`:386-417`) and of
    /// `ForcedPadAlgo.forced_pad` (`:301-320`), which are token-identical:
    /// join the changed area, insert the piece, normalize it against the clip,
    /// and — only when the obstacles had no tails before — remove any tail the
    /// insertion created.
    pub(crate) fn materialize_piece(
        &mut self,
        piece: &SubstitutePiece,
        layer: i32,
        tails_exist_before: bool,
    ) -> bool {
        for i in 0..piece.polyline.corner_count() {
            self.changed
                .join_point(piece.polyline.corner_approx(i), layer);
        }
        let end_corners = if tails_exist_before {
            None
        } else {
            Some([piece.polyline.first_corner(), piece.polyline.last_corner()])
        };
        let Some(trace_id) = self.board.insert_trace_without_cleaning(
            piece.polyline.clone(),
            layer,
            piece.half_width,
            piece.nets.clone(),
            piece.cl_class,
            FixedState::Unfixed,
        ) else {
            return true;
        };
        let clip = self.changed.area(layer);
        self.board.normalize_trace(trace_id, clip.as_ref());

        if let Some(end_corners) = end_corners {
            for corner in end_corners {
                if let Some(tail) = self.trace_tail_at(&corner, layer, &piece.nets) {
                    self.board.remove_item(tail);
                    for net_no in piece.nets.iter() {
                        self.board.combine_traces(*net_no);
                    }
                }
            }
        }
        true
    }

    /// `BasicBoard.get_trace_tail(point, layer, net_no_arr)` (`:1857`): the
    /// dangling trace end at `point`, i.e. one with no other contact there.
    fn trace_tail_at(&self, corner: &Point, layer: i32, nets: &NetNos) -> Option<ItemId> {
        let mut picked = Vec::new();
        self.board.pick_items(corner, Some(layer), &mut picked);
        let mut contacts = Vec::new();
        for id in picked {
            let item = self.board.store().get(id)?;
            let ItemKind::Trace(trace) = &item.kind else {
                continue;
            };
            if !item.nets.shares_any(nets) {
                continue;
            }
            if trace.first_corner() != *corner && trace.last_corner() != *corner {
                continue;
            }
            self.board.normal_contacts_at(id, corner, &mut contacts);
            if contacts.is_empty() {
                return Some(id);
            }
        }
        None
    }
}

/// `ShoveTraceAlgo.get_ignore_items_at_tie_pins` (`:420-431`) applied as a
/// filter: every contact, on this layer, of an own-net pin overlapping the
/// shape is dropped from the obstacle list, which is what lets a shove cross a
/// tie pin.
fn remove_ignore_items_at_tie_pins(
    board: &Board,
    trace_shape: &Simplex,
    layer: i32,
    net_no_arr: &NetNos,
    obstacles: &mut Vec<ItemId>,
) {
    let mut overlaps = Vec::new();
    board.overlapping_items(
        &crate::shape::TileShape::Simplex(trace_shape.clone()),
        Some(layer),
        &[],
        &mut overlaps,
    );
    let mut ignore = Vec::new();
    let mut contacts = Vec::new();
    for id in overlaps {
        let Some(item) = board.store().get(id) else {
            continue;
        };
        let ItemKind::Pin(pin) = &item.kind else {
            continue;
        };
        if !item.nets.shares_any(net_no_arr) {
            continue;
        }
        board.normal_contacts_at(id, &Point::Int(pin.center), &mut contacts);
        ignore.extend(contacts.iter().copied());
    }
    if ignore.is_empty() {
        return;
    }
    obstacles.retain(|id| !ignore.contains(id));
}

/// `Item.bounding_box()` — the union of the item's tree shapes.
fn item_bounding_box(board: &Board, id: ItemId) -> IntBox {
    let Some(slot) = board.store().slot_of(id) else {
        return IntBox::EMPTY;
    };
    let mut result = IntBox::EMPTY;
    for tile in board.default_tree().item_shapes(slot, id) {
        if let Some(shape) = tile.shape.as_ref() {
            let curr = shape.bounding_box();
            result = if result.is_empty() {
                curr
            } else {
                result.union(&curr)
            };
        }
    }
    result
}

/// The points an item can be contacted at, upstream's `Item.get_all_contacts`
/// domain: trace end corners and drill centres.
fn connection_points(board: &Board, id: ItemId) -> Vec<Point> {
    let Some(item) = board.store().get(id) else {
        return Vec::new();
    };
    match &item.kind {
        ItemKind::Trace(trace) => vec![trace.first_corner(), trace.last_corner()],
        ItemKind::Pin(pin) => vec![Point::Int(pin.center)],
        ItemKind::Via(via) => vec![Point::Int(via.center)],
        _ => Vec::new(),
    }
}

/// `Polyline.reverse()`: the lines back to front, each flipped.
pub fn reverse_polyline(polyline: &Polyline) -> Polyline {
    let reversed: Vec<Line> = polyline
        .arr
        .iter()
        .rev()
        .map(|line| line.opposite())
        .collect();
    Polyline::new_from_lines(&reversed)
}

/// `FloatPoint` is reached through the changed-area sink only; keep the import
/// honest.
const _: fn(&Polyline, usize) -> FloatPoint = |p, i| p.corner_approx(i);
