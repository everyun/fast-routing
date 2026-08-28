//! The room and door arenas — upstream's `ExpansionRoom` / `ExpansionDoor`
//! hierarchy flattened to indices.
//!
//! ```text
//! ExpansionRoom                     →  Room { kind: RoomKind, .. }, RoomId(u32)
//!  ├─ IncompleteFreeSpaceExpansionRoom  →  RoomKind::IncompleteFree
//!  ├─ CompleteFreeSpaceExpansionRoom    →  RoomKind::CompleteFree
//!  └─ ObstacleExpansionRoom             →  RoomKind::Obstacle
//! ExpansionDoor                     →  Door { kind: DoorKind::Room, .. }, DoorId(u32)
//! TargetItemExpansionDoor           →  Door { kind: DoorKind::Target, .. }
//! ExpansionDrill / DrillPage        →  their own arenas (see `crate::maze::drills`)
//! ExpandableObject                  →  ExpandableRef
//! ```
//!
//! Three structural departures from Java, all sanctioned by `maze-engine.md`
//! §13 and each load-bearing:
//!
//! 1. **Real door ids.** `ExpansionDoor.get_id_no()` hashes the two room ids as
//!    `min·31 + max` and can collide, weakening the queue tie-break
//!    (`ExpansionDoor.java:189-195`). Every expandable object here gets a
//!    monotonic [`ObjectIds`] value instead, so the tie-break is total.
//! 2. **Cached door geometry.** `get_shape()` recomputes `first ∩ second` on
//!    every call and is called several times per expansion
//!    (`ExpansionDoor.java:51-56`). The intersection and its diagonal segment
//!    are computed once, at door creation, and invalidated with the room.
//! 3. **Node blocks, not per-section objects.** Sections address
//!    [`crate::maze::nodes::NodeArena`] through a block base, so the
//!    between-connection reset is a generation bump.

use fr_board::item::ItemId;
use fr_datastructures::arena_shape_tree::ArenaLeafId;
use fr_geometry::planar::{FloatLine, Simplex};

use crate::geom;
use crate::maze::nodes::{NodeArena, NodeIndex};

pub mod complete;
pub mod neighbours;

/// Index of a room in [`RoomArena`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RoomId(pub u32);

/// Index of a door in [`RoomArena`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DoorId(pub u32);

/// Index of an expansion drill in the drill arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DrillId(pub u32);

/// Index of a drill page in the drill-page grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PageId(pub u32);

/// Upstream's `ExpandableObject`: whatever the frontier can point at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExpandableRef {
    /// An [`ExpansionDoor`](DoorKind::Room) or a
    /// [`TargetItemExpansionDoor`](DoorKind::Target).
    Door(DoorId),
    /// A candidate via site.
    Drill(DrillId),
    /// The drill-page indirection between a room and its drills.
    Page(PageId),
}

impl RoomId {
    #[inline]
    pub(crate) fn get(self) -> usize {
        self.0 as usize
    }
}

impl DoorId {
    #[inline]
    pub(crate) fn get(self) -> usize {
        self.0 as usize
    }
}

/// Which of the three upstream room classes a [`Room`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomKind {
    /// `IncompleteFreeSpaceExpansionRoom` — a frontier placeholder whose shape
    /// is a lower bound and whose `contained_shape` must survive completion.
    IncompleteFree,
    /// `CompleteFreeSpaceExpansionRoom` — a maximal obstacle-free convex tile,
    /// itself an obstacle in the engine-local tree.
    CompleteFree,
    /// `ObstacleExpansionRoom` — one tile shape of one routable item, so the
    /// search can plan to rip it up or shove it.
    Obstacle,
}

