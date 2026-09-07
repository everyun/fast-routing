//! `board/OptViaAlgo.java` — optimizing via locations during pull-tight.
//!
//! Ported from the **2.3.0 release tree** (`app/freerouting/board/OptViaAlgo.java`).
//! Moves vias along connected traces to shorten routing length, reduce layer transitions,
//! and clear bottlenecks (such as pulling fanout vias close to SMD pins).

use fr_board::item::{ItemId, ItemKind};
use fr_board::shove::trace::check_trace_segment;
use fr_board::shove::{ShoveCheck, ShoveInsert, FULL_SHOVE};
use fr_board::{AngleRestriction, Board};
use fr_geometry::planar::{FloatLine, FloatPoint, IntPoint, LineSegment, Point, Polyline, Side};
use fr_rules::router_settings::ExpansionCostFactor;

use crate::insert_forced::ChangedArea;
use crate::pull_tight::PullTightAlgo;

impl<'b> PullTightAlgo<'b> {
    /// `OptViaAlgo.opt_via_location` (`:31-137`).
    ///
    /// Optimizes the location of a via connected to at most 2 traces according to
    /// the trace costs on the layers of the connected traces.
    /// If `trace_costs == None`, horizontal and vertical costs default to 1.0.
    /// Returns false if the via was not changed.
    pub fn opt_via_location(
        &mut self,
        via_id: ItemId,
        trace_costs: Option<&[ExpansionCostFactor]>,
        max_recursion_depth: i32,
        changed_area: &mut ChangedArea,
    ) -> bool {
        let Some(item) = self.board.store().get(via_id) else {
            return false;
        };
        if item.is_shove_fixed() {
            return false;
        }
        if max_recursion_depth <= 0 {
            return false;
        }
        let ItemKind::Via(via) = &item.kind else {
            return false;
        };
        let via_center = via.center;

        let mut contacts = Vec::new();
        self.board
            .normal_contacts_at(via_id, &Point::Int(via_center), &mut contacts);

        let is_plane_or_fanout_via = contacts.len() == 1;
        let mut first_trace_id = None;
        let mut second_trace_id = None;
        let mut is_plane_fanout = is_plane_or_fanout_via;

        if !is_plane_fanout {
            if contacts.len() != 2 {
                return false;
            }
            for &c_id in &contacts {
                let Some(c_item) = self.board.store().get(c_id) else {
                    return false;
                };
                if c_item.is_shove_fixed() || !matches!(c_item.kind, ItemKind::Trace(_)) {
                    if matches!(c_item.kind, ItemKind::ConductionArea(_)) {
                        is_plane_fanout = true;
                    } else {
                        return false;
                    }
                } else if first_trace_id.is_none() {
                    first_trace_id = Some(c_id);
                } else {
                    second_trace_id = Some(c_id);
                }
            }
        }

        if is_plane_fanout {
            return self.opt_plane_or_fanout_via(via_id, max_recursion_depth, changed_area);
        }

        let (Some(first_id), Some(second_id)) = (first_trace_id, second_trace_id) else {
            return false;
        };
        let (first_layer, first_hw, first_cl, first_first, first_last, first_count, first_poly) = {
            let item = self.board.store().get(first_id).unwrap();
            let ItemKind::Trace(t) = &item.kind else {
                return false;
            };
            (
                t.layer,
                t.half_width,
                item.clearance_class,
                polyline_corner_int(&t.polyline, 0),
                polyline_corner_int(&t.polyline, t.polyline.corner_count().saturating_sub(1)),
                t.polyline.corner_count(),
                t.polyline.clone(),
            )
        };
        let (
            second_layer,
            second_hw,
            second_cl,
            second_first,
            second_last,
            second_count,
            second_poly,
        ) = {
            let item = self.board.store().get(second_id).unwrap();
            let ItemKind::Trace(t) = &item.kind else {
                return false;
            };
            (
                t.layer,
                t.half_width,
                item.clearance_class,
                polyline_corner_int(&t.polyline, 0),
                polyline_corner_int(&t.polyline, t.polyline.corner_count().saturating_sub(1)),
                t.polyline.corner_count(),
                t.polyline.clone(),
            )
        };

        if first_count < 2 || second_count < 2 {
            return false;
        }

        let tolerance = (via_min_width(self.board, via_id) / 2.0) as i32 + 1;

        let first_trace_from_corner = if is_within_tolerance(first_first, via_center, tolerance) {
            polyline_corner_int(&first_poly, 1)
        } else if is_within_tolerance(first_last, via_center, tolerance) {
            polyline_corner_int(&first_poly, first_count - 2)
        } else {
            return false;
        };

        let second_trace_from_corner = if is_within_tolerance(second_first, via_center, tolerance) {
            polyline_corner_int(&second_poly, 1)
        } else if is_within_tolerance(second_last, via_center, tolerance) {
            polyline_corner_int(&second_poly, second_count - 2)
        } else {
            return false;
        };

        let default_cost = ExpansionCostFactor {
            horizontal: 1.0,
            vertical: 1.0,
        };
        let first_costs = trace_costs
            .and_then(|c| c.get(first_layer as usize))
            .copied()
            .unwrap_or(default_cost);
        let second_costs = trace_costs
            .and_then(|c| c.get(second_layer as usize))
            .copied()
            .unwrap_or(default_cost);

        let new_location = self.reposition_via_double(
            via_id,
            first_hw,
            first_cl,
            first_layer,
            first_costs,
            first_trace_from_corner,
            second_hw,
            second_cl,
            second_layer,
            second_costs,
            second_trace_from_corner,
        );

        let Some(new_loc) = new_location else {
            return false;
        };
        if new_loc == via_center {
            return false;
        }

        let delta = new_loc.difference_by_int(&via_center);
        let mut shove_insert = ShoveInsert::new(self.board, self.bounds, changed_area);
        if !shove_insert.move_drill_item(via_id, &delta, 9, 9) {
            return false;
        }

        // Pull tight picked traces at new location on first and second layer
        let mut picked = Vec::new();
        self.board
            .pick_items(&Point::Int(new_loc), Some(first_layer), &mut picked);
        for id in picked {
            if self
                .board
                .store()
                .get(id)
                .is_some_and(|it| matches!(it.kind, ItemKind::Trace(_)))
            {
                self.trace_pull_tight(id, changed_area);
            }
        }
        let mut picked2 = Vec::new();
        self.board
            .pick_items(&Point::Int(new_loc), Some(second_layer), &mut picked2);
        for id in picked2 {
            if self
                .board
                .store()
                .get(id)
                .is_some_and(|it| matches!(it.kind, ItemKind::Trace(_)))
            {
                self.trace_pull_tight(id, changed_area);
            }
        }

        // Recursive via optimization at new location
        let mut picked_vias = Vec::new();
        self.board
            .pick_items(&Point::Int(new_loc), Some(first_layer), &mut picked_vias);
        for id in picked_vias {
            if self
                .board
                .store()
                .get(id)
                .is_some_and(|it| matches!(it.kind, ItemKind::Via(_)))
            {
                self.opt_via_location(id, trace_costs, max_recursion_depth - 1, changed_area);
                break;
            }
        }

        true
    }

