//! `AutorouteEngine` — the room database one maze search runs against.
//!
//! Transcribed from `autoroute/AutorouteEngine.java`. What it owns:
//!
//! * the **compensated item tree**, borrowed from the board's
//!   [`SearchTreeManager`](fr_board::search_tree::SearchTreeManager) — read
//!   only, never mutated (the maze search never touches the board);
//! * an **engine-local room tree**, which is where this port differs
//!   structurally from Java. Upstream inserts every
//!   `CompleteFreeSpaceExpansionRoom` into the *same* `ShapeSearchTree` as the
//!   items, because a room is a `SearchTreeObject` there
//!   (`AutorouteEngine.java:498`). Our `SearchTree` is item-only and shared
//!   with the board, so rooms go into a private `ArenaShapeTree<RoomId>` and
//!   every obstacle query reads both. The union is what upstream's single tree
//!   contained; the only observable difference is the *order* obstacles are
//!   visited in, which is fixed below;
//! * the room/door arenas, the node arena, the incomplete/complete room lists
//!   and the drill-page grid.
//!
//! # Obstacle visit order
//!
//! `complete_shape` sorts its overlapping leaves (`ShapeSearchTree.java:598-602`
//! — *"Sorting with Leaf's natural comparison … provides a stable visit
//! order"*) and `SortedRoomNeighbours.calculate_neighbours` re-sorts by
//! `(object_id, shape_index)` (`:141-147`). Both are determinism fixes that are
//! shipped in 2.3.0 and must be kept. Since rooms and items no longer share an
//! id space here, the total order is: **all items first, by ascending
//! [`ItemId`] then shape index; then all rooms, by ascending [`RoomId`]**. It
//! is deterministic and stable, but it is not numerically Java's, so a
//! room-tiling divergence against the lab jar is expected on boards where a
//! room id happens to fall between two item ids.

use std::collections::BTreeMap;

use fr_board::board::Board;
use fr_board::item::{ItemId, ItemKind};
use fr_board::search_tree::SearchTree;
use fr_datastructures::arena_shape_tree::{ArenaShapeTree, QueryStack};
use fr_geometry::planar::{IntBox, Simplex};
use fr_rules::board_rules::BoardRules;

use crate::ctrl::AutorouteControl;
use crate::geom;
use crate::maze::drills::{DrillArena, DrillPageArray};
use crate::maze::nodes::NodeArena;
use crate::rooms::{RoomArena, RoomId, RoomKind};

/// One obstacle the room machinery has to tile around.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Obstacle {
    /// A board item's tile shape.
    Item(ItemId, u16),
    /// A completed free-space room.
    Room(RoomId),
}

/// The room database of one connection's search.
pub struct AutorouteEngine<'b> {
    /// The board, read only.
    pub board: &'b Board,
    /// The board rules.
    pub rules: &'b BoardRules,
    /// The per-net cost and geometry parameters.
    pub ctrl: AutorouteControl,
    /// The rooms and doors.
    pub rooms: RoomArena,
    /// The per-door-section node state.
    pub nodes: NodeArena,
    /// The drills found so far, keyed per page.
    pub drills: DrillArena,
    /// The drill-page grid.
    pub pages: DrillPageArray,

    /// Which compensated tree the queries run against.
    tree_class: i32,
    /// The engine-local room tree (see the module docs).
    room_tree: ArenaShapeTree<RoomId>,
    /// Reusable DFS stack for the item tree.
    item_stack: QueryStack,
    /// Reusable DFS stack for the room tree.
    room_stack: QueryStack,
    /// Cache of obstacle rooms per `(item, tile index)`
    /// (`ItemAutorouteInfo.expansion_room_arr`, `:50-63`). A `BTreeMap` and not
    /// a hash map because it is also iterated when the engine is cleared, and
    /// ARCHITECTURE forbids default-hasher iteration.
    obstacle_rooms: BTreeMap<(ItemId, u16), RoomId>,
    /// The board bounding box, `complete_shape`'s start shape.
    bounds: IntBox,
    /// Scratch buffer for tree queries.
    scratch_entries: Vec<fr_board::search_tree::TreeEntry>,
    /// Scratch buffer for the room-tree overlap list.
    scratch_rooms: Vec<(fr_datastructures::arena_shape_tree::ArenaLeafId, RoomId)>,
    /// Guard against unbounded recursion in `restrain_shape`.
    restrain_depth: u32,
}

/// The recursion cap on `restrain_shape`'s split branch.
///
/// Upstream recurses without a bound (`ShapeSearchTree.java:770`); the
/// termination argument is that each split strictly shrinks the room, which
/// holds for exact arithmetic. The cap is a safety valve, not a behaviour: it
/// is high enough that no real board reaches it.
const MAX_RESTRAIN_DEPTH: u32 = 64;

