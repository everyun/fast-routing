//! The 90° door and room calculation — `SortedOrthogonalRoomNeighbours`
//! (`autoroute/SortedOrthogonalRoomNeighbours.java:16-360`).

use fr_board::item::ItemId;
use fr_board::shape::TileShape;
use fr_geometry::planar::{IntBox, Limits};

use crate::engine::{AutorouteEngine, Obstacle};
use crate::rooms::{DoorKind, RoomId, RoomKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SortedNeighbour90 {
    obstacle_id: i64,
    intersection: IntBox,
    first_touching_side: i32,
    last_touching_side: i32,
}

impl SortedNeighbour90 {
    fn new(
        obstacle_id: i64,
        intersection: IntBox,
        room_shape: &IntBox,
        edge_interior_touches_obstacle: &mut [bool; 4],
    ) -> Option<Self> {
        if intersection.ll.y == room_shape.ll.y
            && intersection.ur.x > room_shape.ll.x
            && intersection.ll.x < room_shape.ur.x
        {
            edge_interior_touches_obstacle[0] = true;
        }
        if intersection.ur.x == room_shape.ur.x
            && intersection.ur.y > room_shape.ll.y
            && intersection.ll.y < room_shape.ur.y
        {
            edge_interior_touches_obstacle[1] = true;
        }
        if intersection.ur.y == room_shape.ur.y
            && intersection.ur.x > room_shape.ll.x
            && intersection.ll.x < room_shape.ur.x
        {
            edge_interior_touches_obstacle[2] = true;
        }
        if intersection.ll.x == room_shape.ll.x
            && intersection.ur.y > room_shape.ll.y
            && intersection.ll.y < room_shape.ur.y
        {
            edge_interior_touches_obstacle[3] = true;
        }

        let first_touching_side =
            if intersection.ll.y == room_shape.ll.y && intersection.ll.x > room_shape.ll.x {
                0
            } else if intersection.ur.x == room_shape.ur.x && intersection.ll.y > room_shape.ll.y {
                1
            } else if intersection.ur.y == room_shape.ur.y {
                2
            } else if intersection.ll.x == room_shape.ll.x {
                3
            } else {
                return None;
            };

        let last_touching_side =
            if intersection.ll.x == room_shape.ll.x && intersection.ll.y > room_shape.ll.y {
                3
            } else if intersection.ur.y == room_shape.ur.y && intersection.ll.x > room_shape.ll.x {
                2
            } else if intersection.ur.x == room_shape.ur.x {
                1
            } else if intersection.ll.y == room_shape.ll.y {
                0
            } else {
                return None;
            };

        Some(Self {
            obstacle_id,
            intersection,
            first_touching_side,
            last_touching_side,
        })
    }
}

impl Ord for SortedNeighbour90 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        if self.first_touching_side != other.first_touching_side {
            return self.first_touching_side.cmp(&other.first_touching_side);
        }
        let is1 = self.intersection;
        let is2 = other.intersection;
        let cmp_val = match self.first_touching_side {
            0 => is1.ll.x.cmp(&is2.ll.x),
            1 => is1.ll.y.cmp(&is2.ll.y),
            2 => is2.ur.x.cmp(&is1.ur.x),
            3 => is2.ur.y.cmp(&is1.ur.y),
            _ => std::cmp::Ordering::Equal,
        };
        if cmp_val != std::cmp::Ordering::Equal {
            return cmp_val;
        }

        let this_diff = (self.last_touching_side - self.first_touching_side + 4) % 4;
        let other_diff = (other.last_touching_side - other.first_touching_side + 4) % 4;
        if this_diff != other_diff {
            return this_diff.cmp(&other_diff);
        }

        let cmp_last = match self.last_touching_side {
            0 => is1.ur.x.cmp(&is2.ur.x),
            1 => is1.ur.y.cmp(&is2.ur.y),
            2 => is2.ll.x.cmp(&is1.ll.x),
            3 => is2.ll.y.cmp(&is1.ll.y),
            _ => std::cmp::Ordering::Equal,
        };
        if cmp_last != std::cmp::Ordering::Equal {
            return cmp_last;
        }

        self.obstacle_id.cmp(&other.obstacle_id)
    }
}

