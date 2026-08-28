//! `SortedRoomNeighbours` — door creation and gap-room tiling.
//!
//! Transcribed from `autoroute/SortedRoomNeighbours.java` (716 lines, the
//! **any-angle** variant). `maze-engine.md` §13 prescribes porting this one
//! first: *"it is the general case; 90°/45° are specializations that can share
//! the gap-room builder via an `AngleFamily` trait"*.
//!
//! # Angle families
//!
//! `AutorouteEngine.calculate_doors` dispatches on the tree subclass
//! (`:526-536`): 90° → `SortedOrthogonalRoomNeighbours`, 45° →
//! `Sorted45DegreeRoomNeighbours`, else this one. The two specializations are
//! *not* ported in M2a; every board goes through the any-angle path. The
//! consequence is bounded and worth stating precisely, because the default
//! board flavor is 45°:
//!
//! * **Correctness is unaffected.** The specializations differ in how they
//!   choose which untouched edges to remove in `try_remove_edge` (all eight
//!   octagon edges at once versus one simplex edge at a time) and in the
//!   coordinate comparisons of the neighbour sort. Both produce a valid maximal
//!   tiling; the any-angle one just takes more iterations to get there.
//! * **Tiling geometry differs**, so the settle sequence will not match the lab
//!   jar on a 45° board even before the id remapping. See the crate docs.

use fr_board::item::{ItemClass, ItemId};
use fr_datastructures::Signum;
use fr_geometry::planar::{Line, Point, Side, Simplex};

use crate::engine::{AutorouteEngine, Obstacle};
use crate::geom;
use crate::rooms::{RoomId, RoomKind};

/// Tolerance of the neighbour sort's distance comparison
/// (`SortedRoomNeighbour.c_dist_tolerance`, `:564`).
const DIST_TOLERANCE: f64 = 1.0;

/// One touching neighbour of the room being completed.
#[derive(Debug, Clone)]
struct SortedNeighbour {
    /// The deterministic tie-break identity (`:666`), standing in for
    /// `search_tree_object.get_id_no()`.
    obstacle_id: i64,
    neighbour_shape: Simplex,
    touching_side_no_of_room: usize,
    touching_side_no_of_neighbour_room: usize,
    room_touch_is_corner: bool,
    neighbour_room_touch_is_corner: bool,
    first_corner: Point,
    last_corner: Point,
}

