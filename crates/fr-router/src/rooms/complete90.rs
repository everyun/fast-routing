//! The 90° room completion — `ShapeSearchTree90Degree.complete_shape` and
//! `restrain_shape` (`board/ShapeSearchTree90Degree.java:33-180`).

use fr_board::shape::TileShape;
use fr_geometry::planar::IntBox;

use crate::engine::{AutorouteEngine, Obstacle};
use crate::rooms::complete::CompletedShape;
use crate::rooms::RoomId;

/// A room with `IntBox` shape and `IntBox` contained shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Room90 {
    /// The room shape.
    pub shape: IntBox,
    /// The contained shape.
    pub contained: IntBox,
}

impl AutorouteEngine<'_> {
    /// True when the board is 90°-restricted.
    #[inline]
    pub(crate) fn is_ninety(&self) -> bool {
        self.board.angle_restriction() == fr_board::AngleRestriction::Ninety
    }

    /// `ShapeSearchTree90Degree.complete_shape` (`:33-110`).
    pub(crate) fn complete_shape_90_box(
        &mut self,
        shape: Option<IntBox>,
        contained_box: IntBox,
        layer: i32,
        ignore_room: Option<RoomId>,
        ignore_box: Option<IntBox>,
    ) -> Vec<Room90> {
        if contained_box.is_empty() {
            return Vec::new();
        }

        let mut start = self.bounds();
        if let Some(s) = shape {
            start = s.intersection(&start);
        }
        if start.dimension() != 2 {
            return Vec::new();
        }

        let mut result = vec![Room90 {
            shape: start,
            contained: contained_box,
        }];

        let mut obstacles = Vec::new();
        let mut own_net = Vec::new();
        self.overlapping_obstacles_tile_for_completion(
            &TileShape::Box(start),
            Some(layer),
            self.ctrl.net_no,
            &mut obstacles,
            &mut own_net,
        );

        for obstacle in obstacles {
            if Some(obstacle) == ignore_room.map(Obstacle::Room) {
                continue;
            }
            if self.obstacle_layer(obstacle) != Some(layer) {
                continue;
            }
            let Some(obstacle_box) = self.obstacle_box(obstacle) else {
                continue;
            };

            if !result
                .iter()
                .any(|cand| cand.shape.intersects(&obstacle_box))
            {
                continue;
            }

            let mut new_result: Vec<Room90> = Vec::new();
            for candidate in &result {
                if !candidate.shape.overlaps(&obstacle_box) {
                    new_result.push(*candidate);
                    continue;
                }

                if let Obstacle::Room(_) = obstacle {
                    if let Some(ignore_b) = ignore_box {
                        let is = candidate.shape.intersection(&obstacle_box);
                        if is.is_contained_in(&ignore_b) {
                            continue;
                        }
                    }
                }

                let restrained = restrain_shape_90(candidate, &obstacle_box);
                new_result.extend(restrained);
            }
            result = new_result;
        }

        result
    }

    /// Complete shape returning CompletedShape wrappers with simplex conversion.
    pub(crate) fn complete_shape_90(
        &mut self,
        shape: Option<&fr_geometry::planar::Simplex>,
        contained: &fr_geometry::planar::Simplex,
        layer: i32,
        ignore_room: Option<RoomId>,
        ignore_shape: Option<&fr_geometry::planar::Simplex>,
    ) -> Vec<CompletedShape> {
        let shape_box = shape.map(|s| s.bounding_box());
        let contained_box = contained.bounding_box();
        let ignore_box = ignore_shape.map(|s| s.bounding_box());

        let rooms =
            self.complete_shape_90_box(shape_box, contained_box, layer, ignore_room, ignore_box);

        rooms
            .into_iter()
            .map(|r| CompletedShape {
                shape: r.shape.to_simplex(),
                contained: r.contained.to_simplex(),
                oct: Some(r.shape.to_int_octagon()),
                bbox: std::cell::Cell::new(None),
                sat: std::cell::OnceCell::new(),
            })
            .collect()
    }
}