impl PartialOrd for SortedNeighbour90 {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl AutorouteEngine<'_> {
    /// `AutorouteEngine.calculate_doors` for 90° boards.
    pub fn calculate_doors_90(&mut self, room_id: RoomId) {
        loop {
            self.mint_room_id(room_id);
            let (neighbours, edge_interior_touches_obstacle, own_net) =
                self.calculate_neighbours_90(room_id);
            if self.try_remove_edge_90(room_id, &edge_interior_touches_obstacle) {
                self.remove_all_doors(room_id);
                continue;
            }
            if neighbours.is_empty() {
                if self.rooms.room(room_id).kind == RoomKind::Obstacle {
                    self.calculate_incomplete_rooms_with_empty_neighbours_90(room_id);
                }
            } else {
                self.calculate_new_incomplete_rooms_90(room_id, &neighbours);
            }
            if self.rooms.room(room_id).kind == RoomKind::CompleteFree {
                self.calculate_target_doors(room_id, &own_net);
            }
            return;
        }
    }

    fn calculate_neighbours_90(
        &mut self,
        room_id: RoomId,
    ) -> (Vec<SortedNeighbour90>, [bool; 4], Vec<(ItemId, u16)>) {
        let room_box = self.rooms.room(room_id).shape.bounding_box();
        let layer = self.rooms.room(room_id).layer;
        let is_obstacle_room = self.rooms.room(room_id).kind == RoomKind::Obstacle;

        let mut edge_interior_touches_obstacle = [false; 4];
        let mut own_net = Vec::new();
        let mut obstacles = Vec::new();

        self.overlapping_obstacles_tile(
            &TileShape::Box(room_box),
            Some(layer),
            self.ctrl.net_no,
            &mut obstacles,
            &mut own_net,
        );

        if is_obstacle_room {
            let mut promoted: Vec<Obstacle> = own_net
                .iter()
                .map(|&(item, index)| {
                    let slot = self
                        .board
                        .store()
                        .slot_of(item)
                        .unwrap_or(fr_board::store::SlotIndex::from_raw(0));
                    Obstacle::Item(item, index, slot)
                })
                .collect();
            promoted.append(&mut obstacles);
            promoted.sort_unstable();
            obstacles = promoted;
        }

        let mut sorted_neighbours = Vec::new();

        for obstacle in obstacles {
            if obstacle == Obstacle::Room(room_id) {
                continue;
            }
            if let (Obstacle::Item(item, index, _), Some((room_item, room_index))) =
                (obstacle, self.rooms.room(room_id).item)
            {
                if item == room_item && index == room_index {
                    continue;
                }
            }
            let Some(neighbour_box) = self.obstacle_box(obstacle) else {
                continue;
            };
            let intersection = room_box.intersection(&neighbour_box);
            let dimension = intersection.dimension();

            if dimension > 1 && is_obstacle_room {
                if let Obstacle::Item(item, index, _) = obstacle {
                    self.create_overlap_door(room_id, item, index);
                }
                continue;
            }

            if dimension < 0 {
                continue;
            }

            let obs_id = self.obstacle_id(obstacle);
            if let Some(sn) = SortedNeighbour90::new(
                obs_id,
                intersection,
                &room_box,
                &mut edge_interior_touches_obstacle,
            ) {
                sorted_neighbours.push(sn);
            }

            if dimension > 0 {
                let neighbour_room = match obstacle {
                    Obstacle::Room(other) => Some(other),
                    Obstacle::Item(item, index, _) => {
                        let routable = self
                            .board
                            .store()
                            .get(item)
                            .map(|item| item.is_routable())
                            .unwrap_or(false);
                        if routable {
                            self.obstacle_room(item, index)
                        } else {
                            None
                        }
                    }
                };
                if let Some(neighbour_room) = neighbour_room {
                    let door_shape = intersection.to_simplex();
                    if self.insert_door_ok(room_id, neighbour_room, &door_shape) {
                        self.rooms.add_room_door(room_id, neighbour_room, dimension);
                    }
                }
            }
        }

        sorted_neighbours.sort();
        (sorted_neighbours, edge_interior_touches_obstacle, own_net)
    }