/// A room.
#[derive(Debug, Clone)]
pub struct Room {
    /// Which upstream class this is.
    pub kind: RoomKind,
    /// The room's convex shape. Possibly unbounded while incomplete.
    pub shape: Simplex,
    /// The shape that must stay inside the completed shape
    /// (`FreeSpaceExpansionRoom.java:20-27`). Only meaningful while
    /// [`RoomKind::IncompleteFree`].
    pub contained_shape: Simplex,
    /// The single layer the room lives on.
    pub layer: i32,
    /// Doors to neighbouring rooms.
    pub doors: Vec<DoorId>,
    /// Doors to start/destination items. Only complete free rooms have these.
    pub target_doors: Vec<DoorId>,
    /// Ordering identity, used wherever upstream reads `get_id_no()`.
    ///
    /// Free rooms take the engine's monotonic room counter
    /// (`AutorouteEngine.generate_room_id_no`, `:289`); obstacle rooms take
    /// `(item_id << 10) | index_in_item` (`ObstacleExpansionRoom.java:55-57`).
    pub id_no: i64,
    /// Set when the room's shape or target doors depend on the routed net
    /// (`SortedRoomNeighbours.calculate_target_doors`, `:105-108`).
    pub net_dependent: bool,
    /// For an obstacle room: the item and which of its tile shapes.
    pub item: Option<(ItemId, u16)>,
    /// For an obstacle room: whether `calculate_doors` has already run
    /// (`complete_neighbour_rooms`' once-only guard).
    pub doors_calculated: bool,
    /// Leaf id in the engine-local room tree, for complete free rooms.
    pub leaf: Option<ArenaLeafId>,
    /// False once the room has been removed; ids are never recycled, so a
    /// stale reference reads as dead rather than as a different room.
    pub alive: bool,
}

impl Room {
    /// True for a room the maze may stand in (`CompleteExpansionRoom`).
    #[inline]
    pub fn is_complete(&self) -> bool {
        matches!(self.kind, RoomKind::CompleteFree | RoomKind::Obstacle)
    }

    /// The item this obstacle room wraps.
    #[inline]
    pub fn obstacle_item(&self) -> Option<ItemId> {
        match self.kind {
            RoomKind::Obstacle => self.item.map(|(id, _)| id),
            _ => None,
        }
    }
}

/// Which of the two upstream door classes a [`Door`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoorKind {
    /// `ExpansionDoor` between two rooms.
    Room,
    /// `TargetItemExpansionDoor` from a room to a connectable item.
    Target {
        /// The start/destination item.
        item: ItemId,
        /// Which of its tree shapes.
        tree_entry_no: u16,
    },
}

/// A door.
#[derive(Debug, Clone)]
pub struct Door {
    /// Monotonic object id — the queue tie-break and the settle-trace key.
    pub oid: u32,
    /// Room door or target door.
    pub kind: DoorKind,
    /// 1 for a shared boundary segment, 2 for an area overlap
    /// (`ExpansionDoor.dimension`; a target door is always 2).
    pub dimension: i32,
    /// One side of the door. For a target door this is the room.
    pub first_room: RoomId,
    /// The other side. Equal to `first_room` for a target door.
    pub second_room: RoomId,
    /// The door shape, cached at creation (departure 2 in the module docs).
    pub shape: Simplex,
    /// The door line segment the sections are cut from, cached at creation.
    /// `None` when the door has no expandable segment (a 2-dim free/free door
    /// with one room inside the other, or a small 2-dim door).
    pub door_line: Option<FloatLine>,
    /// The offset the currently allocated section block was sized for.
    section_offset: f64,
    /// Number of sections currently allocated.
    section_count: u16,
    /// Base of this door's block in the node arena.
    node_base: Option<NodeIndex>,
    /// False once the door has been detached.
    pub alive: bool,
}

impl Door {
    /// The other side of the door, or `None` when `room` is neither side —
    /// and always `None` for a target door
    /// (`TargetItemExpansionDoor.other_room`, `:51-53`).
    #[inline]
    pub fn other_room(&self, room: RoomId) -> Option<RoomId> {
        if matches!(self.kind, DoorKind::Target { .. }) {
            return None;
        }
        if room == self.first_room {
            Some(self.second_room)
        } else if room == self.second_room {
            Some(self.first_room)
        } else {
            None
        }
    }