    /// `OptViaAlgo.opt_plane_or_fanout_via` (`:142-230`).
    ///
    /// Optimisations for vias with only 1 connected trace (plane or fanout vias).
    fn opt_plane_or_fanout_via(
        &mut self,
        via_id: ItemId,
        max_recursion_depth: i32,
        changed_area: &mut ChangedArea,
    ) -> bool {
        if max_recursion_depth <= 0 {
            return false;
        }
        let Some(item) = self.board.store().get(via_id) else {
            return false;
        };
        let ItemKind::Via(via) = &item.kind else {
            return false;
        };
        let via_center = via.center;

        let mut contacts = Vec::new();
        self.board
            .normal_contacts_at(via_id, &Point::Int(via_center), &mut contacts);
        if contacts.is_empty() {
            return false;
        }

        let mut contact_plane = None;
        let mut contact_trace = None;
        for &c_id in &contacts {
            let Some(c_item) = self.board.store().get(c_id) else {
                continue;
            };
            match &c_item.kind {
                ItemKind::ConductionArea(_) => {
                    if contact_plane.is_some() {
                        return false;
                    }
                    contact_plane = Some(c_id);
                }
                ItemKind::Trace(_) => {
                    if c_item.is_shove_fixed() || contact_trace.is_some() {
                        return false;
                    }
                    contact_trace = Some(c_id);
                }
                _ => return false,
            }
        }

        let Some(trace_id) = contact_trace else {
            return false;
        };
        let (trace_layer, trace_hw, trace_cl, trace_first, trace_last, trace_count, trace_poly) = {
            let item = self.board.store().get(trace_id).unwrap();
            let ItemKind::Trace(t) = &item.kind else {
                return false;
            };
            (
                t.layer,
                t.half_width,
                item.clearance_class,
                polyline_corner_int(&t.polyline, 0),
                polyline_corner_int(&t.polyline, t.polyline.corner_count().saturating_sub(1)),
                t.polyline.corner_count(),
                t.polyline.clone(),
            )
        };

        if trace_count < 2 {
            return false;
        }

        let tolerance = (via_min_width(self.board, via_id) / 2.0) as i32 + 1;

        let at_first_corner;
        let check_corner;
        if is_within_tolerance(trace_first, via_center, tolerance) {
            at_first_corner = true;
            check_corner = polyline_corner_int(&trace_poly, 1);
        } else if is_within_tolerance(trace_last, via_center, tolerance) {
            at_first_corner = false;
            check_corner = polyline_corner_int(&trace_poly, trace_count - 2);
        } else {
            return false;
        }

        let mut new_via_location =
            self.reposition_via_single(via_id, check_corner, trace_hw, trace_layer, trace_cl);

        // Try projecting the via to the previous line if corner count >= 3
        if new_via_location.is_none() && trace_count >= 3 {
            let prev_corner = if at_first_corner {
                polyline_corner_int(&trace_poly, 2)
            } else {
                polyline_corner_int(&trace_poly, trace_count - 3)
            };
            let float_check_corner = check_corner.to_float();
            let float_via_center = via_center.to_float();
            let float_prev_corner = prev_corner.to_float();

            if float_check_corner.scalar_product(&float_via_center, &float_prev_corner) != 0.0 {
                let curr_line = FloatLine::new(float_check_corner, float_prev_corner);
                let projection = curr_line
                    .perpendicular_projection(&float_via_center)
                    .round();
                let diff_vector = projection.difference_by_int(&via_center);
                let mut projection_ok = projection != via_center;
                match self.board.angle_restriction() {
                    AngleRestriction::Ninety if !diff_vector.is_orthogonal() => {
                        projection_ok = false;
                    }
                    AngleRestriction::FortyFive
                        if !diff_vector.is_orthogonal() && !diff_vector.is_diagonal() =>
                    {
                        projection_ok = false;
                    }
                    _ => {}
                }
                if projection_ok {
                    let mut shove_check = ShoveCheck::new(self.board, self.bounds);
                    let mut ignore = Vec::new();
                    if shove_check.check_move_drill_item(via_id, &diff_vector, 0, 0, &mut ignore) {
                        let line_seg = LineSegment::from_points(
                            &Point::Int(via_center),
                            &Point::Int(projection),
                        );
                        let nets = self.board.store().get(via_id).unwrap().nets.clone();
                        let ok_length = check_trace_segment(
                            self.board,
                            &line_seg,
                            trace_layer,
                            &nets,
                            trace_hw,
                            trace_cl,
                            false,
                        );
                        if ok_length >= FULL_SHOVE {
                            new_via_location = Some(projection);
                        }
                    }
                }
            }
        }

        let Some(new_via_loc) = new_via_location else {
            return false;
        };

        if let Some(plane_id) = contact_plane {
            let Some(plane_item) = self.board.store().get(plane_id) else {
                return false;
            };
            let ItemKind::ConductionArea(area) = &plane_item.kind else {
                return false;
            };
            let mut picked = Vec::new();
            self.board
                .pick_items(&Point::Int(new_via_loc), Some(area.layer), &mut picked);
            if !picked.contains(&plane_id) {
                return false;
            }
        }

        let diff_vector = new_via_loc.difference_by_int(&via_center);
        let mut shove_insert = ShoveInsert::new(self.board, self.bounds, changed_area);
        if !shove_insert.move_drill_item(via_id, &diff_vector, 9, 9) {
            return false;
        }

        let mut picked = Vec::new();
        self.board
            .pick_items(&Point::Int(new_via_loc), Some(trace_layer), &mut picked);
        for id in picked {
            if self
                .board
                .store()
                .get(id)
                .is_some_and(|it| matches!(it.kind, ItemKind::Trace(_)))
            {
                self.trace_pull_tight(id, changed_area);
            }
        }

        if new_via_loc == check_corner {
            self.opt_plane_or_fanout_via(via_id, max_recursion_depth - 1, changed_area);
        }

        true
    }

