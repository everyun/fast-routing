//! `autoroute/MazeShoveTraceAlgo.java` (315 L) — the **virtual** shove the maze
//! runs during expansion.
//!
//! No board mutation happens here. The question is only *"could this obstacle
//! trace be pushed left/right, and if so which of its room's doors become
//! reachable"*; the answer is encoded in the expansion graph as door sections
//! plus [`Adjustment`]. If the path wins, `InsertFoundConnectionAlgo` later
//! makes the shove real through the generic trace shover
//! ([`fr_board::shove::trace`]), possibly slightly differently — which is why
//! insertion can still fail.
//!
//! # The restrictions, all deliberate (spec §15, "Keep semantically")
//!
//! * **same half width and same clearance class only** (`:42-47`). The
//!   maze-time model swaps our trace into the obstacle's corridor; a different
//!   width would invalidate the door geometry.
//! * **outermost door sections only** — enforced twice: by
//!   `MazeSearchAlgo.shove_trace_room` (section 0 or last) and again here for
//!   1-dim doors, where the section's `shape_entry` endpoint must be within
//!   `compensated_trace_half_width + 5` of the matching door corner.
//! * **no side flip after `Adjustment`** — an element that came through a
//!   shoved link door keeps shoving the same way. The tag itself is *not* set
//!   here: it is the caller's, keyed on the reached door's dimension, and its
//!   polarity is the opposite of the guard of the direction that produced it
//!   (`MazeSearch::shove_trace_room`). Getting that polarity backwards makes
//!   every link-door hop flip sides, which silently loses one connection on
//!   small boards.
//! * **fork exclusion** — [`end_points_matching`] for link doors, and the
//!   "two different obstacle items" door skip during harvesting.
//! * conformance delta **B1**: the stale-index guard is
//!   `trace_corner_no < 0 || trace_corner_no >= arr.len() - 2`, one index
//!   stricter than the spec body and without the warning. It is a real crash
//!   fix (v19 could index `arr[c + 2]` out of range) *and* it prunes one
//!   maze-time shove case v19 attempted, so it is ported as the release has it.

use fr_board::net_nos::NetNos;
use fr_board::shove::trace::{check_shove_segment, check_trace_segment};
use fr_geometry::planar::{
    ConvexShape, FloatLine, FloatPoint, IntBox, Line, LineSegment, Point, Side, Simplex,
};

use crate::engine::AutorouteEngine;
use crate::geom;
use crate::maze::queue::FrontierElement;
use crate::rooms::{DoorId, DoorKind, ExpandableRef, RoomId, RoomKind};

/// `MazeShoveTraceAlgo.DoorSection` (`:290-303`).
///
/// Deliberately *without* an adjustment field: upstream derives the
/// [`Adjustment`](crate::maze::nodes::Adjustment) in
/// `MazeSearchAlgo.shove_trace_room` (`:1501-1531`) from the
/// **reached door's dimension**, not from the branch of
/// [`check_shove_trace_line`] that harvested it. The two are not the same set —
/// a 2-dim door also reaches the 1-dim harvesting branch whenever
/// `shove_width < FULL_SHOVE` — so the decision has to stay at the caller.
#[derive(Debug, Clone, Copy)]
pub struct DoorSection {
    /// The door the shove makes reachable.
    pub door: DoorId,
    /// Which section of it.
    pub section_no: u16,
    /// The section's segment.
    pub section_line: FloatLine,
}