    fn try_remove_edge_90(
        &mut self,
        room_id: RoomId,
        edge_interior_touches_obstacle: &[bool; 4],
    ) -> bool {
        if self.rooms.room(room_id).kind != RoomKind::IncompleteFree {
            return false;
        }
        let room_box = self.rooms.room(room_id).shape.bounding_box();
        let room_area = room_box.area();

        let mut remove_edge_no = -1;
        for (i, &touches) in edge_interior_touches_obstacle.iter().enumerate() {
            if !touches {
                remove_edge_no = i as i32;
                break;
            }
        }

        if remove_edge_no >= 0 {
            let enlarged_box = remove_border_line_90(&room_box, remove_edge_no as usize);
            let doors = self.rooms.room(room_id).doors.clone();
            let mut ignore_shape: Option<IntBox> = None;
            let mut ignore_room: Option<RoomId> = None;
            let mut max_door_area = 0.0;

            for door_id in doors {
                let door = self.rooms.door(door_id);
                if door.dimension == 2 {
                    if let DoorKind::Room = door.kind {
                        if let Some(other_room) = door.other_room_any(room_id) {
                            if self.rooms.room(other_room).kind == RoomKind::CompleteFree {
                                let door_box = door.shape.bounding_box();
                                let door_area = door_box.area();
                                if door_area > max_door_area {
                                    max_door_area = door_area;
                                    ignore_shape = Some(door_box);
                                    ignore_room = Some(other_room);
                                }
                            }
                        }
                    }
                }
            }

            let layer = self.rooms.room(room_id).layer;
            let contained_box = self.rooms.room(room_id).contained_shape.bounding_box();
            let new_rooms = self.complete_shape_90_box(
                Some(enlarged_box),
                contained_box,
                layer,
                ignore_room,
                ignore_shape,
            );

            if new_rooms.len() == 1 {
                let new_room = &new_rooms[0];
                if new_room.shape.area() > room_area {
                    self.rooms
                        .room_mut(room_id)
                        .set_shape(new_room.shape.to_simplex());
                    self.rooms.room_mut(room_id).contained_shape = new_room.contained.to_simplex();
                    return true;
                }
            }
        }
        false
    }

    fn calculate_incomplete_rooms_with_empty_neighbours_90(&mut self, room_id: RoomId) {
        let room_box = self.rooms.room(room_id).shape.bounding_box();
        let bounding_box = self.bounds();
        let layer = self.rooms.room(room_id).layer;

        for i in 0..4 {
            let new_room_box = match i {
                0 => IntBox::new(
                    bounding_box.ll.x,
                    bounding_box.ll.y,
                    bounding_box.ur.x,
                    room_box.ll.y,
                ),
                1 => IntBox::new(
                    room_box.ur.x,
                    bounding_box.ll.y,
                    bounding_box.ur.x,
                    bounding_box.ur.y,
                ),
                2 => IntBox::new(
                    bounding_box.ll.x,
                    room_box.ur.y,
                    bounding_box.ur.x,
                    bounding_box.ur.y,
                ),
                _ => IntBox::new(
                    bounding_box.ll.x,
                    bounding_box.ll.y,
                    room_box.ll.x,
                    bounding_box.ur.y,
                ),
            };
            let new_contained_box = room_box.intersection(&new_room_box);
            let new_room = self.rooms.add_incomplete_room(
                Some(new_room_box.to_simplex()),
                layer,
                new_contained_box.to_simplex(),
            );
            self.rooms.add_room_door(room_id, new_room, 1);
        }
    }