    /// `OptViaAlgo.reposition_via` (single destination, `:232-277`).
    ///
    /// Tries to move the via into the direction of `to_location` as far as possible.
    fn reposition_via_single(
        &self,
        via_id: ItemId,
        to_location: IntPoint,
        trace_half_width: i32,
        trace_layer: i32,
        trace_cl_class: i32,
    ) -> Option<IntPoint> {
        let from_location = self
            .board
            .store()
            .get(via_id)
            .and_then(|it| match &it.kind {
                ItemKind::Via(v) => Some(v.center),
                _ => None,
            })?;

        if from_location == to_location {
            return None;
        }

        let via_nets = self.board.store().get(via_id).unwrap().nets.clone();
        let line_seg =
            LineSegment::from_points(&Point::Int(from_location), &Point::Int(to_location));
        let ok_length = check_trace_segment(
            self.board,
            &line_seg,
            trace_layer,
            &via_nets,
            trace_half_width,
            trace_cl_class,
            false,
        );
        if ok_length <= 0.0 {
            return None;
        }

        let float_from = from_location.to_float();
        let float_to = to_location.to_float();
        let new_float = if ok_length >= FULL_SHOVE {
            float_to
        } else {
            float_from.change_length(&float_to, ok_length)
        };
        let new_to_location = new_float.round();
        let delta = new_to_location.difference_by_int(&from_location);
        let mut shove_check = ShoveCheck::new(self.board, self.bounds);
        let mut ignore = Vec::new();
        if shove_check.check_move_drill_item(via_id, &delta, 0, 0, &mut ignore) {
            return Some(new_to_location);
        }

        // Binary search along the segment for the furthest valid point
        let c_min_length = 0.3 * f64::from(trace_half_width) + 1.0;
        let total_dist = float_from.distance(&float_to);
        let max_ok = ok_length.min(total_dist);
        let mut curr_length = max_ok / 2.0;
        let mut accum = 0.0;
        let mut result = None;

        while curr_length >= c_min_length {
            let check_point = float_from
                .change_length(&float_to, accum + curr_length)
                .round();
            let check_delta = check_point.difference_by_int(&from_location);
            let mut check = ShoveCheck::new(self.board, self.bounds);
            let mut ignore_items = Vec::new();
            if check.check_move_drill_item(via_id, &check_delta, 0, 0, &mut ignore_items) {
                accum += curr_length;
                result = Some(check_point);
            }
            curr_length /= 2.0;
        }

        result
    }

