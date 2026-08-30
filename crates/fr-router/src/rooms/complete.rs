//! On-demand free-space room completion.
//!
//! Two algorithms live here:
//!
//! * `ShapeSearchTree.complete_shape` / `restrain_shape`
//!   (`board/ShapeSearchTree.java:555-782`) — the negative-space query that
//!   grows a maximal obstacle-free convex tile around a contained shape. The
//!   restrain/cut-line selection rules are transcribed line for line from
//!   `search-tree-obstacles.md` §4.1.
//! * `AutorouteEngine.complete_expansion_room` /
//!   `complete_neighbour_rooms` / `remove_complete_expansion_room`
//!   (`autoroute/AutorouteEngine.java:394-568`) — the lifecycle around it.

use fr_board::shape::TileShape;
use fr_geometry::planar::{IntBox, Line, LineSegment, Side, Simplex};

use crate::engine::{AutorouteEngine, Obstacle};
use crate::geom;
use crate::rooms::{RoomId, RoomKind};

/// One candidate produced by [`AutorouteEngine::complete_shape`]: a completed
/// shape together with the contained shape that must stay inside it.
#[derive(Debug, Clone)]
pub struct CompletedShape {
    /// The completed convex tile.
    pub shape: Simplex,
    /// The shape that had to stay inside it.
    pub contained: Simplex,
    /// The exact octagon flavor of [`shape`](CompletedShape::shape), when the
    /// candidate came out of the 45° chain
    /// ([`complete_shape_45`](AutorouteEngine::complete_shape_45)).
    ///
    /// Carrying it means the room the candidate becomes gets its octagon memo
    /// seeded with the value the completion actually computed, instead of
    /// re-deriving it from the simplex it was just turned into.
    pub oct: Option<fr_geometry::planar::IntOctagon>,
}

