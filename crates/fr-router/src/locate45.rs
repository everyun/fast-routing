//! `LocateFoundConnectionAlgo45Degree` — corner placement for the 90°/45°
//! angle modes (`pull-tight-insert.md` §3.1).
//!
//! Transcribed from
//! `/tmp/fr-java-release/src/main/java/app/freerouting/autoroute/LocateFoundConnectionAlgo45Degree.java`
//! (282 L): `calculate_next_trace_corners` `:88-228`,
//! `calc_horizontal_first_from_door` `:35-85`, `calc_horizontal_first_to_door`
//! `:233-282`, the acute-corner guard `:196-209`.
//!
//! One call emits **at most two** corners — an angle knee and the point it
//! leads to — and advances `current_to_door_index` by one, except on the
//! "enter the shrunken room" step, which emits without advancing. The
//! `Locator` loop ([`crate::locate`]) calls it until it emits nothing.

use fr_geometry::planar::{FloatLine, FloatPoint, IntBox, Simplex};

use crate::ctrl::TRACE_WIDTH_TOLERANCE;
use crate::locate::{calculate_additional_corner, Hop, LocateFailure, Locator};
use crate::result::BacktrackKind;

/// `LocateFoundConnectionAlgo45Degree.calculate_next_trace_corners` (`:88-228`).
pub(crate) fn calculate_next_trace_corners_45(
    locator: &mut Locator<'_>,
    out: &mut Vec<FloatPoint>,
) -> Result<(), LocateFailure> {
    out.clear();

    // 1. `:91-102` — the trace is finished.
    if locator.current_to_door_index > locator.current_target_door_index {
        return Ok(());
    }

    // 2. `:104-109` — the room we are standing in is the one *before* the door
    // we are heading for.
    let from_index = locator.current_to_door_index - 1;
    let Some(from_hop) = locator.hops.get(from_index) else {
        return Err(LocateFailure::MissingRoom);
    };
    let Some(room_shape) = from_hop.room_shape.clone() else {
        // Upstream warns and returns an empty corner list, which ends the
        // trace mid-air; refusing the connection is the honest answer.
        return Err(LocateFailure::MissingRoom);
    };

    // 3. `:113-123` — shrink the room. An obstacle room (a planned ripup) gets
    // the bare compensated half width; a free-space room gets the tolerance on
    // top, "for free space expansion rooms".
    let trace_halfwidth = locator
        .ctrl
        .compensated_half_width(locator.current_trace_layer);
    let trace_halfwidth_add = trace_halfwidth + TRACE_WIDTH_TOLERANCE as f64;
    let shrink_offset = if from_hop.room_is_obstacle {
        trace_halfwidth
    } else {
        trace_halfwidth_add
    };
    let mut shrunk_room_shape = room_shape.offset(-shrink_offset);

    // 4. `:137-147` — enter the shrunken shape by an angle knee first.
    if !shrunk_room_shape.is_empty() {
        let nearest = shrunk_room_shape.nearest_point_approx(&locator.current_from_point);
        let horizontal_first =
            calc_horizontal_first_from_door(from_hop, locator.current_from_point, nearest);
        let nearest = nearest.round().to_float();
        out.push(calculate_additional_corner(
            locator.current_from_point,
            nearest,
            horizontal_first,
            locator.angle_restriction,
        ));
        out.push(nearest);
        locator.current_from_point = nearest;
    } else {
        shrunk_room_shape = room_shape;
    }

    // 5. `:149-171` — this is the trace's last step: aim at the target shape.
    if locator.current_to_door_index == locator.current_target_door_index {
        let nearest = locator
            .current_target_shape
            .nearest_point_approx(&locator.current_from_point)
            .round()
            .to_float();
        let mut add_corner = calculate_additional_corner(
            locator.current_from_point,
            nearest,
            true,
            locator.angle_restriction,
        );
        if !shrunk_room_shape.contains_float(&add_corner, 0.0) {
            add_corner = calculate_additional_corner(
                locator.current_from_point,
                nearest,
                false,
                locator.angle_restriction,
            );
        }
        out.push(add_corner);
        out.push(nearest);
        locator.current_to_door_index += 1;
        return Ok(());
    }

    // 6. `:173-215` — the next object must be an `ExpansionDoor`.
    let to_index = locator.current_to_door_index;
    let Some(to_hop) = locator.hops.get(to_index) else {
        return Err(LocateFailure::MissingRoom);
    };
    if !matches!(to_hop.kind, BacktrackKind::RoomDoor) {
        // `:174-177`: warn, empty result. The trace stops here, which the
        // caller turns into a refusal rather than a dangling stub.
        return Err(LocateFailure::UnexpectedDestinationDoor);
    }

    let nearest_to_door_point = if to_hop.dimension == 2 {
        // `:180-186`. "May not happen in free angle routing mode because then
        // corners are cut off."
        to_hop
            .shape
            .shrink(shrink_offset)
            .nearest_point_approx(&locator.current_from_point)
            .round()
            .to_float()
    } else {
        // `:188-209` — the door's section segment for this hop's section.
        locator.section_segments(to_index, trace_halfwidth);
        let Some(section) = locator
            .scratch_sections
            .get(to_hop.section_no as usize)
            .copied()
        else {
            // `:189-192`: "line_sections inconsistent" — warn, empty result.
            return Err(LocateFailure::MissingRoom);
        };
        let mut point = section.nearest_segment_point(&locator.current_from_point);
        if !nearest_to_door_point_ok(to_hop, point, trace_halfwidth_add) {
            // The room may have an acute (45°) angle at a corner of the door:
            // fall back to the section's midpoint (`:206-209`).
            point = section.a.middle_point(&section.b);
        }
        point.round().to_float()
    };

    let horizontal_first =
        calc_horizontal_first_to_door(to_hop, locator.current_from_point, nearest_to_door_point);
    out.push(calculate_additional_corner(
        locator.current_from_point,
        nearest_to_door_point,
        horizontal_first,
        locator.angle_restriction,
    ));
    out.push(nearest_to_door_point);
    locator.current_to_door_index += 1;
    Ok(())
}