    /// `OptViaAlgo.reposition_via` (two segments check, `:280-315`).
    #[allow(clippy::too_many_arguments)]
    fn reposition_via_two_segments(
        &self,
        via_id: ItemId,
        to_location: IntPoint,
        hw1: i32,
        layer1: i32,
        cl1: i32,
        connect_location: IntPoint,
        hw2: i32,
        layer2: i32,
        cl2: i32,
    ) -> bool {
        let from_location = self
            .board
            .store()
            .get(via_id)
            .and_then(|it| match &it.kind {
                ItemKind::Via(v) => Some(v.center),
                _ => None,
            });
        let Some(from_location) = from_location else {
            return false;
        };

        if from_location == to_location {
            return false;
        }

        let delta = to_location.difference_by_int(&from_location);
        if self.board.angle_restriction() == AngleRestriction::None
            && delta.to_float().size() <= 1.5
        {
            return false;
        }

        let via_nets = self.board.store().get(via_id).unwrap().nets.clone();
        let seg1 = LineSegment::from_points(&Point::Int(from_location), &Point::Int(to_location));
        if check_trace_segment(self.board, &seg1, layer1, &via_nets, hw1, cl1, false) < FULL_SHOVE {
            return false;
        }

        let seg2 =
            LineSegment::from_points(&Point::Int(to_location), &Point::Int(connect_location));
        if check_trace_segment(self.board, &seg2, layer2, &via_nets, hw2, cl2, false) < FULL_SHOVE {
            return false;
        }

        let mut shove_check = ShoveCheck::new(self.board, self.bounds);
        let mut ignore = Vec::new();
        shove_check.check_move_drill_item(via_id, &delta, 0, 0, &mut ignore)
    }