impl AutorouteEngine<'_> {
    /// `ShapeSearchTree.complete_shape(room, net_no, ignore_object, ignore_shape)`
    /// (`ShapeSearchTree.java:555-653`).
    ///
    /// `shape` of `None` is upstream's unbounded start shape; the real start is
    /// always `board.bounding_box ∩ shape`.
    pub fn complete_shape(
        &mut self,
        shape: Option<&Simplex>,
        contained: &Simplex,
        layer: i32,
        ignore_room: Option<RoomId>,
        ignore_shape: Option<&Simplex>,
    ) -> Vec<CompletedShape> {
        // `AutorouteEngine.java:70` picks the tree, and the tree's subclass is
        // what decides which `complete_shape` runs. The dispatch lives here so
        // the whole lifecycle below — `complete_expansion_room`, the re-completion
        // of the second and later candidates, `try_remove_edge` — is shared.
        if self.is_fortyfive_completion() {
            return self.complete_shape_45(shape, contained, layer, ignore_room, ignore_shape);
        }
        if contained.is_empty() {
            return Vec::new();
        }
        let bounds_simplex = self.bounds().to_simplex();
        let start_shape = match shape {
            Some(shape) => bounds_simplex.intersection(shape),
            None => bounds_simplex,
        };
        let mut result: Vec<CompletedShape> = Vec::new();
        if start_shape.dimension() == 2 {
            result.push(CompletedShape {
                shape: start_shape,
                contained: contained.clone(),
                oct: None,
            });
        }
        if result.is_empty() {
            return result;
        }

        // The running bound is the union of the surviving candidates' boxes.
        // Upstream keeps a `RegularTileShape`; the pruning gets *stronger* as
        // obstacles carve the rooms, so an `IntBox` union is the same idea with
        // one fewer flavor.
        let mut bounding = result[0].shape.bounding_box();

        // The obstacle list is gathered once against the running bound, which
        // is upstream's single tree walk (`:578-595`). Obstacles that only
        // touch the *initial* bound and not a later, smaller one are simply
        // no-ops in the carve loop below.
        //
        // The query goes in as an `IntBox`, not as `bounding.to_simplex()`:
        // upstream's walk here filters leaves by `curr_node.bounding_shape
        // .intersects(bounding_shape)` (`:585`) — a *regular* shape test — and
        // never runs a shape-exact test at this stage; the exact one is the
        // `intersection.dimension() == 2` gate below (`:617-620`). Handing the
        // tree a simplex made `TileShape::intersects` take its simplex arm and
        // run a full exact `Simplex::intersection` per candidate leaf, which
        // the Issue034 profile put at 52% of the whole run.
        //
        // The box query returns a *superset* of the simplex query's answer —
        // `IntBox::intersects` is inclusive, and any non-empty shape overlap
        // implies a bounds overlap — and every extra entry is provably a no-op:
        // it fails either the running-bound guard below or the
        // `dimension() == 2` gate, and `own_net` is unused here.
        //
        // `:598` — the leaves are sorted before the carve loop, and with the
        // *completion* comparator, not the neighbour one, which is why the
        // query is the `_for_completion` entry point rather than
        // `overlapping_obstacles_tile` followed by a second sort of the same
        // list.
        let mut obstacles = Vec::new();
        let mut own_net = Vec::new();
        self.overlapping_obstacles_tile_for_completion(
            &TileShape::Box(bounding),
            Some(layer),
            self.ctrl.net_no,
            &mut obstacles,
            &mut own_net,
        );

        for obstacle in obstacles {
            if Some(obstacle) == ignore_room.map(Obstacle::Room) {
                continue;
            }
            // Layer and bounds first, shape second. The walk above ran against
            // the *initial* bound, so by now `bounding` has usually shrunk past
            // most of what it returned — 95.7 % of the layer-matching obstacles
            // on `Issue230` — and
            // [`obstacle_layer_and_bounds`](AutorouteEngine::obstacle_layer_and_bounds)
            // answers both questions off the stored tile, without building the
            // simplex the rejected ones would immediately drop. It is the same
            // rejection, not a weaker one; see that method for why the octagon
            // row's wider box cannot change an answer.
            let Some((obstacle_layer, obstacle_bounds)) = self.obstacle_layer_and_bounds(obstacle)
            else {
                continue;
            };
            if obstacle_layer != layer
                || obstacle_bounds.is_empty()
                || !obstacle_bounds.intersects(&bounding)
            {
                continue;
            }
            let Some(obstacle_shape) = self.obstacle_shape(obstacle) else {
                continue;
            };

            let mut new_result: Vec<CompletedShape> = Vec::new();
            let mut new_bounding: Option<IntBox> = None;
            for candidate in &result {
                let intersection = candidate.shape.intersection(&obstacle_shape);
                let mut something_changed = false;
                if intersection.dimension() == 2 {
                    // `:625-628`: a completed room whose overlap is contained in
                    // the from-door shape is the room we came through — leaving
                    // it alone is what stops the tiling re-carving itself.
                    let ignore_expansion_room = matches!(obstacle, Obstacle::Room(_))
                        && ignore_shape
                            .map(|ignore| geom::contains_shape(ignore, &intersection))
                            .unwrap_or(false);
                    if !ignore_expansion_room {
                        something_changed = true;
                        let restrained = self.restrain_shape(candidate, &obstacle_shape);
                        for room in restrained {
                            union_into(&mut new_bounding, &room.shape);
                            new_result.push(room);
                        }
                    }
                }
                if !something_changed {
                    union_into(&mut new_bounding, &candidate.shape);
                    new_result.push(candidate.clone());
                }
            }
            result = new_result;
            match new_bounding {
                Some(bb) => bounding = bb,
                None => return Vec::new(),
            }
        }

        self.divide_large_rooms(result)
    }

    /// `ShapeSearchTree.restrain_shape` (`:664-782`), the geometric heart.
    ///
    /// Pass 1 picks, among the obstacle border lines whose *segment* crosses
    /// the room's interior, the one with the largest
    /// [`distance_to_the_left`](crate::geom::distance_to_the_left) from the
    /// contained shape, and cuts the room with the opposite half plane. Pass 2
    /// (reached only when no such line exists, i.e. the obstacle overlaps the
    /// contained shape) finds a border line collinear with the contained shape,
    /// cuts the room in both directions, and recurses on the near piece.
    fn restrain_shape(
        &mut self,
        room: &CompletedShape,
        obstacle_shape: &Simplex,
    ) -> Vec<CompletedShape> {
        let mut result = Vec::new();
        if room.contained.is_empty() {
            return result;
        }
        if !self.enter_restrain() {
            return result;
        }
        let obstacle_simplex = obstacle_shape.clone();
        let room_shape = &room.shape;
        let shape_to_be_contained = &room.contained;

        let mut cut_line: Option<Line> = None;
        let mut cut_line_distance = -1.0f64;
        for i in 0..obstacle_simplex.border_line_count() {
            let Some(segment) = border_segment(&obstacle_simplex, i) else {
                continue;
            };
            if !geom::is_intersected_interior_by(room_shape, &segment) {
                continue;
            }
            let curr_line = obstacle_simplex.border_line(i);
            let curr_min_distance = geom::distance_to_the_left(shape_to_be_contained, &curr_line);
            if curr_min_distance > cut_line_distance {
                cut_line_distance = curr_min_distance;
                cut_line = Some(curr_line.opposite());
            }
        }

        if let Some(cut_line) = cut_line {
            let piece = room_shape.intersection(&geom::half_plane(&cut_line));
            if piece.dimension() >= 2 {
                result.push(CompletedShape {
                    shape: piece,
                    contained: shape_to_be_contained.clone(),
                    oct: None,
                });
            }
            self.leave_restrain();
            return result;
        }

        // No line has the whole contained shape on its right.
        if shape_to_be_contained.dimension() < 1 {
            // There is already a completed room around it (`:723-726`).
            self.leave_restrain();
            return result;
        }
        let mut split_line: Option<Line> = None;
        for i in 0..obstacle_simplex.border_line_count() {
            let Some(segment) = border_segment(&obstacle_simplex, i) else {
                continue;
            };
            if !geom::is_intersected_interior_by(room_shape, &segment) {
                continue;
            }
            let curr_line = obstacle_simplex.border_line(i);
            if geom::side_of_line(shape_to_be_contained, &curr_line) == Side::Collinear {
                split_line = Some(curr_line.opposite());
                break;
            }
        }
        let Some(split_line) = split_line else {
            // Parts of the room may already be occupied from somewhere else.
            self.leave_restrain();
            return result;
        };

        let cut_half_plane = geom::half_plane(&split_line);
        let new_contained = shape_to_be_contained.intersection(&cut_half_plane);
        let piece = room_shape.intersection(&cut_half_plane);
        if piece.dimension() >= 2 {
            result.push(CompletedShape {
                shape: piece,
                contained: new_contained,
                oct: None,
            });
        }
        let opposite_half_plane = geom::half_plane(&split_line.opposite());
        let rest_piece = room_shape.intersection(&opposite_half_plane);
        if rest_piece.dimension() >= 2 {
            let rest_contained = shape_to_be_contained.intersection(&opposite_half_plane);
            let rest = CompletedShape {
                shape: rest_piece,
                contained: rest_contained,
                oct: None,
            };
            result.extend(self.restrain_shape(&rest, obstacle_shape));
        }
        self.leave_restrain();
        result
    }

    /// `ShapeSearchTree.divide_large_room` (`:1057-1090`): a single result room
    /// whose box is wider than half the board in **both** axes is split into
    /// sections of `0.5 × max(board_h, board_w)` — *"otherwise the maze search
    /// algorithm gets problems with vias"*.
    pub(super) fn divide_large_rooms(&self, rooms: Vec<CompletedShape>) -> Vec<CompletedShape> {
        if rooms.len() != 1 {
            return rooms;
        }
        let board_box = self.bounds();
        let room = &rooms[0];
        if !room.shape.is_bounded() {
            return rooms;
        }
        let room_box = room.shape.bounding_box();
        let half_board_width = 0.5 * board_box.width() as f64;
        let half_board_height = 0.5 * board_box.height() as f64;
        if (room_box.width() as f64) <= half_board_width
            || (room_box.height() as f64) <= half_board_height
        {
            return rooms;
        }
        let max_section_width = 0.5 * (board_box.height().max(board_box.width()) as f64);
        let sections = room.shape.divide_into_sections(max_section_width);
        if sections.len() <= 1 {
            return rooms;
        }
        // `:1078-1087` verbatim: the contained shape of a section is
        // `section ∩ contained` and nothing else. A section that misses the
        // contained shape keeps an **empty** contained shape and dies where
        // upstream lets it die — `restrain_shape` bails on an empty contained
        // shape (`:689-693`), so the section is dropped the first time it is
        // re-completed, and `complete_expansion_room` re-completes every
        // candidate but the first.
        //
        // Substituting the section for an empty intersection, which this used
        // to do, is not a harmless default: in the 45° chain the substituted
        // room is immediately equal to its own contained shape and the
        // endless-loop guard (`ShapeSearchTree45Degree.java:216-218`) deletes
        // it, so the very split this function exists to make — *"otherwise the
        // maze search algorithm gets problems with vias"* — collapsed back to
        // one room. In the any-angle chain it did the opposite and kept
        // sections upstream discards. There is no `dimension() == 2` filter
        // either: `try_remove_edge` gates on `new_rooms.len() != 1`
        // (`SortedRoomNeighbours.java:360-364`), so dropping a section here
        // silently changes an edge-removal verdict.
        let divided: Vec<CompletedShape> = sections
            .into_iter()
            .map(|section| {
                let contained = room.contained.intersection(&section);
                CompletedShape {
                    shape: section,
                    contained,
                    oct: None,
                }
            })
            .collect();
        if crate::rooms::trace::is_enabled() {
            crate::rooms::trace::divide(
                divided.len(),
                divided.iter().filter(|s| !s.contained.is_empty()).count(),
            );
        }
        divided
    }

    /// `AutorouteEngine.complete_expansion_room` (`:394-477`).
    ///
    /// The room is consumed: it is detached and removed from the incomplete
    /// list, and the completed rooms take its place. Conformance appendix D10:
    /// the first/rest split is on the first **2-dimensional** candidate, not on
    /// raw index 0, and a candidate whose door calculation collapses it is
    /// silently dropped.
    pub fn complete_expansion_room(&mut self, room_id: RoomId) -> Vec<RoomId> {
        fr_metrics::counters::bump::room_completions(1);
        let mut result = Vec::new();
        if !self.rooms.room(room_id).alive
            || self.rooms.room(room_id).kind != RoomKind::IncompleteFree
        {
            return result;
        }

        // Find the 2-dim door to an already complete free room; that room and
        // its door shape become `ignore_object` / `from_door_shape` (`:399-411`).
        let mut from_door_shape = None;
        let mut ignore_room = None;
        for door_id in self.rooms.room(room_id).doors.clone() {
            let door = self.rooms.door(door_id);
            let Some(other) = door.other_room_any(room_id) else {
                continue;
            };
            if door.dimension == 2 && self.rooms.room(other).kind == RoomKind::CompleteFree {
                from_door_shape = Some(door.shape.clone());
                ignore_room = Some(other);
                break;
            }
        }

        let (shape, contained, layer) = {
            let room = self.rooms.room(room_id);
            (room.shape.clone(), room.contained_shape.clone(), room.layer)
        };
        let shape_arg = if shape.border_line_count() == 0 {
            None
        } else {
            Some(&shape)
        };
        if crate::rooms::trace::is_enabled() {
            crate::rooms::trace::input(
                self.ctrl.net_no,
                layer,
                &shape,
                &contained,
                from_door_shape.as_ref(),
            );
        }
        let completed = self.complete_shape(
            shape_arg,
            &contained,
            layer,
            ignore_room,
            from_door_shape.as_ref(),
        );

        self.remove_incomplete_expansion_room(room_id);

        let mut is_first = true;
        for candidate in completed {
            if candidate.shape.dimension() != 2 {
                continue;
            }
            if is_first {
                is_first = false;
                if let Some(room) = self.add_complete_room(&candidate, layer) {
                    result.push(room);
                }
            } else {
                // The first room's shape may have changed during door
                // calculation, so the rest are re-completed (`:437-459`).
                let recompleted = self.complete_shape(
                    Some(&candidate.shape),
                    &candidate.contained,
                    layer,
                    ignore_room,
                    from_door_shape.as_ref(),
                );
                for room in recompleted {
                    if room.shape.dimension() != 2 {
                        continue;
                    }
                    if let Some(room) = self.add_complete_room(&room, layer) {
                        result.push(room);
                    }
                }
            }
        }
        result
    }

    /// `AutorouteEngine.add_complete_room` (`:484-500`): calculate the doors,
    /// then — if the shape is still 2-dimensional — append to the complete list
    /// and insert into the room tree.
    fn add_complete_room(&mut self, candidate: &CompletedShape, layer: i32) -> Option<RoomId> {
        let room_id = match candidate.oct {
            Some(oct) => self.rooms.add_complete_free_room_oct(oct, layer),
            None => self
                .rooms
                .add_complete_free_room(candidate.shape.clone(), layer),
        };
        self.rooms.room_mut(room_id).contained_shape = candidate.contained.clone();
        self.calculate_doors(room_id);
        if !self.rooms.room(room_id).alive || self.rooms.room(room_id).shape.dimension() != 2 {
            self.rooms.room_mut(room_id).alive = false;
            return None;
        }
        self.rooms.complete.push(room_id);
        self.insert_room_in_tree(room_id);
        if crate::rooms::trace::is_enabled() {
            let shape = self.rooms.room(room_id).shape.clone();
            crate::rooms::trace::added(self.ctrl.net_no, layer, &shape);
        }
        Some(room_id)
    }

    /// `AutorouteEngine.remove_incomplete_expansion_room` (`:346-349`).
    pub fn remove_incomplete_expansion_room(&mut self, room_id: RoomId) {
        self.remove_all_doors(room_id);
        self.rooms.incomplete.retain(|id| *id != room_id);
        self.rooms.room_mut(room_id).alive = false;
    }

    /// `AutorouteEngine.remove_all_doors` (`:588-601`): detach every door and
    /// drop any incomplete room that hung off one.
    pub fn remove_all_doors(&mut self, room_id: RoomId) {
        for door_id in self.rooms.room(room_id).doors.clone() {
            let Some(other) = self.rooms.door(door_id).other_room_any(room_id) else {
                continue;
            };
            self.rooms.detach_door(door_id);
            if other != room_id && self.rooms.room(other).kind == RoomKind::IncompleteFree {
                self.remove_incomplete_expansion_room(other);
            }
        }
        self.rooms.room_mut(room_id).doors.clear();
    }

    /// `AutorouteEngine.remove_complete_expansion_room` (`:359-394`).
    ///
    /// Every 1-dimensional neighbour gets a fresh half-plane incomplete room
    /// and a door back — the repair frontier that keeps the tiling connected —
    /// before the room leaves the tree, the list and the drill-page cache.
    pub fn remove_complete_expansion_room(&mut self, room_id: RoomId) {
        let (room_shape, room_layer) = {
            let room = self.rooms.room(room_id);
            (room.shape.clone(), room.layer)
        };
        for door_id in self.rooms.room(room_id).doors.clone() {
            let Some(neighbour) = self.rooms.door(door_id).other_room_any(room_id) else {
                continue;
            };
            self.rooms.detach_door(door_id);
            let neighbour_shape = self.rooms.room(neighbour).shape.clone();
            let intersection = room_shape.intersection(&neighbour_shape);
            if intersection.dimension() != 1 {
                continue;
            }
            let Some((_, neighbour_side)) = geom::touching_sides(&room_shape, &neighbour_shape)
            else {
                continue;
            };
            let line = neighbour_shape.border_line(neighbour_side).opposite();
            let new_shape = geom::half_plane(&line);
            let new_room =
                self.rooms
                    .add_incomplete_room(Some(new_shape), room_layer, intersection);
            self.rooms.add_room_door(neighbour, new_room, 1);
        }
        self.remove_all_doors(room_id);
        self.remove_room_from_tree(room_id);
        self.rooms.complete.retain(|id| *id != room_id);
        self.rooms.room_mut(room_id).alive = false;
        self.pages.invalidate(&room_shape);
    }

    /// `AutorouteEngine.complete_neighbour_rooms` (`:532-557`).
    ///
    /// Completing an incomplete neighbour mutates the entered room's door list,
    /// so upstream restarts the iterator (`:551`). Here the walk is an index
    /// cursor over the door list with the list re-read on every step, which is
    /// the same fixed point without the O(doors²) restart.
    pub fn complete_neighbour_rooms(&mut self, room_id: RoomId) {
        let mut index = 0usize;
        // Doors already visited; a restart must not re-complete them.
        let mut visited: Vec<crate::rooms::DoorId> = Vec::new();
        loop {
            let doors = &self.rooms.room(room_id).doors;
            if index >= doors.len() {
                break;
            }
            let door_id = doors[index];
            if visited.contains(&door_id) {
                index += 1;
                continue;
            }
            visited.push(door_id);
            let Some(neighbour) = self.rooms.door(door_id).other_room_any(room_id) else {
                index += 1;
                continue;
            };
            match self.rooms.room(neighbour).kind {
                RoomKind::IncompleteFree => {
                    self.complete_expansion_room(neighbour);
                    // The door list changed under us: restart the scan, but the
                    // `visited` set keeps it linear.
                    index = 0;
                }
                RoomKind::Obstacle => {
                    if !self.rooms.room(neighbour).doors_calculated {
                        self.calculate_doors(neighbour);
                        self.rooms.room_mut(neighbour).doors_calculated = true;
                    }
                    index += 1;
                }
                RoomKind::CompleteFree => index += 1,
            }
        }
    }
}