impl<'b> AutorouteEngine<'b> {
    /// Creates the engine for one net.
    ///
    /// The caller must have called
    /// [`Board::ensure_compensated_tree`](fr_board::board::Board::ensure_compensated_tree)
    /// for `ctrl.trace_clearance_class_no` first — upstream's
    /// `search_tree_manager.get_autoroute_tree(class)`
    /// (`AutorouteEngine.java:70`) creates the tree on demand, which needs
    /// `&mut Board` and therefore cannot happen here. Without it the engine
    /// falls back to the uncompensated default tree and every clearance is the
    /// caller's problem.
    pub fn new(board: &'b Board, rules: &'b BoardRules, ctrl: AutorouteControl) -> Self {
        let bounds = board_bounds(board);
        // `AutorouteEngine.java:87-89`: the *maximum* page width; the grid
        // derives the actual page size from it (conformance appendix D15).
        let max_page_width = ((5.0 * rules.get_default_via_diameter()) as i32).max(10_000);
        let pages = DrillPageArray::new(&bounds, max_page_width, ctrl.layer_count);
        let mut rooms = RoomArena::new();
        // Pages own the object ids below their count; doors and drills start
        // above them, so no two expandable objects share a queue key.
        rooms.set_oid_base(pages.oid_ceiling());
        AutorouteEngine {
            board,
            rules,
            tree_class: ctrl.trace_clearance_class_no,
            ctrl,
            rooms,
            nodes: NodeArena::new(),
            drills: DrillArena::new(),
            pages,
            room_tree: ArenaShapeTree::new(),
            item_stack: QueryStack::new(),
            room_stack: QueryStack::new(),
            obstacle_rooms: BTreeMap::new(),
            bounds,
            scratch_entries: Vec::new(),
            scratch_rooms: Vec::new(),
            restrain_depth: 0,
        }
    }

    /// The board bounding box (`complete_shape`'s `start_shape`).
    #[inline]
    pub fn bounds(&self) -> IntBox {
        self.bounds
    }

    /// The compensated tree every obstacle query runs against.
    #[inline]
    pub fn tree(&self) -> &SearchTree {
        self.board
            .trees()
            .compensated_tree(self.tree_class)
            .unwrap_or_else(|| self.board.default_tree())
    }

    /// `AutorouteEngine.clear()` (`:280-291`): drop every room, every door and
    /// every cached obstacle room, and reset the id counters.
    pub fn clear(&mut self) {
        self.rooms.clear();
        self.room_tree.clear();
        self.obstacle_rooms.clear();
        self.drills.clear();
        self.pages.reset_all();
        self.nodes.reset();
    }

    /// `AutorouteEngine.reset_all_doors()` (`:634-649`) in O(1): the node
    /// arena's generation bump. Dead on the shipped hot path (conformance
    /// appendix D3), kept because it is what makes cross-connection room reuse
    /// possible at all.
    pub fn reset_all_doors(&mut self) {
        self.nodes.reset();
        self.pages.reset_all();
    }

    /// The obstacle room for one tile shape of a routable item, created on
    /// first use and cached (`ItemAutorouteInfo.get_expansion_room`, `:50-63`).
    ///
    /// `None` when the item has no such tile shape in the compensated tree.
    pub fn obstacle_room(&mut self, item: ItemId, index: u16) -> Option<RoomId> {
        if let Some(room) = self.obstacle_rooms.get(&(item, index)) {
            return Some(*room);
        }
        let slot = self.board.store().slot_of(item)?;
        let tile = self.tree().item_shapes(slot, item).get(index as usize)?;
        let shape = geom::tile_to_simplex(tile.shape.as_ref()?);
        let layer = tile.layer;
        let room = self.rooms.add_obstacle_room(item, index, shape, layer);
        self.obstacle_rooms.insert((item, index), room);
        Some(room)
    }

    /// Number of tile shapes the compensated tree holds for `item`.
    pub fn tree_shape_count(&self, item: ItemId) -> usize {
        match self.board.store().slot_of(item) {
            Some(slot) => self.tree().item_shapes(slot, item).len(),
            None => 0,
        }
    }

    /// One tile shape of an item in the compensated tree.
    pub fn item_tile(&self, item: ItemId, index: u16) -> Option<Simplex> {
        let slot = self.board.store().slot_of(item)?;
        self.tree()
            .item_shapes(slot, item)
            .get(index as usize)?
            .shape
            .as_ref()
            .map(geom::tile_to_simplex)
    }

    /// The layer of one tile shape of an item.
    pub fn item_tile_layer(&self, item: ItemId, index: u16) -> Option<i32> {
        let slot = self.board.store().slot_of(item)?;
        Some(
            self.tree()
                .item_shapes(slot, item)
                .get(index as usize)?
                .layer,
        )
    }