    /// `OptViaAlgo.reposition_via` (two connected traces, `:323-552`).
    #[allow(clippy::too_many_arguments)]
    fn reposition_via_double(
        &self,
        via_id: ItemId,
        hw1: i32,
        cl1: i32,
        layer1: i32,
        costs1: ExpansionCostFactor,
        from1: IntPoint,
        hw2: i32,
        cl2: i32,
        layer2: i32,
        costs2: ExpansionCostFactor,
        from2: IntPoint,
    ) -> Option<IntPoint> {
        let via_location = self
            .board
            .store()
            .get(via_id)
            .and_then(|it| match &it.kind {
                ItemKind::Via(v) => Some(v.center),
                _ => None,
            })?;

        let first_delta = from1.difference_by_int(&via_location);
        let second_delta = from2.difference_by_int(&via_location);
        let scalar_product = first_delta.scalar_product_int(&second_delta);

        let float_via = via_location.to_float();
        let float_from1 = from1.to_float();
        let float_from2 = from2.to_float();
        let dist1 = float_via.distance(&float_from1);
        let dist2 = float_via.distance(&float_from2);

        // Case of overlapping collinear lines
        let side = Point::Int(via_location).side_of_points(&Point::Int(from1), &Point::Int(from2));
        if side == Side::Collinear && scalar_product > 0.0 {
            if dist2 < dist1 {
                return self.reposition_via_single(via_id, from2, hw1, layer1, cl1);
            }
            return self.reposition_via_single(via_id, from1, hw2, layer2, cl2);
        }

        let curr_wdist1 =
            float_via.weighted_distance(&float_from1, costs1.horizontal, costs1.vertical);
        let curr_wdist2 =
            float_via.weighted_distance(&float_from1, costs2.horizontal, costs2.vertical);
        if curr_wdist1 > curr_wdist2 {
            if let Some(res) = self.reposition_via_single(via_id, from1, hw2, layer2, cl2) {
                return Some(res);
            }
        }

        let curr_wdist1_b =
            float_via.weighted_distance(&float_from2, costs2.horizontal, costs2.vertical);
        let curr_wdist2_b =
            float_via.weighted_distance(&float_from2, costs1.horizontal, costs1.vertical);
        if curr_wdist1_b > curr_wdist2_b {
            if let Some(res) = self.reposition_via_single(via_id, from2, hw1, layer1, cl1) {
                return Some(res);
            }
        }

        // Acute angle case
        if scalar_product > 0.0 && self.board.angle_restriction() != AngleRestriction::Ninety {
            let (to_1, to_2, float_to_1, float_to_2) = if dist1 < dist2 {
                let to2_float = float_via.change_length(&float_from2, dist1);
                (from1, to2_float.round(), float_from1, to2_float)
            } else {
                let to1_float = float_via.change_length(&float_from1, dist2);
                (to1_float.round(), from2, to1_float, float_from2)
            };
            let wdist_a =
                float_to_1.weighted_distance(&float_to_2, costs1.horizontal, costs1.vertical);
            let wdist_b =
                float_to_1.weighted_distance(&float_to_2, costs2.horizontal, costs2.vertical);
            if wdist_a > wdist_b {
                if let Some(res) = self.reposition_via_single(via_id, to_1, hw2, layer2, cl2) {
                    return Some(res);
                }
                if let Some(res) = self.reposition_via_single(via_id, to_2, hw1, layer1, cl1) {
                    return Some(res);
                }
            } else {
                if let Some(res) = self.reposition_via_single(via_id, to_2, hw1, layer1, cl1) {
                    return Some(res);
                }
                if let Some(res) = self.reposition_via_single(via_id, to_1, hw2, layer2, cl2) {
                    return Some(res);
                }
            }
        }

        // Axis-parallel decomposition for first delta
        if !first_delta.is_orthogonal() {
            let float_check_loc = FloatPoint::new(float_via.x, float_from1.y);
            let wdist1 =
                float_via.weighted_distance(&float_from1, costs1.horizontal, costs1.vertical);
            let wdist2 =
                float_via.weighted_distance(&float_check_loc, costs2.horizontal, costs2.vertical);
            let wdist3 =
                float_check_loc.weighted_distance(&float_from1, costs1.horizontal, costs1.vertical);
            if wdist1 > wdist2 + wdist3 {
                let check_loc = float_check_loc.round();
                if self.reposition_via_two_segments(
                    via_id, check_loc, hw2, layer2, cl2, from1, hw1, layer1, cl1,
                ) {
                    return Some(check_loc);
                }
            }

            let float_check_loc2 = FloatPoint::new(float_from1.x, float_via.y);
            let wdist2_b =
                float_via.weighted_distance(&float_check_loc2, costs2.horizontal, costs2.vertical);
            let wdist3_b = float_check_loc2.weighted_distance(
                &float_from1,
                costs1.horizontal,
                costs1.vertical,
            );
            if wdist1 > wdist2_b + wdist3_b {
                let check_loc = float_check_loc2.round();
                if self.reposition_via_two_segments(
                    via_id, check_loc, hw2, layer2, cl2, from1, hw1, layer1, cl1,
                ) {
                    return Some(check_loc);
                }
            }
        }

        // Axis-parallel decomposition for second delta
        if !second_delta.is_orthogonal() {
            let float_check_loc = FloatPoint::new(float_via.x, float_from2.y);
            let wdist1 =
                float_via.weighted_distance(&float_from2, costs2.horizontal, costs2.vertical);
            let wdist2 =
                float_via.weighted_distance(&float_check_loc, costs1.horizontal, costs1.vertical);
            let wdist3 =
                float_check_loc.weighted_distance(&float_from2, costs2.horizontal, costs2.vertical);
            if wdist1 > wdist2 + wdist3 {
                let check_loc = float_check_loc.round();
                if self.reposition_via_two_segments(
                    via_id, check_loc, hw1, layer1, cl1, from2, hw2, layer2, cl2,
                ) {
                    return Some(check_loc);
                }
            }

            let float_check_loc2 = FloatPoint::new(float_from2.x, float_via.y);
            let wdist2_b =
                float_via.weighted_distance(&float_check_loc2, costs1.horizontal, costs1.vertical);
            let wdist3_b = float_check_loc2.weighted_distance(
                &float_from2,
                costs2.horizontal,
                costs2.vertical,
            );
            if wdist1 > wdist2_b + wdist3_b {
                let check_loc = float_check_loc2.round();
                if self.reposition_via_two_segments(
                    via_id, check_loc, hw1, layer1, cl1, from2, hw2, layer2, cl2,
                ) {
                    return Some(check_loc);
                }
            }
        }

        None
    }
}