fn union_into(bounds: &mut Option<IntBox>, shape: &Simplex) {
    if !shape.is_bounded() {
        // An unbounded candidate makes the running bound useless; upstream's
        // `bounding_shape` would report the coordinate envelope, so do the same.
        let crit = fr_geometry::planar::Limits::CRIT_INT;
        *bounds = Some(IntBox::new(-crit, -crit, crit, crit));
        return;
    }
    let bb = shape.bounding_box();
    *bounds = Some(match bounds.take() {
        Some(existing) => existing.union(&bb),
        None => bb,
    });
}

/// `new LineSegment(simplex, i)` — the border segment of a bounded corner pair.
fn border_segment(shape: &Simplex, index: usize) -> Option<LineSegment> {
    let count = shape.border_line_count();
    if count < 2 {
        return None;
    }
    let prev = shape.border_line(geom::prev_no(count, index));
    let curr = shape.border_line(index);
    let next = shape.border_line(geom::next_no(count, index));
    Some(LineSegment::new(prev, curr, next))
}

#[cfg(test)]
mod tests {
    use super::{Obstacle, TileShape};
    use crate::test_support::TestFixture;
    use fr_geometry::planar::IntBox;

    #[test]
    fn an_empty_board_completes_to_the_whole_bounding_box() {
        let fixture = TestFixture::empty_two_layer();
        let mut engine = fixture.engine();
        let contained = IntBox::new(0, 0, 10, 10).to_simplex();
        let completed = engine.complete_shape(None, &contained, 0, None, None);
        // The only obstacle is the board outline ring, so the whole outline
        // interior survives and `divide_large_room` splits it: the single
        // result is wider than half the board in *both* axes
        // (`ShapeSearchTree.java:1057-1090`).
        assert!(completed.len() > 1, "a board-sized room must be divided");
        assert!(completed.iter().all(|c| c.shape.dimension() == 2));
        let origin = fr_geometry::planar::Point::Int(fr_geometry::planar::IntPoint::new(5, 5));
        assert!(completed.iter().any(|c| c.shape.contains(&origin)));
        // The pieces tile the same area they were cut from, with no overlap.
        let total: f64 = completed.iter().map(|c| c.shape.area()).sum();
        for (i, a) in completed.iter().enumerate() {
            for b in completed.iter().skip(i + 1) {
                assert!(a.shape.intersection(&b.shape).dimension() < 2);
            }
        }
        assert!(total > 0.0);
    }

