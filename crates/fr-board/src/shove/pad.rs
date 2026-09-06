//! `board/ForcedPadAlgo.java` (351 L) — checking and inserting a pad, shoving
//! obstacle traces aside, plus the `in_front_of_pad` octagon predicate.

use fr_geometry::planar::{Direction, IntOctagon, Line, Point, Polyline, Simplex};

use super::entries::{contains_trace_tails, ShapeTraceEntries};
use super::from_side::FromSide;
use super::geom;
use super::shape_and_from_side::ShapeAndFromSide;
use super::{ShoveCheck, ShoveInsert};
use crate::item::{ItemId, ItemKind};
use crate::net_nos::NetNos;

/// `ForcedPadAlgo.CheckDrillResult` (`:349-351`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckDrillResult {
    /// The pad fits, possibly after shoving.
    Drillable,
    /// It fits, but it would land on a same-net SMD pad.
    DrillableWithAttachSmd,
    /// It does not fit.
    NotDrillable,
}

/// `ForcedPadAlgo.in_front_of_pad(line, pad_shape, from_side, width,
/// with_sides)` (`:48-136`).
///
/// Only implemented for octagonal pads with integer line points; anything else
/// answers "in front", which is the conservative direction (more segments get
/// checked, never fewer).
///
/// The case-0 typo `line_b.x + line_b.x` where `line_b.x + line_b.y` is clearly
/// intended (`:65`) ships in 2.3.0 and is preserved deliberately: it is a
/// pruning filter, so copying it keeps the set of checked segments identical to
/// upstream's, and "fixing" it would silently change routing results.
pub fn in_front_of_pad(
    line: &Line,
    pad_shape: &Simplex,
    from_side: i32,
    width: i32,
    with_sides: bool,
) -> bool {
    if !pad_shape.is_int_octagon() {
        return true;
    }
    let Some(pad) = pad_shape.bounding_octagon() else {
        return true;
    };
    let (a, b) = (line.a, line.b);
    let w = width as f64;
    let diag_width = w * std::f64::consts::SQRT_2;

    let min_y = a.y.min(b.y) as f64;
    let max_y = a.y.max(b.y) as f64;
    let min_x = a.x.min(b.x) as f64;
    let max_x = a.x.max(b.x) as f64;
    let min_diff = (a.x - a.y).min(b.x - b.y) as f64;
    let max_diff = (a.x - a.y).max(b.x - b.y) as f64;
    let min_sum = (a.x + a.y).min(b.x + b.y) as f64;
    let max_sum = (a.x + a.y).max(b.x + b.y) as f64;
    // The shipped typo, isolated so it reads as deliberate.
    let min_sum_case_0 = ((a.x + a.y) as f64).min((b.x + b.x) as f64);

    let IntOctagon {
        left_x,
        bottom_y,
        right_x,
        top_y,
        upper_left_diagonal_x: ulx,
        lower_right_diagonal_x: lrx,
        lower_left_diagonal_x: llx,
        upper_right_diagonal_x: urx,
    } = pad;
    let (lx, ly, rx, uy) = (left_x as f64, bottom_y as f64, right_x as f64, top_y as f64);
    let (ulx, lrx, llx, urx) = (ulx as f64, lrx as f64, llx as f64, urx as f64);

    let mut result = match from_side {
        0 => min_y >= uy + w || max_diff <= ulx - diag_width || min_sum_case_0 >= urx + diag_width,
        1 => min_y >= uy + w || max_diff <= ulx - diag_width || max_x <= lx - w,
        2 => max_x <= lx - w || max_diff <= ulx - diag_width || max_sum <= llx - diag_width,
        3 => max_x <= lx - w || max_y <= ly - w || max_sum <= llx - diag_width,
        4 => max_y <= ly - w || max_sum <= llx - diag_width || min_diff >= lrx + diag_width,
        5 => max_y <= ly - w || min_x >= rx + w || min_diff >= lrx + diag_width,
        6 => min_x >= rx + w || min_sum >= urx + diag_width || min_diff >= lrx + diag_width,
        7 => min_y >= uy + w || min_sum >= urx + diag_width || min_x >= rx + w,
        _ => return true,
    };
    if with_sides && !result {
        result = match from_side {
            0 => {
                max_x <= lx - w && min_diff <= ulx - diag_width
                    || min_x >= rx + w && min_sum >= urx + diag_width
            }
            1 => {
                min_x <= lx - w && max_sum <= llx - diag_width
                    || max_y >= uy + w && min_sum >= urx + diag_width
            }
            2 => {
                max_y <= ly - w && min_sum <= llx - diag_width
                    || min_y >= uy + w && min_diff <= ulx - diag_width
            }
            3 => {
                min_y <= ly - w && min_diff >= lrx + diag_width
                    || min_x <= lx - w && max_diff <= ulx - diag_width
            }
            4 => {
                min_x >= rx + w && max_diff >= lrx + diag_width
                    || max_x <= lx - w && min_sum <= llx - diag_width
            }
            5 => {
                max_x >= rx + w && min_sum >= urx + diag_width
                    || min_y <= ly - w && max_sum <= llx - diag_width
            }
            6 => {
                max_y <= ly - w && max_diff >= lrx + diag_width
                    || min_y >= uy + w && max_sum >= urx + diag_width
            }
            7 => {
                max_y >= uy + w && max_diff <= ulx - diag_width
                    || max_x >= rx + w && min_diff >= lrx + diag_width
            }
            _ => result,
        };
    }
    result
}