/// `MazeShoveTraceAlgo.check_shove_trace_line` (`:31-282`).
///
/// Returns **false** when "trying to shove from another door section may be
/// more successful"; the caller treats any `false` as "this direction found
/// nothing", not as an error. Reachable door sections are appended to
/// `to_door_list`.
pub fn check_shove_trace_line(
    engine: &mut AutorouteEngine<'_>,
    element: &FrontierElement,
    obstacle_room: RoomId,
    shove_to_the_left: bool,
    to_door_list: &mut Vec<DoorSection>,
) -> bool {
    let ExpandableRef::Door(from_door_id) = element.payload.door else {
        return true;
    };
    if !matches!(engine.rooms.door(from_door_id).kind, DoorKind::Room) {
        return true;
    }
    let Some(obstacle_id) = engine.rooms.room(obstacle_room).obstacle_item() else {
        return true;
    };
    let trace_layer = engine.rooms.room(obstacle_room).layer;
    let Some((trace_polyline, obstacle_half_width, obstacle_cl_class)) =
        trace_geometry(engine, obstacle_id)
    else {
        return true;
    };

    // Only traces with the same half width and the same clearance class can be
    // shoved (`:42-47`).
    let ctrl_half_width = engine
        .ctrl
        .trace_half_width
        .get(trace_layer.max(0) as usize)
        .copied()
        .unwrap_or_default();
    if obstacle_half_width != ctrl_half_width
        || obstacle_cl_class != engine.ctrl.trace_clearance_class_no
    {
        return true;
    }
    let compensated_trace_half_width = engine.ctrl.compensated_half_width(trace_layer);
    let from_door_shape = engine.rooms.door(from_door_id).shape.clone();
    if geom::max_width(&from_door_shape) < 2.0 * compensated_trace_half_width {
        return true;
    }
    let Some((_, index_in_item)) = engine.rooms.room(obstacle_room).item else {
        return true;
    };
    let trace_corner_no = index_in_item as usize;

    // Conformance delta B1 (`:57-63`): the index must allow access up to
    // `trace_corner_no + 2`. Stale indices occur when traces are modified
    // during routing.
    if trace_polyline.arr.len() < 3 || trace_corner_no + 2 >= trace_polyline.arr.len() {
        return false;
    }

    let from_door_dimension = engine.rooms.door(from_door_id).dimension;
    let shove_line_segment = if from_door_dimension == 2 {
        // Shove from a link door toward the other link door (`:66-90`).
        let Some(other_room) = engine.rooms.door(from_door_id).other_room(obstacle_room) else {
            return false;
        };
        if engine.rooms.room(other_room).kind != RoomKind::Obstacle {
            return false;
        }
        let Some(other_item) = engine.rooms.room(other_room).obstacle_item() else {
            return false;
        };
        if !end_points_matching(engine, obstacle_id, other_item) {
            return false;
        }
        let door_center = from_door_shape.centre_of_gravity();
        let corner_1 = trace_polyline.corner_approx(trace_corner_no);
        let corner_2 = trace_polyline.corner_approx(trace_corner_no + 1);
        if corner_1.distance_square(&corner_2) < 1.0 {
            // The shove segment would be reduced to a point.
            return false;
        }
        let Some(segment) = LineSegment::from_polyline(&trace_polyline, trace_corner_no + 1) else {
            return false;
        };
        if door_center.distance_square(&corner_2) < door_center.distance_square(&corner_1) {
            // Shove from the end point to the start point of the segment.
            segment.opposite()
        } else {
            segment
        }
    } else {
        // A 1-dim door (`:90-171`).
        let Some(from_room) = engine.rooms.door(from_door_id).other_room(obstacle_room) else {
            return false;
        };
        let from_point = engine.rooms.room(from_room).shape.centre_of_gravity();
        let shove_trace_line = trace_polyline.arr[trace_corner_no + 1];
        let Some(door_line_segment) = geom::diagonal_corner_segment(&from_door_shape) else {
            return false;
        };
        let side_of_trace_line = shove_trace_line.side_of_float_tol(&door_line_segment.a, 0.0);
        let Some(polar_line_segment) = polar_line_segment(&from_door_shape, &from_point) else {
            return false;
        };
        let door_line_swapped = polar_line_segment.b.distance_square(&door_line_segment.a)
            < polar_line_segment.a.distance_square(&door_line_segment.a);

        let shape_entry_check_distance = compensated_trace_half_width + 5.0;
        let check_dist_square = shape_entry_check_distance * shape_entry_check_distance;
        let section_no = element.payload.section_no_of_door;
        let section_count = engine.rooms.door(from_door_id).section_count();
        let entry = element.payload.shape_entry;

        let section_ok = if shove_to_the_left != door_line_swapped {
            section_count > 0
                && section_no == section_count - 1
                && (entry.a.distance_square(&door_line_segment.b) <= check_dist_square
                    || entry.b.distance_square(&door_line_segment.b) <= check_dist_square)
        } else {
            section_no == 0
                && (entry.a.distance_square(&door_line_segment.a) <= check_dist_square
                    || entry.b.distance_square(&door_line_segment.a) <= check_dist_square)
        };
        if !section_ok {
            return false;
        }

        let shrinked = geom::shrink_segment(&polar_line_segment, compensated_trace_half_width);
        let perpendicular_direction = shove_trace_line.direction().turn_45_degree(2);
        let forward = matches!(
            (side_of_trace_line, shove_to_the_left),
            (Side::OnTheLeft, true) | (Side::OnTheRight, false)
        );
        let (closing_point, line_1, line_2) = if forward {
            (
                shrinked.b.round(),
                trace_polyline.arr[trace_corner_no + 1],
                trace_polyline.arr[trace_corner_no + 2],
            )
        } else {
            (
                shrinked.a.round(),
                trace_polyline.arr[trace_corner_no + 1].opposite(),
                trace_polyline.arr[trace_corner_no].opposite(),
            )
        };
        let start_closing_line =
            Line::new_from_point_and_dir(closing_point, &perpendicular_direction);
        LineSegment::new(start_closing_line, line_1, line_2)
    };

    let trace_half_width = ctrl_half_width;
    let net_no_arr = NetNos::of(engine.ctrl.net_no);
    let cl_class = engine.ctrl.trace_clearance_class_no;
    let bounds = engine.bounds();

    let mut shove_width = check_trace_segment(
        engine.board,
        &shove_line_segment,
        trace_layer,
        &net_no_arr,
        trace_half_width,
        cl_class,
        true,
    );
    let mut shove_line_segment = shove_line_segment;
    let mut segment_shortened = false;
    if shove_width < fr_board::shove::FULL_SHOVE {
        shove_width -= 1.0;
        if shove_width <= 0.0 {
            return true;
        }
        shove_line_segment = shove_line_segment.change_length_approx(shove_width);
        segment_shortened = true;
    }

    let from_corner = shove_line_segment.start_point_approx();
    let to_corner = shove_line_segment.end_point_approx();
    let segment_is_point = from_corner.distance_square(&to_corner) < 0.1;

    if !segment_is_point {
        fr_metrics::counters::bump::shove_checks(1);
        shove_width = check_shove_segment(
            engine.board,
            &bounds,
            &shove_line_segment,
            shove_to_the_left,
            trace_layer,
            &net_no_arr,
            trace_half_width,
            cl_class,
            engine.ctrl.max_shove_trace_recursion_depth,
            engine.ctrl.max_shove_via_recursion_depth,
        );
        if shove_width <= 0.0 {
            return true;
        }
    }
    if segment_shortened {
        shove_width = shove_width.min(from_corner.distance(&to_corner));
    }

    let shove_line = shove_line_segment.get_line();

    // Used to check that a door lies between the from-door and the end point of
    // the shove line (`:186-193`).
    let from_door_compare_distance = if from_door_dimension == 2 || segment_is_point {
        f64::MAX
    } else {
        to_corner.distance_square(&from_door_shape.corner_approx(0))
    };

    let room_doors = engine.rooms.room(obstacle_room).doors.clone();
    for curr_door in room_doors {
        if curr_door == from_door_id {
            continue;
        }
        let door = engine.rooms.door(curr_door);
        if !door.alive {
            continue;
        }
        let (first_room, second_room, dimension) =
            (door.first_room, door.second_room, door.dimension);
        if engine.rooms.room(first_room).kind == RoomKind::Obstacle
            && engine.rooms.room(second_room).kind == RoomKind::Obstacle
        {
            let first_item = engine.rooms.room(first_room).obstacle_item();
            let second_item = engine.rooms.room(second_room).obstacle_item();
            if first_item != second_item {
                // Topological problems at a trace fork (`:204-213`).
                continue;
            }
        }
        let curr_door_shape = engine.rooms.door(curr_door).shape.clone();
        if dimension == 2 && shove_width >= fr_board::shove::FULL_SHOVE {
            if curr_door_shape.contains_float(&to_corner, 0.0) {
                if let Some(section) =
                    section_segment(engine, curr_door, compensated_trace_half_width, 0)
                {
                    to_door_list.push(DoorSection {
                        door: curr_door,
                        section_no: 0,
                        section_line: section,
                    });
                }
            }
        } else if !segment_is_point {
            // Now `curr_door` is one-dimensional.
            let Some(curr_door_segment) = geom::diagonal_corner_segment(&curr_door_shape) else {
                continue;
            };
            let start_side = shove_line.side_of_float_tol(&curr_door_segment.a, 0.0);
            let end_side = shove_line.side_of_float_tol(&curr_door_segment.b, 0.0);
            let wanted = if shove_to_the_left {
                Side::OnTheLeft
            } else {
                Side::OnTheRight
            };
            if start_side != wanted || end_side != wanted {
                continue;
            }
            let Some(curr_door_line) = polar_line_segment(&curr_door_shape, &from_corner) else {
                continue;
            };
            let curr_door_nearest_corner = if curr_door_line.a.distance_square(&from_corner)
                <= curr_door_line.b.distance_square(&from_corner)
            {
                curr_door_line.a
            } else {
                curr_door_line.b
            };
            if to_corner.distance_square(&curr_door_nearest_corner) >= from_door_compare_distance {
                // Not located toward `to_corner`.
                continue;
            }
            let curr_door_projection = curr_door_nearest_corner.projection_approx(&shove_line);
            if curr_door_projection.distance(&from_corner) + compensated_trace_half_width
                > shove_width
            {
                continue;
            }
            let sections = all_section_segments(engine, curr_door, compensated_trace_half_width);
            for (i, curr_line_section) in sections.into_iter().enumerate() {
                let nearest = if curr_line_section.a.distance_square(&from_corner)
                    <= curr_line_section.b.distance_square(&from_corner)
                {
                    curr_line_section.a
                } else {
                    curr_line_section.b
                };
                let projection = nearest.projection_approx(&shove_line);
                if projection.distance(&from_corner) <= shove_width {
                    to_door_list.push(DoorSection {
                        door: curr_door,
                        section_no: i as u16,
                        section_line: curr_line_section,
                    });
                }
            }
        }
    }
    true
}