    /// `divide_large_room` (`ShapeSearchTree.java:1078-1087`) gives each section
    /// `section ∩ contained` and nothing else: the sections the contained shape
    /// reaches keep a clipped copy of it, the rest keep an **empty** contained
    /// shape.
    ///
    /// Substituting the section itself for an empty intersection reads like a
    /// harmless default and is not: the contained shape is what
    /// `restrain_shape` measures its cut line against and what the 45° chain's
    /// endless-loop guard (`ShapeSearchTree45Degree.java:216-218`) compares the
    /// room to, so a section carrying *itself* as its contained shape is
    /// deleted on the spot by that guard — the split this function exists to
    /// make collapses back to a single room.
    #[test]
    fn a_divided_section_that_misses_the_contained_shape_keeps_an_empty_one() {
        let fixture = TestFixture::empty_two_layer();
        let mut engine = fixture.engine();
        // Well inside one section: the board is 6000 × 3000 and the sections
        // are 3000 wide, so a contained shape on the origin would straddle the
        // split and reach both.
        let contained = IntBox::new(1000, 0, 1010, 10).to_simplex();
        let completed = engine.complete_shape(None, &contained, 0, None, None);
        assert!(completed.len() > 1, "a board-sized room must be divided");

        let with_contained = completed
            .iter()
            .filter(|section| !section.contained.is_empty())
            .count();
        assert!(
            with_contained >= 1 && with_contained < completed.len(),
            "the board-sized contained shape reaches some sections, not all: \
             {with_contained} of {}",
            completed.len()
        );
        for section in &completed {
            // The signature of the old fallback: a section that *is* its own
            // contained shape.
            assert!(
                section.contained.is_empty()
                    || section.contained.area() < section.shape.area(),
                "a section must never carry itself as its contained shape"
            );
            assert!(
                section.contained.is_empty() || section.contained.area() <= contained.area(),
                "a section's contained shape is the original clipped, nothing more"
            );
        }
    }