impl ShoveCheck<'_> {
    /// `ForcedPadAlgo.check_forced_pad(...)` (`:141-226`).
    ///
    /// Stricter than the trace check: there is no partial-length fallback, a
    /// via that cannot move is a hard failure.
    #[allow(clippy::too_many_arguments)]
    pub fn check_forced_pad(
        &mut self,
        pad_shape: &TileShape,
        from_side: Option<FromSide>,
        layer: i32,
        net_no_arr: &NetNos,
        cl_type: i32,
        copper_sharing_allowed: bool,
        ignore_items: &[ItemId],
        max_recursion_depth: i32,
        max_via_recursion_depth: i32,
        check_only_front: bool,
    ) -> CheckDrillResult {
        let pad_simplex = pad_shape.to_simplex();
        if !geom::simplex_contained_in(&pad_simplex, &self.bounds) {
            self.failing_obstacle = self.board_outline();
            return CheckDrillResult::NotDrillable;
        }
        let board = self.board;
        let mut shape_entries = ShapeTraceEntries::new(
            pad_simplex.clone(),
            layer,
            net_no_arr.clone(),
            cl_type,
            from_side,
        );
        let mut obstacles = self.scratch.take();
        geom::overlapping_items_with_clearance_tile(
            board,
            pad_shape,
            layer,
            &[],
            cl_type,
            &mut obstacles,
        );
        obstacles.retain(|id| !ignore_items.contains(id));
        if !shape_entries.store_items(board, &obstacles, true, copper_sharing_allowed) {
            let blame = shape_entries.found_obstacle();
            self.blame(blame);
            self.scratch.give(obstacles);
            return CheckDrillResult::NotDrillable;
        }

        let shove_vias: Vec<ItemId> = shape_entries.shove_via_list.clone();
        for via_id in shove_vias {
            if max_via_recursion_depth <= 0 {
                self.failing_obstacle = Some(via_id);
                self.scratch.give(obstacles);
                return CheckDrillResult::NotDrillable;
            }
            let new_via_center =
                super::drill::try_shove_via_points(board, &pad_simplex, layer, via_id, cl_type, false);
            let Some(&first) = new_via_center.first() else {
                self.failing_obstacle = Some(via_id);
                self.scratch.give(obstacles);
                return CheckDrillResult::NotDrillable;
            };
            let Some(via_item) = board.store().get(via_id) else {
                continue;
            };
            let ItemKind::Via(via) = &via_item.kind else {
                continue;
            };
            let delta = first.difference_by_int(&via.center);
            let mut cascade_ignore = Vec::new();
            if !self.check_move_drill_item(
                via_id,
                &delta,
                max_recursion_depth,
                max_via_recursion_depth - 1,
                &mut cascade_ignore,
            ) {
                self.scratch.give(obstacles);
                return CheckDrillResult::NotDrillable;
            }
        }

        let mut result = CheckDrillResult::Drillable;
        if copper_sharing_allowed
            && obstacles.iter().any(|id| {
                board
                    .store()
                    .get(*id)
                    .is_some_and(|item| matches!(item.kind, ItemKind::Pin(_)))
            })
        {
            result = CheckDrillResult::DrillableWithAttachSmd;
        }
        self.scratch.give(obstacles);

        let trace_piece_count = shape_entries.substitute_trace_count();
        if trace_piece_count == 0 {
            return result;
        }
        if max_recursion_depth <= 0 || shape_entries.stack_depth() > 1 {
            let blame = shape_entries.found_obstacle();
            self.blame(blame);
            return CheckDrillResult::NotDrillable;
        }

        let orthogonal = self.orthogonal;
        let from_side_no = from_side.map(|side| side.no).unwrap_or(-1);
        while let Some(piece) = shape_entries.next_substitute_trace_piece(board) {
            for i in 0..piece.tile_shape_count() {
                let curr_line = piece.polyline.arr[i + 1];
                let curr_dir = curr_line.direction();
                let is_in_front = if check_only_front {
                    in_front_of_pad(&curr_line, &pad_simplex, from_side_no, piece.half_width, true)
                } else {
                    true
                };
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
                    0,
                ) {
                    return CheckDrillResult::NotDrillable;
                }
            }
        }
        result
    }

    /// `ForcedPadAlgo.calc_from_side(shape, center, layer, offset, cl_class)`
    /// (`:329-347`): the first side whose probe from the centre is clear, with
    /// a second pass at clearance class 0.
    pub fn calc_pad_from_side(
        &self,
        shape: &Simplex,
        shape_center: &Point,
        layer: i32,
        offset: i32,
        cl_class: i32,
    ) -> FromSide {
        let offset_shape = shape.offset(offset as f64);
        for cl in [cl_class, 0] {
            for i in 0..offset_shape.border_line_count() {
                let Some(check_shape) =
                    calc_check_shape_for_from_side(shape_center, &offset_shape.border_line(i))
                else {
                    continue;
                };
                if self.check_trace_shape_clear(&check_shape, layer, &NetNos::default(), cl) {
                    return FromSide::raw(i as i32, None);
                }
            }
        }
        FromSide::NOT_CALCULATED
    }

    /// `BasicBoard.check_trace_shape(shape, layer, nets, cl_class, null)` —
    /// the plain "is anything in the way" probe the from-side search uses.
    pub fn check_trace_shape_clear(
        &self,
        shape: &Simplex,
        layer: i32,
        net_no_arr: &NetNos,
        cl_class: i32,
    ) -> bool {
        if !geom::simplex_contained_in(shape, &self.bounds) {
            return false;
        }
        let mut obstacles = Vec::new();
        geom::overlapping_items_with_clearance(
            self.board,
            shape,
            layer,
            net_no_arr.as_slice(),
            cl_class,
            &mut obstacles,
        );
        obstacles.is_empty()
    }
}