/// `MazeShoveTraceAlgo.end_points_matching(trace, from_item)` (`:288-311`):
/// do the two items really continue each other, or is this a T-junction the
/// shove must not cross?
fn end_points_matching(
    engine: &AutorouteEngine<'_>,
    trace_id: fr_board::item::ItemId,
    from_item: fr_board::item::ItemId,
) -> bool {
    use fr_board::item::ItemKind;
    if from_item == trace_id {
        return true;
    }
    let (Some(trace_item), Some(other)) = (
        engine.board.store().get(trace_id),
        engine.board.store().get(from_item),
    ) else {
        return false;
    };
    if !trace_item.shares_net(other) {
        return false;
    }
    let ItemKind::Trace(trace) = &trace_item.kind else {
        return false;
    };
    match &other.kind {
        ItemKind::Via(via) => {
            let center = Point::Int(via.center);
            center == trace.first_corner() || center == trace.last_corner()
        }
        ItemKind::Pin(pin) => {
            let center = Point::Int(pin.center);
            center == trace.first_corner() || center == trace.last_corner()
        }
        ItemKind::Trace(from_trace) => {
            trace.first_corner() == from_trace.first_corner()
                || trace.first_corner() == from_trace.last_corner()
                || trace.last_corner() == from_trace.first_corner()
                || trace.last_corner() == from_trace.last_corner()
        }
        _ => false,
    }
}