/// The acute-corner guard (`:196-209`, conformance table row "45° locator
/// acute-corner guard").
///
/// The next room's shape is converted to a `Simplex` first because an `IntBox`
/// or `IntOctagon` carries border lines of length 0, for which the two-nearest
/// border-point query does not work (`:198-200`). Our room shapes are simplices
/// throughout ([`crate::geom`] module docs), so the conversion is already done.
fn nearest_to_door_point_ok(to_hop: &Hop, point: FloatPoint, trace_halfwidth_add: f64) -> bool {
    let Some(next_room_shape) = to_hop.room_shape.as_ref() else {
        return true;
    };
    let nearest_points = next_room_shape.nearest_border_points_approx(&point, 2);
    if nearest_points.len() < 2 {
        return true;
    }
    nearest_points[1].distance(&point) >= trace_halfwidth_add
}

/// `calc_horizontal_first_from_door` (`:35-85`).
fn calc_horizontal_first_from_door(
    from_hop: &Hop,
    from_point: FloatPoint,
    to_point: FloatPoint,
) -> bool {
    let door_box = from_hop.shape.bounding_box();
    if from_hop.dimension != 1 {
        return door_box.height() >= door_box.width();
    }
    match door_orientation(&from_hop.shape, &door_box) {
        DoorOrientation::Vertical => true,
        DoorOrientation::Horizontal => false,
        DoorOrientation::RightDiagonal => {
            let dx = to_point.x - from_point.x;
            let dy = to_point.y - from_point.y;
            if signum(dx) == signum(dy) {
                dx.abs() > dy.abs()
            } else {
                dx.abs() < dy.abs()
            }
        }
        DoorOrientation::LeftDiagonal => {
            let dx = to_point.x - from_point.x;
            let dy = to_point.y - from_point.y;
            if signum(dx) == signum(dy) {
                dx.abs() < dy.abs()
            } else {
                dx.abs() > dy.abs()
            }
        }
    }
}

/// `calc_horizontal_first_to_door` (`:233-282`) — the sign-mirrored twin of
/// [`calc_horizontal_first_from_door`].
fn calc_horizontal_first_to_door(
    to_hop: &Hop,
    from_point: FloatPoint,
    to_point: FloatPoint,
) -> bool {
    let door_box = to_hop.shape.bounding_box();
    if to_hop.dimension != 1 {
        return door_box.height() <= door_box.width();
    }
    match door_orientation(&to_hop.shape, &door_box) {
        DoorOrientation::Vertical => false,
        DoorOrientation::Horizontal => true,
        DoorOrientation::RightDiagonal => {
            let dx = to_point.x - from_point.x;
            let dy = to_point.y - from_point.y;
            if signum(dx) == signum(dy) {
                dx.abs() < dy.abs()
            } else {
                dx.abs() > dy.abs()
            }
        }
        DoorOrientation::LeftDiagonal => {
            let dx = to_point.x - from_point.x;
            let dy = to_point.y - from_point.y;
            if signum(dx) == signum(dy) {
                dx.abs() > dy.abs()
            } else {
                dx.abs() < dy.abs()
            }
        }
    }
}

/// How the two `calc_horizontal_first_*` functions classify a 1-dimensional
/// door (`:52-83` and `:249-280`, identical in both).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DoorOrientation {
    Vertical,
    Horizontal,
    RightDiagonal,
    LeftDiagonal,
}