/// Helper to get the integer coordinate of corner `i` of a polyline.
fn polyline_corner_int(p: &Polyline, i: usize) -> IntPoint {
    match p.corner(i) {
        Point::Int(ip) => ip,
        Point::Rational(_) => p.corner(i).to_float().round(),
    }
}

/// Checks if two points are within the specified tolerance distance.
/// Matches `OptViaAlgo.isWithinTolerance`: Manhattan distance `|dx| + |dy| <= tolerance`.
#[inline]
fn is_within_tolerance(p1: IntPoint, p2: IntPoint, tolerance: i32) -> bool {
    (p1.x - p2.x).abs() + (p1.y - p2.y).abs() <= tolerance
}

/// Minimal bounding box width of a via across all layers it spans.
/// Matches `DrillItem.min_width()`.
fn via_min_width(board: &Board, via_id: ItemId) -> f64 {
    let Some(item) = board.store().get(via_id) else {
        return 0.0;
    };
    let layers = item.layer_range();
    let mut min_w = f64::MAX;
    for layer in layers.first..=layers.last {
        if let Some(shape) = fr_board::shove::entries::tree_shape_on_layer(board, via_id, layer) {
            min_w = min_w.min(shape.bounding_box().min_width());
        }
    }
    if min_w == f64::MAX {
        0.0
    } else {
        min_w
    }
}