/// `PolylineShape.polar_line_segment(FloatPoint)` (`PolylineShape.java:194-211`):
/// `a` is the leftmost corner seen from `from_point`, `b` the rightmost.
fn polar_line_segment(shape: &Simplex, from_point: &FloatPoint) -> Option<FloatLine> {
    if shape.is_empty() {
        return None;
    }
    let mut left_most = shape.corner_approx(0);
    let mut right_most = left_most;
    for i in 1..shape.border_line_count() {
        let curr = shape.corner_approx(i);
        if curr.side_of(from_point, &right_most) == Side::OnTheRight {
            right_most = curr;
        }
        if curr.side_of(from_point, &left_most) == Side::OnTheLeft {
            left_most = curr;
        }
    }
    Some(FloatLine::new(left_most, right_most))
}

/// `ExpansionDoor.get_section_segments(offset)`, one section.
fn section_segment(
    engine: &mut AutorouteEngine<'_>,
    door: DoorId,
    offset: f64,
    index: usize,
) -> Option<FloatLine> {
    all_section_segments(engine, door, offset)
        .get(index)
        .copied()
}

/// `ExpansionDoor.get_section_segments(offset)`.
fn all_section_segments(
    engine: &mut AutorouteEngine<'_>,
    door: DoorId,
    offset: f64,
) -> Vec<FloatLine> {
    let AutorouteEngine { rooms, nodes, .. } = engine;
    let mut out = Vec::new();
    let count = rooms.section_segments(door, offset, nodes, &mut out);
    out.truncate(count as usize);
    out
}

/// The obstacle trace's polyline, half width and clearance class.
fn trace_geometry(
    engine: &AutorouteEngine<'_>,
    id: fr_board::item::ItemId,
) -> Option<(fr_geometry::planar::Polyline, i32, i32)> {
    let item = engine.board.store().get(id)?;
    let fr_board::item::ItemKind::Trace(trace) = &item.kind else {
        return None;
    };
    Some((
        trace.polyline.clone(),
        trace.half_width,
        item.clearance_class,
    ))
}

/// `IntBox` and `ConvexShape` come in through the door-width comparison; keep
/// the imports honest.
const _: fn(&IntBox) -> f64 = |b| ConvexShape::max_width(b);