impl AutorouteEngine<'_> {
    /// `AutorouteEngine.calculate_doors` → `SortedRoomNeighbours.calculate`
    /// (`:52-86`).
    ///
    /// Computes the neighbours of `room_id`, inserts the doors, runs the
    /// `try_remove_edge` maximality loop and builds the gap rooms. The room's
    /// own shape may shrink inside this call (the corner cut-off at `:457-478`).
    pub fn calculate_doors(&mut self, room_id: RoomId) {
        // `:60-65`: an edge removal restarts the whole calculation. The loop is
        // bounded by the strict area increase the removal is gated on.
        loop {
            let (neighbours, own_net_objects) = self.calculate_neighbours(room_id);
            if self.try_remove_edge(room_id, &neighbours) {
                self.remove_all_doors(room_id);
                continue;
            }
            if neighbours.is_empty() {
                if self.rooms.room(room_id).kind == RoomKind::Obstacle {
                    self.calculate_incomplete_rooms_with_empty_neighbours(room_id);
                }
            } else {
                self.calculate_new_incomplete_rooms(room_id, &neighbours);
            }
            if self.rooms.room(room_id).kind == RoomKind::CompleteFree {
                self.calculate_target_doors(room_id, &own_net_objects);
            }
            return;
        }
    }

    /// `SortedRoomNeighbours.calculate_neighbours` (`:125-241`).
    fn calculate_neighbours(
        &mut self,
        room_id: RoomId,
    ) -> (Vec<SortedNeighbour>, Vec<(ItemId, u16)>) {
        let (room_shape, layer, is_obstacle_room) = {
            let room = self.rooms.room(room_id);
            (
                room.shape.clone(),
                room.layer,
                room.kind == RoomKind::Obstacle,
            )
        };
        let mut obstacles = Vec::new();
        let mut own_net_objects = Vec::new();
        // Upstream queries with no net filter and splits inside the loop; our
        // tree query already splits, and for an obstacle room the own-net
        // deferral does not apply (`:156`, guarded on `IncompleteFreeSpaceExpansionRoom`).
        self.overlapping_obstacles(
            &room_shape,
            Some(layer),
            self.ctrl.net_no,
            &mut obstacles,
            &mut own_net_objects,
        );
        if is_obstacle_room {
            // `:156` only defers for a free room being completed; for an
            // obstacle room the same-net items are ordinary neighbours.
            let mut promoted: Vec<Obstacle> = own_net_objects
                .drain(..)
                .map(|(item, index)| Obstacle::Item(item, index))
                .collect();
            promoted.append(&mut obstacles);
            promoted.sort_unstable();
            obstacles = promoted;
        }

        let mut neighbours: Vec<SortedNeighbour> = Vec::new();
        for obstacle in obstacles {
            if obstacle == Obstacle::Room(room_id) {
                continue;
            }
            if let (Obstacle::Item(item, index), Some((room_item, room_index))) =
                (obstacle, self.rooms.room(room_id).item)
            {
                if item == room_item && index == room_index {
                    continue;
                }
            }
            let Some(neighbour_shape) = self.obstacle_shape(obstacle) else {
                continue;
            };
            let intersection = room_shape.intersection(&neighbour_shape);
            let dimension = intersection.dimension();

            if dimension > 1 {
                // `:164-176`: only an obstacle room may have a 2-dim overlap,
                // and only with another routable item of the same net.
                if is_obstacle_room {
                    if let Obstacle::Item(item, index) = obstacle {
                        self.create_overlap_door(room_id, item, index);
                    }
                }
                continue;
            }
            if dimension < 0 {
                continue;
            }

            if dimension == 1 {
                let Some((side_of_room, side_of_neighbour)) =
                    geom::touching_sides(&room_shape, &neighbour_shape)
                else {
                    continue;
                };
                neighbours.push(self.make_neighbour(
                    obstacle,
                    &room_shape,
                    neighbour_shape.clone(),
                    side_of_room,
                    side_of_neighbour,
                    false,
                    false,
                ));
                // `:188-205`: make sure there is a door to the neighbour room.
                let neighbour_room = match obstacle {
                    Obstacle::Room(other) => Some(other),
                    Obstacle::Item(item, index) => {
                        let routable = self
                            .board
                            .store()
                            .get(item)
                            .map(|item| item.is_routable())
                            .unwrap_or(false);
                        if routable {
                            self.obstacle_room(item, index)
                        } else {
                            // A non-routable obstacle is a pure wall: no door.
                            None
                        }
                    }
                };
                if let Some(neighbour_room) = neighbour_room {
                    if self.insert_door_ok(room_id, neighbour_room, &intersection) {
                        self.rooms.add_room_door(room_id, neighbour_room, 1);
                    }
                }
            } else {
                // dimension == 0: a point touch. No door, but the corner
                // bookkeeping is needed to build correct gap rooms (`:206-238`).
                let touching_point = intersection.corner(0);
                let (room_touch_is_corner, side_of_room) =
                    match geom::equals_corner(&room_shape, &touching_point) {
                        Some(corner) => (true, corner),
                        None => {
                            match geom::contains_on_border_line_no(&room_shape, &touching_point) {
                                Some(side) => (false, side),
                                None => continue,
                            }
                        }
                    };
                let neighbour_line_count = neighbour_shape.border_line_count();
                let (neighbour_touch_is_corner, side_of_neighbour) =
                    match geom::equals_corner(&neighbour_shape, &touching_point) {
                        // "The previous border line is preferred to make the
                        // shape of the incomplete room as big as possible."
                        Some(corner) => (true, geom::prev_no(neighbour_line_count, corner)),
                        None => match geom::contains_on_border_line_no(
                            &neighbour_shape,
                            &touching_point,
                        ) {
                            Some(side) => (false, side),
                            None => continue,
                        },
                    };
                neighbours.push(self.make_neighbour(
                    obstacle,
                    &room_shape,
                    neighbour_shape.clone(),
                    side_of_room,
                    side_of_neighbour,
                    room_touch_is_corner,
                    neighbour_touch_is_corner,
                ));
            }
        }

        sort_neighbours(&mut neighbours, &room_shape);
        (neighbours, own_net_objects)
    }

    #[allow(clippy::too_many_arguments)]
    fn make_neighbour(
        &self,
        obstacle: Obstacle,
        room_shape: &Simplex,
        neighbour_shape: Simplex,
        touching_side_no_of_room: usize,
        touching_side_no_of_neighbour_room: usize,
        room_touch_is_corner: bool,
        neighbour_room_touch_is_corner: bool,
    ) -> SortedNeighbour {
        let first_corner = first_corner(
            room_shape,
            &neighbour_shape,
            touching_side_no_of_room,
            touching_side_no_of_neighbour_room,
            room_touch_is_corner,
            neighbour_room_touch_is_corner,
        );
        let last_corner = last_corner(
            room_shape,
            &neighbour_shape,
            touching_side_no_of_room,
            touching_side_no_of_neighbour_room,
            room_touch_is_corner,
            neighbour_room_touch_is_corner,
        );
        SortedNeighbour {
            obstacle_id: self.obstacle_id(obstacle),
            neighbour_shape,
            touching_side_no_of_room,
            touching_side_no_of_neighbour_room,
            room_touch_is_corner,
            neighbour_room_touch_is_corner,
            first_corner,
            last_corner,
        }
    }

    /// The `get_id_no()` the neighbour sort's tie-break reads (`:666`).
    fn obstacle_id(&self, obstacle: Obstacle) -> i64 {
        match obstacle {
            Obstacle::Item(item, index) => ((item.raw() as i64) << 10) | index as i64,
            // Rooms sort after every item, matching the obstacle visit order.
            Obstacle::Room(room) => (1i64 << 48) + self.rooms.room(room).id_no,
        }
    }

    /// `ObstacleExpansionRoom.create_overlap_door` (`:75-100`): a 2-dimensional
    /// door between two obstacle rooms of the same net. For two pieces of the
    /// same trace only consecutive segments get one.
    fn create_overlap_door(&mut self, room_id: RoomId, item: ItemId, index: u16) {
        let routable = self
            .board
            .store()
            .get(item)
            .map(|item| item.is_routable())
            .unwrap_or(false);
        if !routable {
            return;
        }
        let Some(other_room) = self.obstacle_room(item, index) else {
            return;
        };
        if !self.insert_overlap_door_ok(room_id, other_room) {
            return;
        }
        self.rooms.add_room_door(room_id, other_room, 2);
    }

    fn insert_overlap_door_ok(&self, first: RoomId, second: RoomId) -> bool {
        if first == second || self.rooms.door_exists(first, second) {
            return false;
        }
        let (Some((first_item, first_index)), Some((second_item, second_index))) =
            (self.rooms.room(first).item, self.rooms.room(second).item)
        else {
            return false;
        };
        let (Some(a), Some(b)) = (
            self.board.store().get(first_item),
            self.board.store().get(second_item),
        ) else {
            return false;
        };
        if !a.shares_net(b) {
            return false;
        }
        if first_item == second_item && a.class() == ItemClass::Trace {
            // Only consecutive segments of the same trace get a door.
            let delta = first_index as i32 - second_index as i32;
            return delta.abs() == 1;
        }
        true
    }

    /// `SortedRoomNeighbours.insert_door_ok` (`:246-301`).
    fn insert_door_ok(&self, first: RoomId, second: RoomId, door_shape: &Simplex) -> bool {
        if first == second || self.rooms.door_exists(first, second) {
            return false;
        }
        let first_is_obstacle = self.rooms.room(first).kind == RoomKind::Obstacle;
        let second_is_obstacle = self.rooms.room(second).kind == RoomKind::Obstacle;
        if first_is_obstacle && second_is_obstacle {
            // Overlap doors only between items of the same net, "for
            // performance reasons".
            let (Some((a, _)), Some((b, _))) =
                (self.rooms.room(first).item, self.rooms.room(second).item)
            else {
                return false;
            };
            let (Some(a), Some(b)) = (self.board.store().get(a), self.board.store().get(b)) else {
                return false;
            };
            return a.shares_net(b);
        }
        if !first_is_obstacle && !second_is_obstacle {
            return true;
        }

        // A 1-dimensional door onto the first or last tile of a trace must be
        // parallel to the trace's end line, "otherwise there may be check ripup
        // problems with entering at the wrong side at a fork" (`:259-260`).
        let mut door_line = None;
        let corner_count = door_shape.border_line_count();
        if corner_count > 1 {
            let mut prev_corner = door_shape.corner(0);
            for i in 1..corner_count {
                let curr_corner = door_shape.corner(i);
                if curr_corner != prev_corner {
                    door_line = Some(door_shape.border_line(i - 1));
                    break;
                }
                prev_corner = curr_corner;
            }
        }
        let Some(door_line) = door_line else {
            return false;
        };
        if first_is_obstacle && !self.trace_end_door_ok(first, &door_line) {
            return false;
        }
        if second_is_obstacle && !self.trace_end_door_ok(second, &door_line) {
            return false;
        }
        true
    }

    /// `SortedRoomNeighbours.insert_door_ok(ObstacleExpansionRoom, Line)`
    /// (`:287-301`).
    fn trace_end_door_ok(&self, room_id: RoomId, door_line: &Line) -> bool {
        let Some((item_id, index)) = self.rooms.room(room_id).item else {
            return true;
        };
        let Some(item) = self.board.store().get(item_id) else {
            return true;
        };
        let fr_board::item::ItemKind::Trace(trace) = &item.kind else {
            return true;
        };
        let tile_count = self.tree_shape_count(item_id);
        if tile_count == 0 {
            return true;
        }
        if index as usize != 0 && index as usize != tile_count - 1 {
            return true;
        }
        // `curr_trace.polyline().arr[room_index + 1]` — the trace's own line at
        // the end the door touches.
        let line_index = index as usize + 1;
        match trace.polyline.arr.get(line_index) {
            Some(trace_line) => trace_line.is_parallel(door_line),
            None => true,
        }
    }

    /// `SortedRoomNeighbours.try_remove_edge` (`:313-390`).
    ///
    /// An edge with no touching neighbour is not obstacle-constrained, so the
    /// room is not maximal: drop the border line, re-complete, and — only when
    /// the area strictly grew — adopt the enlarged shape and restart.
    fn try_remove_edge(&mut self, room_id: RoomId, neighbours: &[SortedNeighbour]) -> bool {
        if self.rooms.room(room_id).kind != RoomKind::CompleteFree {
            return false;
        }
        // The incomplete room this complete room was made from is the one whose
        // shape upstream mutates; here the complete room carries both.
        let room_simplex = self.rooms.room(room_id).shape.clone();
        if !room_simplex.is_bounded() {
            return false;
        }
        let room_shape_area = geom::area(&room_simplex);

        let mut remove_edge_no: i32 = -1;
        let mut prev_edge_no: i32 = -1;
        let mut curr_edge_no: i32 = 0;
        for neighbour in neighbours {
            let side = neighbour.touching_side_no_of_room as i32;
            if side == prev_edge_no {
                continue;
            }
            if side == curr_edge_no {
                prev_edge_no = curr_edge_no;
                curr_edge_no += 1;
            } else {
                remove_edge_no = curr_edge_no;
                break;
            }
        }
        if remove_edge_no < 0 && (curr_edge_no as usize) < room_simplex.border_line_count() {
            remove_edge_no = curr_edge_no;
        }
        if remove_edge_no < 0 {
            return false;
        }

        let enlarged_shape = room_simplex.remove_border_line(remove_edge_no as usize);
        let (contained, layer) = {
            let room = self.rooms.room(room_id);
            (room.contained_shape.clone(), room.layer)
        };
        let new_rooms = self.complete_shape(Some(&enlarged_shape), &contained, layer, None, None);
        if new_rooms.len() != 1 {
            return false;
        }
        let new_room = &new_rooms[0];
        if geom::area(&new_room.shape) <= room_shape_area {
            return false;
        }
        let shape = new_room.shape.clone();
        let contained = new_room.contained.clone();
        let room = self.rooms.room_mut(room_id);
        room.shape = shape;
        room.contained_shape = contained;
        true
    }

    /// `SortedRoomNeighbours.calculate_incomplete_rooms_with_empty_neighbours`
    /// (`:88-103`): one incomplete room per border line of an obstacle room
    /// that touches nothing.
    fn calculate_incomplete_rooms_with_empty_neighbours(&mut self, room_id: RoomId) {
        let (room_shape, layer) = {
            let room = self.rooms.room(room_id);
            (room.shape.clone(), room.layer)
        };
        for i in 0..room_shape.border_line_count() {
            let curr_line = room_shape.border_line(i);
            if !self.trace_end_door_ok(room_id, &curr_line) {
                continue;
            }
            let new_room_shape = geom::half_plane(&curr_line.opposite());
            let new_contained = room_shape.intersection(&new_room_shape);
            if new_contained.is_empty() {
                continue;
            }
            let new_room =
                self.rooms
                    .add_incomplete_room(Some(new_room_shape), layer, new_contained);
            self.rooms.add_room_door(room_id, new_room, 1);
        }
    }

    /// `SortedRoomNeighbours.calculate_target_doors` (`:105-123`).
    ///
    /// Every deferred same-net tree entry that is connectable, carries the
    /// routed net and whose connection shape meets the room becomes a target
    /// door — and any deferred entry at all marks the room net-dependent.
    fn calculate_target_doors(&mut self, room_id: RoomId, own_net_objects: &[(ItemId, u16)]) {
        if own_net_objects.is_empty() {
            return;
        }
        self.rooms.room_mut(room_id).net_dependent = true;
        for (item_id, index) in own_net_objects {
            let Some(item) = self.board.store().get(*item_id) else {
                continue;
            };
            if !item.is_connectable() || !item.contains_net(self.ctrl.net_no) {
                continue;
            }
            let Some(connection_shape) = self.trace_connection_shape(*item_id, *index) else {
                continue;
            };
            if !self.rooms.room(room_id).shape.intersects(&connection_shape) {
                continue;
            }
            self.rooms
                .add_target_door(*item_id, *index, room_id, &connection_shape);
        }
    }

    /// `SortedRoomNeighbours.calculate_new_incomplete_rooms` (`:395-557`).
    ///
    /// Walks the neighbours counterclockwise around the border and, for every
    /// gap between consecutive touch intervals, builds the half-plane
    /// incomplete room(s) that span it plus a 1-dimensional door back. The
    /// any-angle corner cut-off (`:450-478`) shrinks the completed room itself,
    /// which is why this runs after `try_remove_edge` and not before.
    fn calculate_new_incomplete_rooms(&mut self, room_id: RoomId, neighbours: &[SortedNeighbour]) {
        if neighbours.is_empty() {
            return;
        }
        let room_simplex = self.rooms.room(room_id).shape.clone();
        let line_count = room_simplex.border_line_count();
        if line_count == 0 {
            return;
        }
        let layer = self.rooms.room(room_id).layer;
        let is_free_room = self.rooms.room(room_id).kind == RoomKind::CompleteFree;
        let neighbour_count = neighbours.len();

        let mut prev_index = neighbour_count - 1;
        for next_index in 0..neighbour_count {
            let prev_neighbour = &neighbours[prev_index];
            let next_neighbour = &neighbours[next_index];
            let prev_is_last = prev_index == neighbour_count - 1;

            let mut first_touching_side_no = prev_neighbour.touching_side_no_of_room;
            let mut last_touching_side_no = next_neighbour.touching_side_no_of_room;
            let curr_next_no = geom::next_no(line_count, first_touching_side_no);

            let sides_differ = first_touching_side_no != last_touching_side_no || prev_is_last;
            let prev_ends_at_corner =
                sides_differ && prev_neighbour.last_corner == room_simplex.corner(curr_next_no);
            let next_starts_at_corner = sides_differ
                && next_neighbour.first_corner == room_simplex.corner(last_touching_side_no);

            if prev_ends_at_corner {
                first_touching_side_no = curr_next_no;
            }
            if next_starts_at_corner {
                last_touching_side_no = geom::prev_no(line_count, last_touching_side_no);
            }
            let neighbours_touch =
                neighbour_count > 1 && prev_neighbour.last_corner == next_neighbour.first_corner;

            if !neighbours_touch {
                let mut last_bounding_line_no = prev_neighbour.touching_side_no_of_neighbour_room;
                if !(prev_ends_at_corner || prev_neighbour.room_touch_is_corner) {
                    last_bounding_line_no = geom::prev_no(
                        prev_neighbour.neighbour_shape.border_line_count(),
                        last_bounding_line_no,
                    );
                }
                let mut first_bounding_line_no = next_neighbour.touching_side_no_of_neighbour_room;
                if !(next_starts_at_corner || next_neighbour.neighbour_room_touch_is_corner) {
                    first_bounding_line_no = geom::next_no(
                        next_neighbour.neighbour_shape.border_line_count(),
                        first_bounding_line_no,
                    );
                }
                let mut start_edge_line = Some(
                    next_neighbour
                        .neighbour_shape
                        .border_line(first_bounding_line_no)
                        .opposite(),
                );
                let mut curr_touching_side_no = last_touching_side_no;
                let mut first_time = true;

                loop {
                    let mut corner_cut_off = false;
                    let mut middle_edge_line: Option<Line> = None;
                    if is_free_room
                        && curr_touching_side_no == last_touching_side_no
                        && first_touching_side_no != last_touching_side_no
                    {
                        // The corner cut-off: a line from the previous
                        // neighbour's last corner to the next neighbour's first
                        // corner trims the room's outstanding corner in open
                        // space, so that the incomplete-room geometry stays
                        // consistent (`:453-478`).
                        let cut_line_start = prev_neighbour.last_corner.to_float().round();
                        let cut_line_end = next_neighbour.first_corner.to_float().round();
                        if cut_line_start != cut_line_end {
                            let cut_line = Line::new(cut_line_start, cut_line_end);
                            let cut_half_plane = geom::half_plane(&cut_line);
                            let new_shape =
                                self.rooms.room(room_id).shape.intersection(&cut_half_plane);
                            self.rooms.room_mut(room_id).shape = new_shape;
                            corner_cut_off = geom::side_of_line(
                                &self.rooms.room(room_id).contained_shape,
                                &cut_line,
                            ) == Side::OnTheLeft;
                            if corner_cut_off {
                                middle_edge_line = Some(cut_line.opposite());
                            }
                        }
                    }
                    let next_touching_side_no = geom::prev_no(line_count, curr_touching_side_no);
                    if !corner_cut_off {
                        middle_edge_line =
                            Some(room_simplex.border_line(curr_touching_side_no).opposite());
                    }
                    let middle_edge_line = middle_edge_line.expect("set on both branches");
                    let middle_line_dir = middle_edge_line.direction();

                    let last_time = (curr_touching_side_no == first_touching_side_no
                        && !(prev_is_last && first_time))
                        || corner_cut_off;

                    let mut end_edge_line = if last_time {
                        let line = prev_neighbour
                            .neighbour_shape
                            .border_line(last_bounding_line_no)
                            .opposite();
                        if line.direction().side_of(&middle_line_dir) != Side::OnTheLeft {
                            // Concave corner between the middle and last line.
                            None
                        } else {
                            Some(line)
                        }
                    } else {
                        None
                    };
                    if let Some(start) = start_edge_line {
                        if middle_line_dir.side_of(&start.direction()) != Side::OnTheLeft {
                            // Concave corner between the first and middle line.
                            start_edge_line = None;
                        }
                    }
                    let mut new_edge_lines: Vec<Line> = Vec::with_capacity(3);
                    if let Some(start) = start_edge_line {
                        new_edge_lines.push(start);
                    }
                    new_edge_lines.push(middle_edge_line);
                    if let Some(end) = end_edge_line.take() {
                        new_edge_lines.push(end);
                    }
                    let new_room_shape = Simplex::get_instance(&new_edge_lines);
                    if !new_room_shape.is_empty() {
                        let new_contained =
                            self.rooms.room(room_id).shape.intersection(&new_room_shape);
                        if !new_contained.is_empty() {
                            let new_room = self.rooms.add_incomplete_room(
                                Some(new_room_shape),
                                layer,
                                new_contained,
                            );
                            self.rooms.add_room_door(room_id, new_room, 1);
                        }
                    }
                    if last_time {
                        break;
                    }
                    curr_touching_side_no = next_touching_side_no;
                    start_edge_line = None;
                    first_time = false;
                }
            }
            prev_index = next_index;
        }
    }
}