    /// [`other_room`](Self::other_room) ignoring the target-door rule; the
    /// engine needs it to walk the incomplete side of a door
    /// (`ExpansionDoor.other_room(ExpansionRoom)`, `:69-79`).
    #[inline]
    pub fn other_room_any(&self, room: RoomId) -> Option<RoomId> {
        if room == self.first_room {
            Some(self.second_room)
        } else if room == self.second_room {
            Some(self.first_room)
        } else {
            None
        }
    }

    /// Number of sections currently allocated.
    #[inline]
    pub fn section_count(&self) -> u16 {
        self.section_count
    }
}

/// The monotonic id source shared by doors, drills and pages.
#[derive(Debug, Clone, Copy, Default)]
pub struct ObjectIds {
    next: u32,
}

impl ObjectIds {
    /// Hands out the next id.
    #[inline]
    pub fn next_id(&mut self) -> u32 {
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        id
    }

    /// Resets the counter — `AutorouteEngine.clear()`'s
    /// `expansion_room_instance_count = 0` (`:288`).
    #[inline]
    pub fn reset(&mut self) {
        self.next = 0;
    }
}

/// The room and door storage of one engine.
#[derive(Debug, Default)]
pub struct RoomArena {
    rooms: Vec<Room>,
    doors: Vec<Door>,
    /// Ids of the incomplete rooms, in creation order — upstream's
    /// `incomplete_expansion_rooms` list.
    pub incomplete: Vec<RoomId>,
    /// Ids of the complete free rooms — upstream's `complete_expansion_rooms`.
    pub complete: Vec<RoomId>,
    /// The room-id counter (`generate_room_id_no`).
    room_id_no: i64,
    /// The object-id counter for doors and drills.
    object_ids: ObjectIds,
    /// The first object id this arena may hand out.
    ///
    /// Drill pages take the low ids `0..page_count` at engine construction, so
    /// the arena starts above them and every expandable object — door, drill,
    /// page — has a distinct queue tie-break key and a distinct settle-trace
    /// `door_id`. Upstream's `get_id_no()` makes no such promise: it hashes,
    /// and its hashes collide (`ExpansionDoor.java:189-195`).
    oid_base: u32,
}

impl RoomArena {
    /// An empty arena.
    pub fn new() -> RoomArena {
        RoomArena::default()
    }

    /// Drops every room and door. `AutorouteEngine.clear()` (`:280-291`).
    pub fn clear(&mut self) {
        self.rooms.clear();
        self.doors.clear();
        self.incomplete.clear();
        self.complete.clear();
        self.room_id_no = 0;
        self.object_ids.reset();
    }

    /// Sets the first object id this arena hands out.
    #[inline]
    pub fn set_oid_base(&mut self, base: u32) {
        self.oid_base = base;
    }

    /// The next object id for a door or a drill.
    #[inline]
    pub fn next_object_id(&mut self) -> u32 {
        self.oid_base.wrapping_add(self.object_ids.next_id())
    }

    /// Number of room slots ever allocated (dead ones included).
    #[inline]
    pub fn room_count(&self) -> usize {
        self.rooms.len()
    }

    /// Number of door slots ever allocated (dead ones included).
    #[inline]
    pub fn door_count(&self) -> usize {
        self.doors.len()
    }

    /// `generate_room_id_no()` — a monotonically increasing per-engine counter.
    #[inline]
    pub fn generate_room_id_no(&mut self) -> i64 {
        let id = self.room_id_no;
        self.room_id_no += 1;
        id
    }

    /// Read access to a room.
    #[inline]
    pub fn room(&self, id: RoomId) -> &Room {
        &self.rooms[id.get()]
    }

    /// Write access to a room.
    #[inline]
    pub fn room_mut(&mut self, id: RoomId) -> &mut Room {
        &mut self.rooms[id.get()]
    }