    fn calculate_new_incomplete_rooms_90(
        &mut self,
        room_id: RoomId,
        neighbours: &[SortedNeighbour90],
    ) {
        let board_bounds = self.bounds();
        let is_obstacle = self.rooms.room(room_id).kind == RoomKind::Obstacle;
        let room_box = self.rooms.room(room_id).shape.bounding_box();
        let layer = self.rooms.room(room_id).layer;

        let mut prev_neighbour = &neighbours[neighbours.len() - 1];

        for next_neighbour in neighbours {
            if !next_neighbour
                .intersection
                .intersects(&prev_neighbour.intersection)
            {
                match next_neighbour.first_touching_side {
                    0 => {
                        if prev_neighbour.last_touching_side == 0 {
                            if prev_neighbour.intersection.ur.x < next_neighbour.intersection.ll.x {
                                self.insert_incomplete_room_90(
                                    room_id,
                                    layer,
                                    &room_box,
                                    IntBox::new(
                                        prev_neighbour.intersection.ur.x,
                                        board_bounds.ll.y,
                                        next_neighbour.intersection.ll.x,
                                        room_box.ll.y,
                                    ),
                                );
                            }
                        } else if prev_neighbour.intersection.ll.y > room_box.ll.y
                            || next_neighbour.intersection.ll.x > room_box.ll.x
                        {
                            if is_obstacle {
                                if prev_neighbour.last_touching_side == 3 {
                                    self.insert_incomplete_room_90(
                                        room_id,
                                        layer,
                                        &room_box,
                                        IntBox::new(
                                            board_bounds.ll.x,
                                            room_box.ll.y,
                                            room_box.ll.x,
                                            prev_neighbour.intersection.ll.y,
                                        ),
                                    );
                                }
                                self.insert_incomplete_room_90(
                                    room_id,
                                    layer,
                                    &room_box,
                                    IntBox::new(
                                        room_box.ll.x,
                                        board_bounds.ll.y,
                                        next_neighbour.intersection.ll.x,
                                        room_box.ll.y,
                                    ),
                                );
                            } else {
                                self.insert_incomplete_room_90(
                                    room_id,
                                    layer,
                                    &room_box,
                                    IntBox::new(
                                        board_bounds.ll.x,
                                        board_bounds.ll.y,
                                        next_neighbour.intersection.ll.x,
                                        prev_neighbour.intersection.ll.y,
                                    ),
                                );
                            }
                        }
                    }
                    1 => {
                        if prev_neighbour.last_touching_side == 1 {
                            if prev_neighbour.intersection.ur.y < next_neighbour.intersection.ll.y {
                                self.insert_incomplete_room_90(
                                    room_id,
                                    layer,
                                    &room_box,
                                    IntBox::new(
                                        room_box.ur.x,
                                        prev_neighbour.intersection.ur.y,
                                        board_bounds.ur.x,
                                        next_neighbour.intersection.ll.y,
                                    ),
                                );
                            }
                        } else if prev_neighbour.intersection.ur.x < room_box.ur.x
                            || next_neighbour.intersection.ll.y > room_box.ll.y
                        {
                            if is_obstacle {
                                if prev_neighbour.last_touching_side == 0 {
                                    self.insert_incomplete_room_90(
                                        room_id,
                                        layer,
                                        &room_box,
                                        IntBox::new(
                                            prev_neighbour.intersection.ur.x,
                                            board_bounds.ll.y,
                                            room_box.ur.x,
                                            room_box.ll.y,
                                        ),
                                    );
                                }
                                self.insert_incomplete_room_90(
                                    room_id,
                                    layer,
                                    &room_box,
                                    IntBox::new(
                                        room_box.ur.x,
                                        room_box.ll.y,
                                        room_box.ur.x,
                                        next_neighbour.intersection.ll.y,
                                    ),
                                );
                            } else {
                                self.insert_incomplete_room_90(
                                    room_id,
                                    layer,
                                    &room_box,
                                    IntBox::new(
                                        prev_neighbour.intersection.ur.x,
                                        board_bounds.ll.y,
                                        board_bounds.ur.x,
                                        next_neighbour.intersection.ll.y,
                                    ),
                                );
                            }
                        }
                    }
                    2 => {
                        if prev_neighbour.last_touching_side == 2 {
                            if prev_neighbour.intersection.ll.x > next_neighbour.intersection.ur.x {
                                self.insert_incomplete_room_90(
                                    room_id,
                                    layer,
                                    &room_box,
                                    IntBox::new(
                                        next_neighbour.intersection.ur.x,
                                        room_box.ur.y,
                                        prev_neighbour.intersection.ll.x,
                                        board_bounds.ur.y,
                                    ),
                                );
                            }
                        } else if prev_neighbour.intersection.ur.y < room_box.ur.y
                            || next_neighbour.intersection.ur.x < room_box.ur.x
                        {
                            if is_obstacle {
                                if prev_neighbour.last_touching_side == 1 {
                                    self.insert_incomplete_room_90(
                                        room_id,
                                        layer,
                                        &room_box,
                                        IntBox::new(
                                            room_box.ur.x,
                                            prev_neighbour.intersection.ur.y,
                                            board_bounds.ur.x,
                                            room_box.ur.y,
                                        ),
                                    );
                                }
                                self.insert_incomplete_room_90(
                                    room_id,
                                    layer,
                                    &room_box,
                                    IntBox::new(
                                        next_neighbour.intersection.ur.x,
                                        room_box.ur.y,
                                        room_box.ur.x,
                                        board_bounds.ur.y,
                                    ),
                                );
                            } else {
                                self.insert_incomplete_room_90(
                                    room_id,
                                    layer,
                                    &room_box,
                                    IntBox::new(
                                        next_neighbour.intersection.ur.x,
                                        prev_neighbour.intersection.ur.y,
                                        board_bounds.ur.x,
                                        board_bounds.ur.y,
                                    ),
                                );
                            }
                        }
                    }
                    3 => {
                        if prev_neighbour.last_touching_side == 3 {
                            if prev_neighbour.intersection.ll.y > next_neighbour.intersection.ur.y {
                                self.insert_incomplete_room_90(
                                    room_id,
                                    layer,
                                    &room_box,
                                    IntBox::new(
                                        board_bounds.ll.x,
                                        next_neighbour.intersection.ur.y,
                                        room_box.ll.x,
                                        prev_neighbour.intersection.ll.y,
                                    ),
                                );
                            }
                        } else if next_neighbour.intersection.ur.y < room_box.ur.y
                            || prev_neighbour.intersection.ll.x > room_box.ll.x
                        {
                            if is_obstacle {
                                if prev_neighbour.last_touching_side == 2 {
                                    self.insert_incomplete_room_90(
                                        room_id,
                                        layer,
                                        &room_box,
                                        IntBox::new(
                                            room_box.ll.x,
                                            room_box.ur.y,
                                            prev_neighbour.intersection.ll.x,
                                            board_bounds.ur.y,
                                        ),
                                    );
                                }
                                self.insert_incomplete_room_90(
                                    room_id,
                                    layer,
                                    &room_box,
                                    IntBox::new(
                                        board_bounds.ll.x,
                                        next_neighbour.intersection.ur.y,
                                        room_box.ll.x,
                                        room_box.ur.y,
                                    ),
                                );
                            } else {
                                self.insert_incomplete_room_90(
                                    room_id,
                                    layer,
                                    &room_box,
                                    IntBox::new(
                                        board_bounds.ll.x,
                                        next_neighbour.intersection.ur.y,
                                        prev_neighbour.intersection.ll.x,
                                        board_bounds.ur.y,
                                    ),
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
            prev_neighbour = next_neighbour;
        }
    }

    fn insert_incomplete_room_90(
        &mut self,
        room_id: RoomId,
        layer: i32,
        room_box: &IntBox,
        shape_box: IntBox,
    ) {
        if shape_box.dimension() == 2 {
            let new_contained_shape = room_box.intersection(&shape_box);
            if !new_contained_shape.is_empty() {
                let door_dimension = shape_box.intersection(room_box).dimension();
                if door_dimension > 0 {
                    let new_room = self.rooms.add_incomplete_room(
                        Some(shape_box.to_simplex()),
                        layer,
                        new_contained_shape.to_simplex(),
                    );
                    self.rooms.add_room_door(room_id, new_room, door_dimension);
                }
            }
        }
    }
}

fn remove_border_line_90(room_box: &IntBox, remove_edge_no: usize) -> IntBox {
    match remove_edge_no {
        0 => IntBox::new(
            room_box.ll.x,
            -Limits::CRIT_INT,
            room_box.ur.x,
            room_box.ur.y,
        ),
        1 => IntBox::new(
            room_box.ll.x,
            room_box.ll.y,
            Limits::CRIT_INT,
            room_box.ur.y,
        ),
        2 => IntBox::new(
            room_box.ll.x,
            room_box.ll.y,
            room_box.ur.x,
            Limits::CRIT_INT,
        ),
        3 => IntBox::new(
            -Limits::CRIT_INT,
            room_box.ll.y,
            room_box.ur.x,
            room_box.ur.y,
        ),
        _ => *room_box,
    }
}