    /// `Connectable.get_trace_connection_shape(tree, index)`.
    ///
    /// **Not** the item's tree shape. A `DrillItem` — pin or via — answers
    /// `TileShape.get_instance(get_center())`, a *point* at its centre
    /// (`board/DrillItem.java:349-352`), and a `PolylineTrace` the bare segment
    /// `p_index + 1` (`board/PolylineTrace.java:918-927`); only a
    /// `ConductionArea` answers its tree shape
    /// (`board/ConductionArea.java:373-379`).
    ///
    /// The distinction is load-bearing twice over. It is the predicate
    /// `SortedRoomNeighbours.calculate_target_doors` tests the room against
    /// (`:111-116`), so a target door on a pin exists only for the room that
    /// contains the pin *centre*; and it is the shape
    /// `LocateFoundConnectionAlgo.calculate_starting_point` intersects with
    /// that room (`:185-189`), so answering with the compensated pad instead
    /// produces doors whose landing shape is empty and a start point at the
    /// coordinate origin.
    pub fn trace_connection_shape(&self, item: ItemId, index: u16) -> Option<Simplex> {
        let board_item = self.board.store().get(item)?;
        match &board_item.kind {
            ItemKind::Pin(pin) => Some(pin.center.surrounding_box().to_simplex()),
            ItemKind::Via(via) => Some(via.center.surrounding_box().to_simplex()),
            ItemKind::Trace(trace) => {
                fr_geometry::planar::LineSegment::from_polyline(
                    &trace.polyline,
                    index as usize + 1,
                )
                .map(|segment| segment.to_simplex())
            }
            _ => self.item_tile(item, index),
        }
    }

    /// Every obstacle overlapping `query` on `layer`, in the module docs' total
    /// order.
    ///
    /// `ignore_net` follows upstream's `is_trace_obstacle(net_no)`: an item
    /// that is not an obstacle to the routed net is skipped, and reported
    /// separately through `own_net` so the caller can defer it into the
    /// target-door candidate list.
    pub fn overlapping_obstacles(
        &mut self,
        query: &Simplex,
        layer: Option<i32>,
        net_no: i32,
        obstacles: &mut Vec<Obstacle>,
        own_net: &mut Vec<(ItemId, u16)>,
    ) {
        obstacles.clear();
        own_net.clear();
        let query_tile = fr_board::shape::TileShape::Simplex(query.clone());
        fr_metrics::counters::bump::tree_queries(1);

        let tree = self
            .board
            .trees()
            .compensated_tree(self.tree_class)
            .unwrap_or_else(|| self.board.default_tree());
        tree.overlapping_tree_entries(
            self.board.store(),
            &query_tile,
            layer,
            &[],
            &mut self.item_stack,
            &mut self.scratch_entries,
        );
        for entry in &self.scratch_entries {
            let Some(item) = self.board.store().get(entry.item) else {
                continue;
            };
            if item.is_trace_obstacle(net_no) {
                obstacles.push(Obstacle::Item(entry.item, entry.shape_index));
            } else {
                own_net.push((entry.item, entry.shape_index));
            }
        }
        // Ascending item id then shape index — `SortedRoomNeighbours.java:141-147`.
        obstacles.sort_unstable();
        own_net.sort_unstable();

        // Rooms come after every item; see the module docs.
        self.scratch_rooms.clear();
        self.room_tree.overlaps_with_stack(
            &query_tile.bounds(),
            &mut self.room_stack,
            &mut self.scratch_rooms,
        );
        let mut room_hits: Vec<RoomId> = self
            .scratch_rooms
            .iter()
            .map(|(_, room)| *room)
            .filter(|room| {
                let room = self.rooms.room(*room);
                room.alive
                    && room.kind == RoomKind::CompleteFree
                    && layer.map(|l| l == room.layer).unwrap_or(true)
                    && room.shape.intersects(query)
            })
            .collect();
        room_hits.sort_unstable();
        room_hits.dedup();
        obstacles.extend(room_hits.into_iter().map(Obstacle::Room));
    }

    /// The shape of one obstacle.
    pub fn obstacle_shape(&self, obstacle: Obstacle) -> Option<Simplex> {
        match obstacle {
            Obstacle::Item(item, index) => self.item_tile(item, index),
            Obstacle::Room(room) => {
                let room = self.rooms.room(room);
                room.alive.then(|| room.shape.clone())
            }
        }
    }

    /// The layer of one obstacle.
    pub fn obstacle_layer(&self, obstacle: Obstacle) -> Option<i32> {
        match obstacle {
            Obstacle::Item(item, index) => self.item_tile_layer(item, index),
            Obstacle::Room(room) => Some(self.rooms.room(room).layer),
        }
    }