/// `SortedRoomNeighbour.compareTo` (`:612-669`), applied as a sort.
fn sort_neighbours(neighbours: &mut [SortedNeighbour], room_shape: &Simplex) {
    neighbours.sort_by(|a, b| compare_neighbours(a, b, room_shape));
}

fn compare_neighbours(
    this: &SortedNeighbour,
    other: &SortedNeighbour,
    room_shape: &Simplex,
) -> std::cmp::Ordering {
    let side_delta = this.touching_side_no_of_room as i64 - other.touching_side_no_of_room as i64;
    if side_delta != 0 {
        return side_delta.cmp(&0);
    }
    let compare_corner = room_shape.corner_approx(this.touching_side_no_of_room);
    let this_distance = this.first_corner.to_float().distance(&compare_corner);
    let other_distance = other.first_corner.to_float().distance(&compare_corner);
    let mut delta_distance = this_distance - other_distance;

    if delta_distance.abs() <= DIST_TOLERANCE && this.first_corner == other.first_corner {
        let this_distance2 = this.last_corner.to_float().distance(&compare_corner);
        let other_distance2 = other.last_corner.to_float().distance(&compare_corner);
        delta_distance = this_distance2 - other_distance2;
        if delta_distance.abs() <= DIST_TOLERANCE
            && this.neighbour_room_touch_is_corner
            && other.neighbour_room_touch_is_corner
        {
            // Otherwise there may be a short 1-dim touch at a link between two
            // trace lines, where equality is correct (`:643-659`).
            let mut compare_line_no = this.touching_side_no_of_room;
            if this.room_touch_is_corner {
                compare_line_no = geom::prev_no(room_shape.border_line_count(), compare_line_no);
            }
            let compare_dir = room_shape
                .border_line(compare_line_no)
                .direction()
                .opposite();
            let this_line = this
                .neighbour_shape
                .border_line(this.touching_side_no_of_neighbour_room);
            let other_line = other
                .neighbour_shape
                .border_line(other.touching_side_no_of_neighbour_room);
            delta_distance =
                compare_dir.compare_from(&this_line.direction(), &other_line.direction()) as f64;
        }
    }

    let res = Signum::as_int(delta_distance);
    if res != 0 {
        return res.cmp(&0);
    }
    // Deterministic tie-break for identical geometry (`:664-667`).
    this.obstacle_id.cmp(&other.obstacle_id)
}