    #[test]
    fn an_obstacle_carves_the_completed_shape() {
        // The pads belong to the routed net and are therefore *not* trace
        // obstacles for it; the foreign-net trace is.
        let fixture = TestFixture::with_foreign_trace();
        let mut engine = fixture.engine();
        let contained = IntBox::new(-1900, -10, -1800, 10).to_simplex();
        let completed = engine.complete_shape(None, &contained, 0, None, None);
        assert!(!completed.is_empty());
        let trace = fixture.trace_id();
        let tile_count = engine.tree_shape_count(trace);
        assert!(tile_count > 0);
        for candidate in &completed {
            for index in 0..tile_count as u16 {
                if let Some(shape) = engine.item_tile(trace, index) {
                    let overlap = candidate.shape.intersection(&shape);
                    assert!(
                        overlap.dimension() < 2,
                        "completed room overlaps the foreign trace"
                    );
                }
            }
        }
    }

    #[test]
    fn completing_a_start_room_produces_a_live_complete_room() {
        let fixture = TestFixture::two_layer_with_pads();
        let mut engine = fixture.engine();
        let contained = IntBox::new(-10, -10, 10, 10).to_simplex();
        let incomplete = engine.rooms.add_incomplete_room(None, 0, contained);
        let completed = engine.complete_expansion_room(incomplete);
        assert!(!completed.is_empty());
        assert!(!engine.rooms.room(incomplete).alive);
        for room in completed {
            assert!(engine.rooms.room(room).alive);
            assert_eq!(
                engine.rooms.room(room).kind,
                crate::rooms::RoomKind::CompleteFree
            );
        }
    }