    /// Inserts a completed room into the engine-local tree.
    pub(crate) fn insert_room_in_tree(&mut self, room_id: RoomId) {
        let bounds =
            fr_board::shape::TileShape::Simplex(self.rooms.room(room_id).shape.clone()).bounds();
        let leaf = self.room_tree.insert(bounds, room_id);
        self.rooms.room_mut(room_id).leaf = Some(leaf);
    }

    /// Removes a completed room from the engine-local tree.
    pub(crate) fn remove_room_from_tree(&mut self, room_id: RoomId) {
        if let Some(leaf) = self.rooms.room_mut(room_id).leaf.take() {
            self.room_tree.remove(leaf);
        }
    }

    pub(crate) fn enter_restrain(&mut self) -> bool {
        if self.restrain_depth >= MAX_RESTRAIN_DEPTH {
            return false;
        }
        self.restrain_depth += 1;
        true
    }

    pub(crate) fn leave_restrain(&mut self) {
        self.restrain_depth = self.restrain_depth.saturating_sub(1);
    }
}

/// The board bounding box: the union of every indexed tile shape, or a
/// degenerate box when the board is empty.
///
/// Upstream reads `RoutingBoard.bounding_box`, which the board keeps as the
/// union of the outline shapes plus a margin. Deriving it from the default
/// tree's root bounds gives the same envelope for every board that has an
/// outline, and a usable one for the synthetic boards the tests build.
fn board_bounds(board: &Board) -> IntBox {
    let mut bounds: Option<IntBox> = None;
    for (id, item) in board.store().iter() {
        if matches!(item.kind, ItemKind::ComponentOutline(_)) {
            continue;
        }
        let Some(slot) = board.store().slot_of(id) else {
            continue;
        };
        for tile in board.default_tree().item_shapes(slot, id) {
            let Some(shape) = tile.shape.as_ref() else {
                continue;
            };
            let bb = shape.bounding_box();
            bounds = Some(match bounds {
                Some(existing) => existing.union(&bb),
                None => bb,
            });
        }
    }
    // `RoutingBoard` pads the outline; a bare union would put the outermost
    // pad exactly on the border and leave the router no room to reach it.
    match bounds {
        Some(bb) => IntBox::new(
            bb.left_x().saturating_sub(BOUNDS_MARGIN),
            bb.bottom_y().saturating_sub(BOUNDS_MARGIN),
            bb.right_x().saturating_add(BOUNDS_MARGIN),
            bb.top_y().saturating_add(BOUNDS_MARGIN),
        ),
        // A board with no indexed geometry at all — only the synthetic
        // fixtures — still needs a 2-dimensional start shape for
        // `complete_shape`, or every room completion returns nothing.
        None => IntBox::new(-BOUNDS_MARGIN, -BOUNDS_MARGIN, BOUNDS_MARGIN, BOUNDS_MARGIN),
    }
}

/// The margin [`board_bounds`] adds around the indexed geometry.
const BOUNDS_MARGIN: i32 = 1000;

#[cfg(test)]
mod tests {
    use crate::test_support::TestFixture;

    #[test]
    fn the_engine_starts_with_no_rooms() {
        let fixture = TestFixture::two_layer_with_pads();
        let engine = fixture.engine();
        assert_eq!(engine.rooms.room_count(), 0);
        assert_eq!(engine.rooms.door_count(), 0);
    }

    #[test]
    fn bounds_enclose_every_pad() {
        let fixture = TestFixture::two_layer_with_pads();
        let engine = fixture.engine();
        let bounds = engine.bounds();
        for pad in fixture.pad_centers() {
            assert!(bounds.left_x() < pad.x && pad.x < bounds.right_x());
            assert!(bounds.bottom_y() < pad.y && pad.y < bounds.top_y());
        }
    }

    #[test]
    fn pages_doors_and_drills_have_disjoint_object_ids() {
        let fixture = TestFixture::two_layer_with_pads();
        let mut engine = fixture.engine();
        let page_ceiling = engine.pages.oid_ceiling();
        let a = engine
            .rooms
            .add_complete_free_room(engine.bounds().to_simplex(), 0);
        let b = engine
            .rooms
            .add_complete_free_room(engine.bounds().to_simplex(), 1);
        let door = engine.rooms.add_room_door(a, b, 1);
        assert!(
            engine.rooms.door(door).oid >= page_ceiling,
            "a door id must not land in the drill pages' range"
        );
        for index in 0..engine.pages.len() as u32 {
            assert!(engine.pages.page(crate::rooms::PageId(index)).oid < page_ceiling);
        }
    }

    #[test]
    fn obstacle_rooms_are_cached_per_item_tile() {
        let fixture = TestFixture::two_layer_with_pads();
        let mut engine = fixture.engine();
        let item = fixture.pad_ids()[0];
        let first = engine.obstacle_room(item, 0).expect("pad has a tile shape");
        let second = engine.obstacle_room(item, 0).expect("cached");
        assert_eq!(first, second, "identity must be stable across the search");
    }
}