/// `SortedRoomNeighbour.first_corner` (`:674-692`).
fn first_corner(
    room_shape: &Simplex,
    neighbour_shape: &Simplex,
    touching_side_no_of_room: usize,
    touching_side_no_of_neighbour_room: usize,
    room_touch_is_corner: bool,
    neighbour_room_touch_is_corner: bool,
) -> Point {
    if room_touch_is_corner {
        return room_shape.corner(touching_side_no_of_room);
    }
    if neighbour_room_touch_is_corner {
        return neighbour_shape.corner(touching_side_no_of_neighbour_room);
    }
    let neighbour_line_count = neighbour_shape.border_line_count();
    let curr_first_corner = neighbour_shape.corner(geom::next_no(
        neighbour_line_count,
        touching_side_no_of_neighbour_room,
    ));
    let prev_line = room_shape.border_line(geom::prev_no(
        room_shape.border_line_count(),
        touching_side_no_of_room,
    ));
    if prev_line.side_of_point(&curr_first_corner) == Side::OnTheRight {
        curr_first_corner
    } else {
        // The corner is outside the door shape.
        room_shape.corner(touching_side_no_of_room)
    }
}

/// `SortedRoomNeighbour.last_corner` (`:697-715`).
fn last_corner(
    room_shape: &Simplex,
    neighbour_shape: &Simplex,
    touching_side_no_of_room: usize,
    touching_side_no_of_neighbour_room: usize,
    room_touch_is_corner: bool,
    neighbour_room_touch_is_corner: bool,
) -> Point {
    if room_touch_is_corner {
        return room_shape.corner(touching_side_no_of_room);
    }
    if neighbour_room_touch_is_corner {
        return neighbour_shape.corner(touching_side_no_of_neighbour_room);
    }
    let curr_last_corner = neighbour_shape.corner(touching_side_no_of_neighbour_room);
    let line_count = room_shape.border_line_count();
    let next_line = room_shape.border_line(geom::next_no(line_count, touching_side_no_of_room));
    if next_line.side_of_point(&curr_last_corner) == Side::OnTheRight {
        curr_last_corner
    } else {
        room_shape.corner(geom::next_no(line_count, touching_side_no_of_room))
    }
}