    /// The carve loop rejects an obstacle on the cheap box from
    /// [`AutorouteEngine::obstacle_layer_and_bounds`] before it materializes
    /// [`AutorouteEngine::obstacle_shape`]. That substitution is only sound
    /// while the cheap box **contains** the materialized shape's own bounding
    /// box: a superset can only make the guard more permissive, and a more
    /// permissive guard cannot change an answer (the extra obstacle fails the
    /// `dimension() == 2` gate and leaves `result` and `bounding` alone). A
    /// *subset* would silently drop a real obstacle, i.e. route through copper.
    ///
    /// Asserted over every obstacle a board-sized query returns on each
    /// fixture, items and rooms alike, so it covers all three tile flavors the
    /// accessor switches on.
    #[test]
    fn the_cheap_obstacle_box_contains_the_materialised_one() {
        for fixture in [
            TestFixture::two_layer_with_pads(),
            TestFixture::with_foreign_trace(),
            TestFixture::pure_smd_two_layer(),
        ] {
            let mut engine = fixture.engine();
            // A complete free room in the query's way, so the `Obstacle::Room`
            // arm is exercised and not just the item ones.
            let room_shape = IntBox::new(-100, -100, 100, 100).to_simplex();
            let room = engine.rooms.add_complete_free_room(room_shape, 0);
            engine.mint_room_id(room);
            engine.insert_room_in_tree(room);

            let bound = engine.bounds();
            let mut obstacles = Vec::new();
            let mut own_net = Vec::new();
            engine.overlapping_obstacles_tile_for_completion(
                &TileShape::Box(bound),
                Some(0),
                crate::test_support::TEST_NET,
                &mut obstacles,
                &mut own_net,
            );
            assert!(
                obstacles.iter().any(|o| matches!(o, Obstacle::Item(_, _)))
                    && obstacles.contains(&Obstacle::Room(room)),
                "the probe must see both obstacle kinds on {:?}",
                obstacles.len()
            );

            for obstacle in obstacles {
                let cheap = engine.obstacle_layer_and_bounds(obstacle);
                let exact = engine.obstacle_shape(obstacle);
                match (cheap, exact) {
                    (Some((layer, cheap_box)), Some(shape)) => {
                        assert_eq!(
                            Some(layer),
                            engine.obstacle_layer(obstacle),
                            "{obstacle:?}: the combined accessor disagrees with obstacle_layer"
                        );
                        let exact_box = shape.bounding_box();
                        if exact_box.is_empty() {
                            continue;
                        }
                        assert!(
                            cheap_box.contains_box(&exact_box),
                            "{obstacle:?}: cheap box {cheap_box:?} does not contain the \
                             materialised {exact_box:?} — the prefilter would drop a real \
                             obstacle"
                        );
                    }
                    (None, None) => {}
                    (cheap, exact) => panic!(
                        "{obstacle:?}: the two accessors disagree on existence \
                         ({}, {})",
                        cheap.is_some(),
                        exact.is_some()
                    ),
                }
            }
        }
    }
}