/// Restrains `room` to a box shape not intersecting `obstacle`'s interior.
/// Ported from `ShapeSearchTree90Degree.restrain_shape` (`:117-180`).
fn restrain_shape_90(room: &Room90, obstacle: &IntBox) -> Vec<Room90> {
    let mut result = Vec::new();
    let contained_shape = room.contained;
    if contained_shape.is_empty() {
        return result;
    }
    let room_shape = room.shape;
    let mut cut_line_distance = 0;
    let mut restrained_shape = None;

    // 1. Right line segment of obstacle
    if room_shape.ll.x < obstacle.ur.x
        && room_shape.ur.x > obstacle.ur.x
        && room_shape.ur.y > obstacle.ll.y
        && room_shape.ll.y < obstacle.ur.y
    {
        let curr_distance = contained_shape.ll.x - obstacle.ur.x;
        if curr_distance > cut_line_distance {
            cut_line_distance = curr_distance;
            restrained_shape = Some(IntBox::new(
                obstacle.ur.x,
                room_shape.ll.y,
                room_shape.ur.x,
                room_shape.ur.y,
            ));
        }
    }
    // 2. Left line segment of obstacle
    if room_shape.ll.x < obstacle.ll.x
        && room_shape.ur.x > obstacle.ll.x
        && room_shape.ur.y > obstacle.ll.y
        && room_shape.ll.y < obstacle.ur.y
    {
        let curr_distance = obstacle.ll.x - contained_shape.ur.x;
        if curr_distance > cut_line_distance {
            cut_line_distance = curr_distance;
            restrained_shape = Some(IntBox::new(
                room_shape.ll.x,
                room_shape.ll.y,
                obstacle.ll.x,
                room_shape.ur.y,
            ));
        }
    }
    // 3. Lower line segment of obstacle
    if room_shape.ll.y < obstacle.ll.y
        && room_shape.ur.y > obstacle.ll.y
        && room_shape.ur.x > obstacle.ll.x
        && room_shape.ll.x < obstacle.ur.x
    {
        let curr_distance = obstacle.ll.y - contained_shape.ur.y;
        if curr_distance > cut_line_distance {
            cut_line_distance = curr_distance;
            restrained_shape = Some(IntBox::new(
                room_shape.ll.x,
                room_shape.ll.y,
                room_shape.ur.x,
                obstacle.ll.y,
            ));
        }
    }
    // 4. Upper line segment of obstacle
    if room_shape.ll.y < obstacle.ur.y
        && room_shape.ur.y > obstacle.ur.y
        && room_shape.ur.x > obstacle.ll.x
        && room_shape.ll.x < obstacle.ur.x
    {
        let curr_distance = contained_shape.ll.y - obstacle.ur.y;
        if curr_distance > cut_line_distance {
            restrained_shape = Some(IntBox::new(
                room_shape.ll.x,
                obstacle.ur.y,
                room_shape.ur.x,
                room_shape.ur.y,
            ));
        }
    }

    if let Some(restr) = restrained_shape {
        result.push(Room90 {
            shape: restr,
            contained: contained_shape,
        });
        return result;
    }

    // Contained shape intersects obstacle; divide in two
    let is = contained_shape.intersection(obstacle);
    if is.is_empty() {
        return result;
    }

    let mut new_shape_1 = None;
    let mut new_shape_2 = None;

    if is.ll.x > room_shape.ll.x && is.ll.x == obstacle.ll.x && is.ll.x < room_shape.ur.x {
        new_shape_1 = Some(IntBox::new(
            room_shape.ll.x,
            room_shape.ll.y,
            is.ll.x,
            room_shape.ur.y,
        ));
        new_shape_2 = Some(IntBox::new(
            is.ll.x,
            room_shape.ll.y,
            room_shape.ur.x,
            room_shape.ur.y,
        ));
    } else if is.ur.x > room_shape.ll.x && is.ur.x == obstacle.ur.x && is.ur.x < room_shape.ur.x {
        new_shape_2 = Some(IntBox::new(
            room_shape.ll.x,
            room_shape.ll.y,
            is.ur.x,
            room_shape.ur.y,
        ));
        new_shape_1 = Some(IntBox::new(
            is.ur.x,
            room_shape.ll.y,
            room_shape.ur.x,
            room_shape.ur.y,
        ));
    } else if is.ll.y > room_shape.ll.y && is.ll.y == obstacle.ll.y && is.ll.y < room_shape.ur.y {
        new_shape_1 = Some(IntBox::new(
            room_shape.ll.x,
            room_shape.ll.y,
            room_shape.ur.x,
            is.ll.y,
        ));
        new_shape_2 = Some(IntBox::new(
            room_shape.ll.x,
            is.ll.y,
            room_shape.ur.x,
            room_shape.ur.y,
        ));
    } else if is.ur.y > room_shape.ll.y && is.ur.y == obstacle.ur.y && is.ur.y < room_shape.ur.y {
        new_shape_2 = Some(IntBox::new(
            room_shape.ll.x,
            room_shape.ll.y,
            room_shape.ur.x,
            is.ur.y,
        ));
        new_shape_1 = Some(IntBox::new(
            room_shape.ll.x,
            is.ur.y,
            room_shape.ur.x,
            room_shape.ur.y,
        ));
    }

    if let (Some(ns1), Some(ns2)) = (new_shape_1, new_shape_2) {
        let new_shape_to_be_contained = contained_shape.intersection(&ns1);
        if new_shape_to_be_contained.dimension() > 0 {
            result.push(Room90 {
                shape: ns1,
                contained: new_shape_to_be_contained,
            });
            let next_contained = contained_shape.intersection(&ns2);
            let next_room = Room90 {
                shape: ns2,
                contained: next_contained,
            };
            result.extend(restrain_shape_90(&next_room, obstacle));
        }
    }

    result
}