    /// Read access to a door.
    #[inline]
    pub fn door(&self, id: DoorId) -> &Door {
        &self.doors[id.get()]
    }

    /// Write access to a door.
    #[inline]
    pub fn door_mut(&mut self, id: DoorId) -> &mut Door {
        &mut self.doors[id.get()]
    }

    /// Adds an incomplete free-space room
    /// (`AutorouteEngine.add_incomplete_expansion_room`, `:317-329`).
    ///
    /// `shape` of `None` is upstream's `null` = the whole plane.
    pub fn add_incomplete_room(
        &mut self,
        shape: Option<Simplex>,
        layer: i32,
        contained_shape: Simplex,
    ) -> RoomId {
        let id = RoomId(self.rooms.len() as u32);
        self.rooms.push(Room {
            kind: RoomKind::IncompleteFree,
            shape: shape.unwrap_or_else(geom::whole_plane),
            contained_shape,
            layer,
            doors: Vec::new(),
            target_doors: Vec::new(),
            id_no: -1,
            net_dependent: false,
            item: None,
            doors_calculated: false,
            leaf: None,
            alive: true,
        });
        self.incomplete.push(id);
        id
    }

    /// Adds a complete free-space room with a fresh room id.
    pub fn add_complete_free_room(&mut self, shape: Simplex, layer: i32) -> RoomId {
        let id_no = self.generate_room_id_no();
        let id = RoomId(self.rooms.len() as u32);
        self.rooms.push(Room {
            kind: RoomKind::CompleteFree,
            contained_shape: shape.clone(),
            shape,
            layer,
            doors: Vec::new(),
            target_doors: Vec::new(),
            id_no,
            net_dependent: false,
            item: None,
            doors_calculated: false,
            leaf: None,
            alive: true,
        });
        id
    }

    /// Adds an obstacle room for one tile shape of one routable item.
    ///
    /// The id is `(item_id << 10) | index_in_item`
    /// (`ObstacleExpansionRoom.java:55-57`).
    pub fn add_obstacle_room(
        &mut self,
        item: ItemId,
        index_in_item: u16,
        shape: Simplex,
        layer: i32,
    ) -> RoomId {
        let id = RoomId(self.rooms.len() as u32);
        self.rooms.push(Room {
            kind: RoomKind::Obstacle,
            contained_shape: shape.clone(),
            shape,
            layer,
            doors: Vec::new(),
            target_doors: Vec::new(),
            id_no: ((item.raw() as i64) << 10) | index_in_item as i64,
            net_dependent: false,
            item: Some((item, index_in_item)),
            doors_calculated: false,
            leaf: None,
            alive: true,
        });
        id
    }

    /// Creates an [`ExpansionDoor`](DoorKind::Room) between two rooms and links
    /// it into both door lists.
    ///
    /// The door shape and its line segment are computed here, once.
    pub fn add_room_door(&mut self, first: RoomId, second: RoomId, dimension: i32) -> DoorId {
        let shape = self
            .room(first)
            .shape
            .intersection(&self.room(second).shape);
        fr_metrics::counters::bump::door_shape_computations(1);
        let door_line = self.door_line_for(first, second, dimension, &shape);
        let oid = self.next_object_id();
        let id = DoorId(self.doors.len() as u32);
        self.doors.push(Door {
            oid,
            kind: DoorKind::Room,
            dimension,
            first_room: first,
            second_room: second,
            shape,
            door_line,
            section_offset: f64::NAN,
            section_count: 0,
            node_base: None,
            alive: true,
        });
        self.room_mut(first).doors.push(id);
        self.room_mut(second).doors.push(id);
        id
    }