impl ShoveInsert<'_> {
    /// `ForcedPadAlgo.forced_pad(...)` (`:232-324`) — the destructive twin.
    #[allow(clippy::too_many_arguments)]
    pub fn forced_pad(
        &mut self,
        pad_shape: &Simplex,
        from_side: Option<FromSide>,
        layer: i32,
        net_no_arr: &NetNos,
        cl_type: i32,
        copper_sharing_allowed: bool,
        ignore_items: &[ItemId],
        max_recursion_depth: i32,
        max_via_recursion_depth: i32,
    ) -> bool {
        if pad_shape.is_empty() {
            return true;
        }
        if !geom::simplex_contained_in(pad_shape, &self.bounds) {
            self.failing_obstacle = self.checker().board_outline();
            return false;
        }
        if !self.shove_vias(
            pad_shape,
            from_side,
            layer,
            net_no_arr,
            cl_type,
            ignore_items,
            max_recursion_depth,
            max_via_recursion_depth,
            false,
        ) {
            return false;
        }
        let mut shape_entries = ShapeTraceEntries::new(
            pad_shape.clone(),
            layer,
            net_no_arr.clone(),
            cl_type,
            from_side,
        );
        let mut obstacles = self.scratch.take();
        geom::overlapping_items_with_clearance(
            self.board,
            pad_shape,
            layer,
            &[],
            cl_type,
            &mut obstacles,
        );
        obstacles.retain(|id| !ignore_items.contains(id));
        let obstacles_shovable =
            shape_entries.store_items(self.board, &obstacles, true, copper_sharing_allowed)
                && shape_entries.shove_via_list.is_empty();
        if !obstacles_shovable {
            let blame = shape_entries.found_obstacle();
            self.blame(blame);
            return false;
        }
        let trace_piece_count = shape_entries.substitute_trace_count();
        if trace_piece_count == 0 {
            return true;
        }
        if max_recursion_depth <= 0 {
            let blame = shape_entries.found_obstacle();
            self.blame(blame);
            return false;
        }
        let tails_exist_before = contains_trace_tails(self.board, &obstacles, net_no_arr);
        shape_entries.cutout_traces(self.board, &obstacles);
        self.scratch.give(obstacles);
        let orthogonal = self.orthogonal;

        while let Some(piece) = shape_entries.next_substitute_trace_piece(self.board) {
            if piece.polyline.first_corner() == piece.polyline.last_corner() {
                continue;
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
                    0,
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
}

/// `ForcedPadAlgo.calc_check_shape_for_from_side` (`:32-43`): a thin probe from
/// the shape centre toward one border line, offset by 1.
fn calc_check_shape_for_from_side(shape_center: &Point, border_line: &Line) -> Option<Simplex> {
    let center_float = geom::point_to_float(shape_center);
    let offset_projection = center_float.projection_approx(border_line);
    let curr_dir = border_line.direction();
    let center_int = match shape_center {
        Point::Int(p) => *p,
        Point::Rational(_) => center_float.round(),
    };
    let line_arr = [
        Line::new_from_point_and_dir(center_int, &curr_dir),
        Line::new_from_point_and_dir(center_int, &curr_dir.turn_45_degree(2)),
        Line::new_from_point_and_dir(offset_projection.round(), &curr_dir),
    ];
    let check_line = Polyline::new_from_lines(&line_arr);
    check_line.offset_shape(1, 0)
}

/// `Direction` is only reached through `Line::direction`; keep the import honest.
const _: fn(&Line) -> Direction = |line| line.direction();
