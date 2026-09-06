//! `board/ForcedViaAlgo.java` (333 L) — forced via insertion, including the
//! release's hole-clearance probe (spec §13.2) and the extra start-trace circle
//! check of conformance deltas **B3** / **B4** / **B5**.
//!
//! Only the geometry-driven half lives here. Resolving a `ViaInfo` to per-layer
//! pad shapes needs `fr-rules`, which `fr-board` does not depend on, so the
//! router hands in a ready [`ViaLayerPad`] list.

use fr_geometry::planar::{Circle, FloatPoint, IntPoint, Point, Simplex};

use super::from_side::FromSide;
use super::pad::CheckDrillResult;
use super::{ShoveCheck, ShoveInsert};
use crate::board::Board;
use crate::clearance::ClearanceSource;
use crate::net_nos::NetNos;
use crate::shape::TileShape;

/// One padstack layer of a via, already translated to the insertion point.
#[derive(Debug, Clone)]
pub struct ViaLayerPad {
    /// The copper layer.
    pub layer: i32,
    /// The pad tile shape, snapped to the board's angle restriction.
    pub shape: Simplex,
    /// The clearance class the pad is checked with; `0` for the hole-only
    /// substitute shape of a copper-less layer (§13.2).
    pub clearance_class: i32,
    /// The half width of the trace that will start at this via, or 0. Drives
    /// the extra start-trace circle check (`:210-237`).
    pub trace_pen_half_width: i32,
}

/// A circle snapped to the board's angle restriction — `Circle.bounding_box()`
/// at 90°, `Circle.bounding_octagon()` otherwise.
pub fn circle_tile_shape(center: IntPoint, radius: i32, orthogonal: bool) -> TileShape {
    let circle = Circle::new(center, radius);
    if orthogonal {
        TileShape::Box(circle.bounding_box())
    } else {
        TileShape::Oct(circle.bounding_octagon())
    }
}

pub fn circle_tile(center: IntPoint, radius: i32, orthogonal: bool) -> Simplex {
    circle_tile_shape(center, radius, orthogonal).to_simplex()
}

/// `ForcedViaAlgo.hole_check_shape(padstack, location, board)` (`:240-252`).
///
/// `Circle(center, ceil(drill_radius + hole_clearance + 10))`, checked with
/// clearance class **0** so the requirement is exact hole-to-copper spacing
/// regardless of the neighbour's class.
pub fn hole_check_shape(
    center: IntPoint,
    drill_radius: f64,
    hole_clearance: i32,
    orthogonal: bool,
) -> Option<Simplex> {
    if hole_clearance <= 0 || drill_radius <= 0.0 {
        return None;
    }
    let radius = (drill_radius + hole_clearance as f64 + 10.0).ceil() as i32;
    Some(circle_tile(center, radius, orthogonal))
}

/// `ForcedViaAlgo.calculate_from_side` (`:254-333`): probe the four axis
/// directions (then the four diagonals at 45°) for a point still inside the
/// room shape.
pub fn calculate_from_side(
    via_location: &FloatPoint,
    via_shape: &Simplex,
    room_shape: &Simplex,
    dist: f64,
    is_90_degree: bool,
) -> Option<FromSide> {
    let via_box = via_shape.bounding_box();
    for i in 0..4 {
        let (check_point, border_x, border_y) = match i {
            0 => (
                FloatPoint::new(via_location.x, via_location.y - dist),
                via_location.x,
                via_box.ll.y as f64,
            ),
            1 => (
                FloatPoint::new(via_location.x + dist, via_location.y),
                via_box.ur.x as f64,
                via_location.y,
            ),
            2 => (
                FloatPoint::new(via_location.x, via_location.y + dist),
                via_location.x,
                via_box.ur.y as f64,
            ),
            _ => (
                FloatPoint::new(via_location.x - dist, via_location.y),
                via_box.ll.x as f64,
                via_location.y,
            ),
        };
        if room_shape.contains_float(&check_point, 0.0) {
            let from_side_no = if is_90_degree { i } else { 2 * i };
            return Some(FromSide::raw(
                from_side_no,
                Some(FloatPoint::new(border_x, border_y)),
            ));
        }
    }
    if is_90_degree {
        return None;
    }
    let diagonal_dist = dist / std::f64::consts::SQRT_2;
    let border_dist = via_box.max_width() / (2.0 * std::f64::consts::SQRT_2);
    for i in 0..4 {
        let (dx, dy) = match i {
            0 => (1.0, -1.0),
            1 => (1.0, 1.0),
            2 => (-1.0, 1.0),
            _ => (-1.0, -1.0),
        };
        let check_point = FloatPoint::new(
            via_location.x + dx * diagonal_dist,
            via_location.y + dy * diagonal_dist,
        );
        if room_shape.contains_float(&check_point, 0.0) {
            let border_point = FloatPoint::new(
                via_location.x + dx * border_dist,
                via_location.y + dy * border_dist,
            );
            return Some(FromSide::raw(2 * i + 1, Some(border_point)));
        }
    }
    None
}