    /// Creates a [`TargetItemExpansionDoor`](DoorKind::Target) and links it into
    /// the room's target-door list (`TargetItemExpansionDoor`, `:22-33`).
    pub fn add_target_door(
        &mut self,
        item: ItemId,
        tree_entry_no: u16,
        room: RoomId,
        item_shape: &Simplex,
    ) -> DoorId {
        let shape = item_shape.intersection(&self.room(room).shape);
        let oid = self.next_object_id();
        let id = DoorId(self.doors.len() as u32);
        self.doors.push(Door {
            oid,
            kind: DoorKind::Target {
                item,
                tree_entry_no,
            },
            // `TargetItemExpansionDoor.get_dimension()` is a constant 2.
            dimension: 2,
            first_room: room,
            second_room: room,
            shape,
            door_line: None,
            section_offset: f64::NAN,
            // A target door has exactly one section (`:61-63`); the block is
            // allocated on first use like any other.
            section_count: 0,
            node_base: None,
            alive: true,
        });
        self.room_mut(room).target_doors.push(id);
        id
    }

    /// The cached door line segment, computed once per door.
    ///
    /// `ExpansionDoor.get_section_segments` (`:113-148`): a 1-dim door uses the
    /// shape's diagonal corner segment; a 2-dim door between two complete free
    /// rooms uses the restraint line
    /// ([`calc_door_line_segment`](Self::calc_door_line_segment)); anything else
    /// (an obstacle overlap door) degenerates to the centre of gravity.
    fn door_line_for(
        &self,
        first: RoomId,
        second: RoomId,
        dimension: i32,
        shape: &Simplex,
    ) -> Option<FloatLine> {
        if shape.is_empty() {
            return None;
        }
        if dimension == 1 {
            return geom::diagonal_corner_segment(shape);
        }
        let both_free = self.room(first).kind == RoomKind::CompleteFree
            && self.room(second).kind == RoomKind::CompleteFree;
        if dimension == 2 && both_free {
            return self.calc_door_line_segment(first, second, shape);
        }
        let gravity = shape.centre_of_gravity();
        Some(FloatLine::new(gravity, gravity))
    }

    /// `ExpansionDoor.calc_door_line_segment` (`:153-175`): the diagonal of a
    /// 2-dimensional door between two free rooms, taken through the two corners
    /// that lie on the border of *both* room shapes.
    ///
    /// `None` when one room is inside the other, which upstream reports by
    /// returning an empty section array.
    fn calc_door_line_segment(
        &self,
        first: RoomId,
        second: RoomId,
        door_shape: &Simplex,
    ) -> Option<FloatLine> {
        let first_shape = &self.room(first).shape;
        let second_shape = &self.room(second).shape;
        let mut first_corner = None;
        let mut second_corner = None;
        for i in 0..door_shape.border_line_count() {
            let corner = door_shape.corner(i);
            if fr_geometry::planar::Shape::contains_inside(first_shape, &corner)
                || fr_geometry::planar::Shape::contains_inside(second_shape, &corner)
            {
                continue;
            }
            match &first_corner {
                None => first_corner = Some(corner),
                Some(existing) if *existing != corner => {
                    second_corner = Some(corner);
                    break;
                }
                _ => {}
            }
        }
        let (a, b) = (first_corner?, second_corner?);
        Some(FloatLine::new(a.to_float(), b.to_float()))
    }