fn door_orientation(door_shape: &Simplex, door_box: &IntBox) -> DoorOrientation {
    let Some(segment) = crate::geom::diagonal_corner_segment(door_shape) else {
        // A door with no corners is degenerate; treat it as vertical, which is
        // what a zero-size bounding box gives upstream via `height() >=
        // width()`.
        return DoorOrientation::Vertical;
    };
    let (left_corner, right_corner) = sort_door_corners(&segment);
    let door_dx = right_corner.x - left_corner.x;
    let door_dy = right_corner.y - left_corner.y;
    let door_max_width = door_dx.max(door_dy.abs());
    let door_half_max_width = 0.5 * door_max_width;
    if (door_box.width() as f64) <= door_half_max_width {
        DoorOrientation::Vertical
    } else if (door_box.height() as f64) <= door_half_max_width {
        DoorOrientation::Horizontal
    } else if left_corner.y < right_corner.y {
        DoorOrientation::RightDiagonal
    } else {
        DoorOrientation::LeftDiagonal
    }
}

/// `:45-51` / `:242-248`: order the door segment's endpoints left-to-right,
/// bottom-to-top on a tie.
fn sort_door_corners(segment: &FloatLine) -> (FloatPoint, FloatPoint) {
    if segment.a.x < segment.b.x || (segment.a.x == segment.b.x && segment.a.y <= segment.b.y) {
        (segment.a, segment.b)
    } else {
        (segment.b, segment.a)
    }
}

/// `datastructures.Signum.of(double)`: `-1`, `0` or `1`.
#[inline]
fn signum(value: f64) -> i32 {
    if value > 0.0 {
        1
    } else if value < 0.0 {
        -1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fr_geometry::planar::IntPoint;

    fn hop_with_door(shape: Simplex, dimension: i32) -> Hop {
        Hop {
            kind: BacktrackKind::RoomDoor,
            section_no: 0,
            door_line: crate::geom::diagonal_corner_segment(&shape),
            shape,
            dimension,
            room_shape: None,
            room_layer: Some(0),
            room_is_obstacle: false,
            connection_shape: None,
        }
    }

    fn vertical_door() -> Hop {
        // A tall, thin door: width 0, height 1000.
        hop_with_door(IntBox::new(100, 0, 100, 1000).to_simplex(), 1)
    }

    fn horizontal_door() -> Hop {
        hop_with_door(IntBox::new(0, 100, 1000, 100).to_simplex(), 1)
    }

    #[test]
    fn a_vertical_door_goes_horizontal_first_when_leaving_it() {
        // `:58-60` and its mirror `:255-257`.
        let hop = vertical_door();
        let from = FloatPoint::new(0.0, 0.0);
        let to = FloatPoint::new(500.0, 500.0);
        assert!(calc_horizontal_first_from_door(&hop, from, to));
        assert!(!calc_horizontal_first_to_door(&hop, from, to));
    }

    #[test]
    fn a_horizontal_door_goes_vertical_first_when_leaving_it() {
        let hop = horizontal_door();
        let from = FloatPoint::new(0.0, 0.0);
        let to = FloatPoint::new(500.0, 500.0);
        assert!(!calc_horizontal_first_from_door(&hop, from, to));
        assert!(calc_horizontal_first_to_door(&hop, from, to));
    }

    #[test]
    fn an_area_door_falls_back_to_its_bounding_box_aspect() {
        // `:38-40`: dimension != 1 short-circuits on the box aspect ratio, and
        // the to-door variant uses the opposite comparison.
        let wide = hop_with_door(IntBox::new(0, 0, 1000, 100).to_simplex(), 2);
        let from = FloatPoint::new(0.0, 0.0);
        let to = FloatPoint::new(1.0, 1.0);
        assert!(!calc_horizontal_first_from_door(&wide, from, to));
        assert!(calc_horizontal_first_to_door(&wide, from, to));
    }

    #[test]
    fn the_acute_corner_guard_accepts_a_point_in_a_wide_room() {
        let hop = Hop {
            room_shape: Some(IntBox::new(0, 0, 1000, 1000).to_simplex()),
            ..hop_with_door(IntBox::new(0, 0, 0, 100).to_simplex(), 1)
        };
        // The centre is far from every border.
        assert!(nearest_to_door_point_ok(
            &hop,
            FloatPoint::new(500.0, 500.0),
            52.0
        ));
        // A point in the corner has two border lines within the half width.
        assert!(!nearest_to_door_point_ok(
            &hop,
            FloatPoint::new(1.0, 1.0),
            52.0
        ));
    }

    #[test]
    fn signum_matches_the_java_helper() {
        assert_eq!(signum(-0.5), -1);
        assert_eq!(signum(0.0), 0);
        assert_eq!(signum(7.0), 1);
        let _ = IntPoint::new(0, 0);
    }
}