impl ShoveCheck<'_> {
    /// `ForcedViaAlgo.check_layer(...)` (`:28-84`) — the maze drill machinery's
    /// per-layer question, **with conformance delta B3**: after the via pad
    /// passes, a `Circle(location, trace_half_width)` is checked too, with the
    /// trace clearance class and copper sharing allowed, and the result is
    /// `DrillableWithAttachSmd` when *either* sub-check says so.
    ///
    /// The caller supplies `required_radius` already resolved as
    /// `max(0.5 * via_shape.max_width(), ctrl.trace_half_width[layer])`
    /// (delta B4, `MazeSearchAlgo.java:1193`).
    #[allow(clippy::too_many_arguments)]
    pub fn check_via_layer(
        &mut self,
        via_radius: f64,
        cl_class: i32,
        attach_smd_allowed: bool,
        room_shape: &Simplex,
        location: IntPoint,
        layer: i32,
        net_no_arr: &NetNos,
        max_recursion_depth: i32,
        max_via_recursion_depth: i32,
        trace_half_width: i32,
        trace_clearance_class: i32,
    ) -> CheckDrillResult {
        if via_radius <= 0.0 {
            return CheckDrillResult::Drillable;
        }
        let orthogonal = self.orthogonal;
        let tile_shape = circle_tile_shape(location, via_radius.ceil() as i32, orthogonal);
        let tile_simplex = tile_shape.to_simplex();
        let check_radius = via_radius
            + 0.5 * self.board.clearances().value(cl_class, cl_class, layer) as f64
            + self.board.min_trace_half_width() as f64;
        let Some(from_side) = calculate_from_side(
            &location.to_float(),
            &tile_simplex,
            room_shape,
            check_radius,
            orthogonal,
        ) else {
            return CheckDrillResult::NotDrillable;
        };

        let via_result = self.check_forced_pad(
            &tile_shape,
            Some(from_side),
            layer,
            net_no_arr,
            cl_class,
            attach_smd_allowed,
            &[],
            max_recursion_depth,
            max_via_recursion_depth,
            false,
        );
        if via_result == CheckDrillResult::NotDrillable || trace_half_width <= 0 {
            return via_result;
        }
        let start_trace_shape = circle_tile_shape(location, trace_half_width, orthogonal);
        let trace_result = self.check_forced_pad(
            &start_trace_shape,
            Some(from_side),
            layer,
            net_no_arr,
            trace_clearance_class,
            true,
            &[],
            max_recursion_depth,
            max_via_recursion_depth,
            false,
        );
        if trace_result == CheckDrillResult::NotDrillable {
            return trace_result;
        }
        if via_result == CheckDrillResult::DrillableWithAttachSmd
            || trace_result == CheckDrillResult::DrillableWithAttachSmd
        {
            CheckDrillResult::DrillableWithAttachSmd
        } else {
            CheckDrillResult::Drillable
        }
    }

    /// `ForcedViaAlgo.check(via_info, location, …)` (`:89-158`) over pads the
    /// caller already resolved, including the hole-clearance probe and the
    /// start-trace circle (delta B5).
    ///
    /// Returns the failing layer on failure, upstream's `set_shove_failing_layer`.
    #[allow(clippy::too_many_arguments)]
    pub fn check_forced_via(
        &mut self,
        pads: &[ViaLayerPad],
        location: IntPoint,
        net_no_arr: &NetNos,
        attach_smd_allowed: bool,
        hole_shape: Option<&Simplex>,
        trace_clearance_class_no: i32,
        max_recursion_depth: i32,
        max_via_recursion_depth: i32,
    ) -> Result<(), i32> {
        let calc_from_side_offset = self.board.min_trace_half_width();
        let orthogonal = self.orthogonal;
        for pad in pads {
            let from_side = self.calc_pad_from_side(
                &pad.shape,
                &Point::Int(location),
                pad.layer,
                calc_from_side_offset,
                pad.clearance_class,
            );
            let pad_tile = TileShape::Simplex(pad.shape.clone());
            if self.check_forced_pad(
                &pad_tile,
                Some(from_side),
                pad.layer,
                net_no_arr,
                pad.clearance_class,
                attach_smd_allowed,
                &[],
                max_recursion_depth,
                max_via_recursion_depth,
                false,
            ) == CheckDrillResult::NotDrillable
            {
                return Err(pad.layer);
            }
            if pad.clearance_class != 0 {
                if let Some(hole_shape) = hole_shape {
                    let hole_tile = TileShape::Simplex(hole_shape.clone());
                    if self.check_forced_pad(
                        &hole_tile,
                        Some(from_side),
                        pad.layer,
                        net_no_arr,
                        0,
                        attach_smd_allowed,
                        &[],
                        max_recursion_depth,
                        max_via_recursion_depth,
                        false,
                    ) == CheckDrillResult::NotDrillable
                    {
                        return Err(pad.layer);
                    }
                }
            }
            if pad.trace_pen_half_width > 0 {
                let start_trace_shape = circle_tile_shape(location, pad.trace_pen_half_width, orthogonal);
                if self.check_forced_pad(
                    &start_trace_shape,
                    Some(from_side),
                    pad.layer,
                    net_no_arr,
                    trace_clearance_class_no,
                    true,
                    &[],
                    max_recursion_depth,
                    max_via_recursion_depth,
                    false,
                ) == CheckDrillResult::NotDrillable
                {
                    return Err(pad.layer);
                }
            }
        }
        Ok(())
    }
}