    /// `ExpansionDoor.get_section_segments(offset)` (`:113-148`).
    ///
    /// Fills `out` with the section segments and returns the section count;
    /// zero means "not expandable" (empty shape, one room inside the other, or
    /// a small 2-dim door). The node block is (re)allocated exactly as
    /// `allocate_sections` does: reused when the count matches, replaced
    /// otherwise.
    pub fn section_segments(
        &mut self,
        door_id: DoorId,
        offset: f64,
        nodes: &mut NodeArena,
        out: &mut Vec<FloatLine>,
    ) -> u16 {
        out.clear();
        // `:114` — the tolerance is added inside `get_section_segments`.
        let offset = offset + crate::ctrl::TRACE_WIDTH_TOLERANCE as f64;

        let (door_line, shrunk) = {
            let door = self.door(door_id);
            if door.shape.is_empty() {
                return 0;
            }
            match door.kind {
                DoorKind::Target { .. } => {
                    // A target door's single section is filled in by the
                    // caller from the connection point; the segment list is
                    // not used.
                    let count = 1u16;
                    self.ensure_sections(door_id, offset, count, nodes);
                    return count;
                }
                DoorKind::Room => {}
            }
            let Some(line) = door.door_line else {
                return 0;
            };
            if door.dimension == 1 {
                (line, geom::shrink_segment(&line, offset))
            } else if self.is_free_free(door_id) {
                // `:134-137` — 2-dimensional small doors are not expanded.
                if line.b.distance_square(&line.a) < 4.0 * offset * offset {
                    return 0;
                }
                (line, geom::shrink_segment(&line, offset))
            } else {
                (line, line)
            }
        };

        // `:144-145`: max section width is `10 × offset`, and the count is
        // derived from the *unshrunk* segment.
        let max_section_width = 10.0 * offset;
        let count = (door_line.b.distance(&door_line.a) / max_section_width) as usize + 1;
        let count = count.min(u16::MAX as usize) as u16;
        self.ensure_sections(door_id, offset, count, nodes);
        geom::divide_segment_into_sections(&shrunk, count as usize, out);
        count
    }

    fn is_free_free(&self, door_id: DoorId) -> bool {
        let door = self.door(door_id);
        self.room(door.first_room).kind == RoomKind::CompleteFree
            && self.room(door.second_room).kind == RoomKind::CompleteFree
    }

    /// `ExpansionDoor.allocate_sections` (`:201-209`) over the node arena.
    fn ensure_sections(&mut self, door_id: DoorId, offset: f64, count: u16, nodes: &mut NodeArena) {
        let door = &self.doors[door_id.get()];
        if door.node_base.is_some() && door.section_count == count {
            self.doors[door_id.get()].section_offset = offset;
            return;
        }
        let base = nodes.allocate(count as usize);
        let door = &mut self.doors[door_id.get()];
        door.node_base = Some(base);
        door.section_count = count;
        door.section_offset = offset;
    }

    /// The node index of `(door, section)`, or `None` when the door has not
    /// been sectioned yet or the section is out of range.
    #[inline]
    pub fn section_node(&self, door_id: DoorId, section: u16) -> Option<NodeIndex> {
        let door = self.door(door_id);
        if section >= door.section_count {
            return None;
        }
        door.node_base
            .map(|base| NodeIndex(base.0 + section as u32))
    }

    /// Ensures a target door has its single node allocated and returns it.
    pub fn target_node(&mut self, door_id: DoorId, nodes: &mut NodeArena) -> NodeIndex {
        if self.door(door_id).node_base.is_none() {
            self.ensure_sections(door_id, 0.0, 1, nodes);
        }
        self.section_node(door_id, 0)
            .expect("target door has one section")
    }

    /// Detaches `door` from both of its rooms and marks it dead.
    pub fn detach_door(&mut self, door_id: DoorId) {
        let (first, second, is_target) = {
            let door = self.door(door_id);
            (
                door.first_room,
                door.second_room,
                matches!(door.kind, DoorKind::Target { .. }),
            )
        };
        if is_target {
            self.room_mut(first).target_doors.retain(|d| *d != door_id);
        } else {
            self.room_mut(first).doors.retain(|d| *d != door_id);
            if second != first {
                self.room_mut(second).doors.retain(|d| *d != door_id);
            }
        }
        self.door_mut(door_id).alive = false;
    }