#[cfg(test)]
mod tests {
    use crate::rooms::RoomKind;
    use crate::test_support::TestFixture;
    use fr_geometry::planar::IntBox;

    #[test]
    fn an_empty_board_completion_yields_only_complete_free_rooms() {
        // With no obstacles at all the board box survives `complete_shape` and
        // is quartered by `divide_large_room`; every piece must come back as a
        // live, 2-dimensional complete free room with its doors calculated.
        let fixture = TestFixture::empty_two_layer();
        let mut engine = fixture.engine();
        let contained = IntBox::new(0, 0, 10, 10).to_simplex();
        let incomplete = engine.rooms.add_incomplete_room(None, 0, contained);
        let rooms = engine.complete_expansion_room(incomplete);
        assert!(!rooms.is_empty());
        for room in rooms {
            assert_eq!(engine.rooms.room(room).kind, RoomKind::CompleteFree);
            assert!(engine.rooms.room(room).alive);
            assert_eq!(engine.rooms.room(room).shape.dimension(), 2);
        }
    }

    #[test]
    fn a_pad_of_the_routed_net_becomes_a_target_door() {
        let fixture = TestFixture::two_layer_with_pads();
        let mut engine = fixture.engine();
        let pad = fixture.pad_ids()[0];
        let shape = engine.item_tile(pad, 0).expect("pad tile");
        let incomplete = engine.rooms.add_incomplete_room(None, 0, shape);
        let rooms = engine.complete_expansion_room(incomplete);
        let target_doors: usize = rooms
            .iter()
            .map(|room| engine.rooms.room(*room).target_doors.len())
            .sum();
        assert!(
            target_doors >= 1,
            "the start pad must produce at least one target door"
        );
        assert!(rooms
            .iter()
            .any(|room| engine.rooms.room(*room).net_dependent));
    }

    #[test]
    fn a_foreign_obstacle_produces_a_door_only_when_routable() {
        // The pads belong to the routed net in this fixture, so the completed
        // room around the origin must not carry a door to a *non-routable*
        // wall. The board outline is such a wall.
        let fixture = TestFixture::two_layer_with_pads();
        let mut engine = fixture.engine();
        let contained = IntBox::new(-10, -10, 10, 10).to_simplex();
        let incomplete = engine.rooms.add_incomplete_room(None, 0, contained);
        let rooms = engine.complete_expansion_room(incomplete);
        for room in rooms {
            for door in engine.rooms.room(room).doors.clone() {
                let other = engine.rooms.door(door).other_room_any(room).unwrap();
                if let Some(item) = engine.rooms.room(other).obstacle_item() {
                    let item = engine.board.store().get(item).unwrap();
                    assert!(item.is_routable(), "a wall must not get a door");
                }
            }
        }
    }
}