impl ShoveInsert<'_> {
    /// `ForcedViaAlgo.insert(...)` (`:165-232`) minus the final `insert_via`,
    /// which the router does because it owns the padstack table.
    ///
    /// The start-trace circle is shoved too — *"necessary in case
    /// start_trace_shape is bigger than tile_shape"* (`:218-224`).
    #[allow(clippy::too_many_arguments)]
    pub fn forced_via_pads(
        &mut self,
        pads: &[ViaLayerPad],
        location: IntPoint,
        net_no_arr: &NetNos,
        attach_smd_allowed: bool,
        hole_shape: Option<&Simplex>,
        trace_clearance_class_no: i32,
        max_recursion_depth: i32,
        max_via_recursion_depth: i32,
    ) -> Result<(), i32> {
        let calc_from_side_offset = self.board.min_trace_half_width();
        let orthogonal = self.orthogonal;
        for pad in pads {
            let from_side = self.checker().calc_pad_from_side(
                &pad.shape,
                &Point::Int(location),
                pad.layer,
                calc_from_side_offset,
                pad.clearance_class,
            );
            if !self.forced_pad(
                &pad.shape,
                Some(from_side),
                pad.layer,
                net_no_arr,
                pad.clearance_class,
                attach_smd_allowed,
                &[],
                max_recursion_depth,
                max_via_recursion_depth,
            ) {
                return Err(pad.layer);
            }
            if pad.clearance_class != 0 {
                if let Some(hole_shape) = hole_shape {
                    if !self.forced_pad(
                        hole_shape,
                        Some(from_side),
                        pad.layer,
                        net_no_arr,
                        0,
                        attach_smd_allowed,
                        &[],
                        max_recursion_depth,
                        max_via_recursion_depth,
                    ) {
                        return Err(pad.layer);
                    }
                }
            }
            if pad.trace_pen_half_width > 0 {
                let start_trace_shape = circle_tile(location, pad.trace_pen_half_width, orthogonal);
                if !self.forced_pad(
                    &start_trace_shape,
                    Some(from_side),
                    pad.layer,
                    net_no_arr,
                    trace_clearance_class_no,
                    true,
                    &[],
                    max_recursion_depth,
                    max_via_recursion_depth,
                ) {
                    return Err(pad.layer);
                }
            }
        }
        Ok(())
    }
}

/// `Board` is reached through the two contexts; keep the import honest.
const _: fn(&Board) -> i32 = |board| board.min_trace_half_width();