    /// True when a door between the two rooms already exists
    /// (`ExpansionRoom.door_exists`).
    pub fn door_exists(&self, first: RoomId, second: RoomId) -> bool {
        self.room(first)
            .doors
            .iter()
            .any(|door| self.door(*door).other_room_any(first) == Some(second))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fr_geometry::planar::IntBox;

    fn box_simplex(ll_x: i32, ll_y: i32, ur_x: i32, ur_y: i32) -> Simplex {
        IntBox::new(ll_x, ll_y, ur_x, ur_y).to_simplex()
    }

    #[test]
    fn a_one_dimensional_door_sections_at_ten_times_the_offset() {
        let mut arena = RoomArena::new();
        let mut nodes = NodeArena::new();
        let left = arena.add_complete_free_room(box_simplex(0, 0, 100, 1000), 0);
        let right = arena.add_complete_free_room(box_simplex(100, 0, 200, 1000), 0);
        let door = arena.add_room_door(left, right, 1);

        let mut out = Vec::new();
        // offset becomes 10 + 2 = 12 ⇒ max section width 120; the door
        // segment is 1000 long ⇒ floor(1000/120) + 1 = 9 sections.
        let count = arena.section_segments(door, 10.0, &mut nodes, &mut out);
        assert_eq!(count, 9);
        assert_eq!(out.len(), 9);
        assert_eq!(arena.door(door).section_count(), 9);
        // The sections cover the *shrunk* segment.
        // `shrink_segment` takes `offset` off *each* end (`FloatLine.java:202-215`).
        let shrunk_length = out
            .iter()
            .map(|section| section.a.distance(&section.b))
            .sum::<f64>();
        assert!(
            (shrunk_length - (1000.0 - 24.0)).abs() < 1e-6,
            "{shrunk_length}"
        );
    }

    #[test]
    fn sections_are_reused_when_the_count_matches() {
        let mut arena = RoomArena::new();
        let mut nodes = NodeArena::new();
        let left = arena.add_complete_free_room(box_simplex(0, 0, 100, 1000), 0);
        let right = arena.add_complete_free_room(box_simplex(100, 0, 200, 1000), 0);
        let door = arena.add_room_door(left, right, 1);
        let mut out = Vec::new();
        arena.section_segments(door, 10.0, &mut nodes, &mut out);
        let first_len = nodes.len();
        arena.section_segments(door, 10.0, &mut nodes, &mut out);
        assert_eq!(nodes.len(), first_len, "the block must be reused");
    }

    #[test]
    fn door_shape_is_cached_at_creation() {
        let mut arena = RoomArena::new();
        let left = arena.add_complete_free_room(box_simplex(0, 0, 100, 100), 0);
        let right = arena.add_complete_free_room(box_simplex(100, 0, 200, 100), 0);
        let door = arena.add_room_door(left, right, 1);
        assert_eq!(arena.door(door).shape.dimension(), 1);
        assert!(arena.door(door).door_line.is_some());
    }

    #[test]
    fn other_room_is_none_for_a_target_door() {
        let mut arena = RoomArena::new();
        let room = arena.add_complete_free_room(box_simplex(0, 0, 100, 100), 0);
        let door = arena.add_target_door(ItemId::from_raw(3), 0, room, &box_simplex(0, 0, 10, 10));
        assert_eq!(arena.door(door).other_room(room), None);
        assert_eq!(arena.room(room).target_doors, vec![door]);
    }

    #[test]
    fn object_ids_are_monotonic_and_never_collide() {
        let mut arena = RoomArena::new();
        let a = arena.add_complete_free_room(box_simplex(0, 0, 100, 100), 0);
        let b = arena.add_complete_free_room(box_simplex(100, 0, 200, 100), 0);
        let c = arena.add_complete_free_room(box_simplex(200, 0, 300, 100), 0);
        let first = arena.add_room_door(a, b, 1);
        let second = arena.add_room_door(b, c, 1);
        assert_ne!(arena.door(first).oid, arena.door(second).oid);
        assert!(arena.door(first).oid < arena.door(second).oid);
    }

    #[test]
    fn obstacle_room_id_packs_item_and_index() {
        let mut arena = RoomArena::new();
        let room = arena.add_obstacle_room(ItemId::from_raw(5), 3, box_simplex(0, 0, 10, 10), 0);
        assert_eq!(arena.room(room).id_no, (5 << 10) | 3);
    }
}
