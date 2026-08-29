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
//! Upstream's two consumers of an obstacle list sort it **differently**, and
//! both sorts are determinism fixes shipped in 2.3.0:
//!
//! * `SortedRoomNeighbours.calculate_neighbours` (`:139-147`) sorts on one
//!   ascending `(get_id_no(), shape_index)` key over items *and* rooms
//!   together. That is
//!   [`sort_obstacles_for_neighbours`](AutorouteEngine::sort_obstacles_for_neighbours),
//!   which every [`overlapping_obstacles`](AutorouteEngine::overlapping_obstacles)
//!   result comes back in.
//! * `ShapeSearchTree.complete_shape` (`:598-602` — *"Sorting with Leaf's
//!   natural comparison … provides a stable visit order"*) sorts with
//!   `ShapeTree.Leaf.compareTo`, and the `compareTo` implementations it
//!   delegates to subtract *this* id from the other's, so that order is
//!   **rooms before items, each class by descending id**. That is
//!   [`sort_obstacles_for_completion`](AutorouteEngine::sort_obstacles_for_completion),
//!   which `complete_shape` applies on top.
//!
//! The two orders live in one id space upstream because a room is a
//! `SearchTreeObject` there. They live in one id space here too:
//! [`Room::id_no`](crate::rooms::Room::id_no) is the same
//! `generate_room_id_no` counter, so the numeric interleave is reproducible
//! even though the rooms sit in a separate tree.
//!
//! Getting the completion order wrong is not a tie-break detail.
//! `restrain_shape` is greedy — it cuts with the furthest border line of
//! whichever obstacle it is handed *now* — so the sequence decides the tiling.
//! Visiting the items first re-cuts free space that the already-completed
//! neighbour rooms had settled, because the `p_ignore_shape` short circuit at
//! `ShapeSearchTree.java:625-628` can only recognise the room we came from
//! while the candidate is still whole. Once an item has split the candidate,
//! the neighbour's overlap with each piece no longer sits inside the from-door
//! shape, the room is carved against itself, and the corridor fills with
//! sliver tiles and the doors that come with them.

use std::collections::BTreeMap;

use fr_board::board::Board;
use fr_board::item::{ItemId, ItemKind};
use fr_board::search_tree::{SearchTree, Tiling45Mode, TreeAngleFamily};
use fr_board::shape::AngleRestriction;
use fr_datastructures::arena_shape_tree::{ArenaShapeTree, QueryStack};
use fr_geometry::planar::{IntBox, IntOctagon, Simplex};
use fr_rules::board_rules::BoardRules;

use crate::batch::budget::ConnectionClock;
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
    /// How much of the 45° tiling chain runs — the flavor selector.
    tiling_45: Tiling45Mode,
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
    /// This connection's allowance, when the caller armed one.
    ///
    /// Upstream's `AutorouteEngine.time_limit` (`:63`), set by
    /// `init_connection` (`:119`) from the `TimeLimit` the batch pass built
    /// (`BatchAutorouter.java:1311-1318`) and read back by `is_stop_requested`
    /// (`:266-270`), which is what the maze loop polls. Borrowed rather than
    /// owned because the pass owns the clock: it arms it around the *whole*
    /// attempt — search, insert and the per-connection pull-tight — and reads
    /// the usage back out after the engine is gone.
    clock: Option<&'b mut ConnectionClock>,
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
            tiling_45: default_tiling_45(board),
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
            clock: None,
        }
    }

    /// Arms this connection's allowance — `AutorouteEngine.init_connection`'s
    /// `p_time_limit` (`:93`, `:119`).
    ///
    /// Without one the search is unbounded, which is upstream's `null`
    /// `TimeLimit`: every direct caller (the fanout stage, the tests) routes
    /// that way, and only the batch pass has a budget to spend.
    #[inline]
    pub fn set_clock(&mut self, clock: &'b mut ConnectionClock) {
        self.clock = Some(clock);
    }

    /// Charges `units` of effort against this connection's allowance, or does
    /// nothing when none is armed.
    ///
    /// The maze loop charges one per settled element, which is what makes
    /// [`BudgetMode::Deterministic`](crate::batch::budget::BudgetMode::Deterministic)
    /// spend virtual milliseconds the machine cannot influence.
    #[inline]
    pub fn charge(&mut self, units: u64) {
        if let Some(clock) = self.clock.as_deref_mut() {
            clock.charge(units);
        }
    }

    /// `AutorouteEngine.is_stop_requested()` (`:266-270`): whether this
    /// connection's allowance is gone.
    ///
    /// `false` with no clock armed — upstream's `time_limit == null` plus a
    /// `stoppable_thread` this port does not have (headless 2.3.0 never
    /// interrupts a connection from outside; conformance #107).
    #[inline]
    pub fn is_stop_requested(&self) -> bool {
        self.clock
            .as_deref()
            .is_some_and(ConnectionClock::is_exhausted)
    }

    /// The board bounding box (`complete_shape`'s `start_shape`).
    #[inline]
    pub fn bounds(&self) -> IntBox {
        self.bounds
    }

    /// The compensated tree every obstacle query runs against.
    ///
    /// `AutorouteEngine.java:70` reads one tree, and which *subclass* it gets is
    /// `SearchTreeManager`'s decision from the board's angle restriction. Here
    /// that is a lookup on both axes, with two fallbacks that keep the 45°
    /// chain usable before its tree exists: the any-angle compensated tree, then
    /// the uncompensated default one. Falling back is not a correctness hole —
    /// the room algorithms narrow every shape to its bounding octagon anyway,
    /// which is exactly what upstream's 45° tree does at each read
    /// (`ShapeSearchTree45Degree.java:147-149`) — it only costs the narrowing.
    #[inline]
    pub fn tree(&self) -> &SearchTree {
        let trees = self.board.trees();
        trees
            .compensated_tree_with_family(self.tree_class, self.angle_family())
            .or_else(|| trees.compensated_tree(self.tree_class))
            .unwrap_or_else(|| self.board.default_tree())
    }

    /// How much of the 45° chain this engine runs.
    #[inline]
    pub fn tiling_45(&self) -> Tiling45Mode {
        self.tiling_45
    }

    /// Which tree subclass the queries read — `ShapeSearchTree45Degree` as soon
    /// as any part of the chain is on, because both halves narrow every shape to
    /// its bounding octagon and reading octagons back is what makes that free.
    #[inline]
    pub fn angle_family(&self) -> TreeAngleFamily {
        if self.tiling_45.is_on() {
            TreeAngleFamily::FortyFive
        } else {
            TreeAngleFamily::AnyAngle
        }
    }

    /// Overrides the mode — the A/B switch's programmatic form, and how the
    /// tests pin a mode regardless of the environment.
    ///
    /// Must be called before any room is built: the modes produce different
    /// tilings and a half-and-half arena is meaningless.
    #[inline]
    pub fn set_tiling_45(&mut self, mode: Tiling45Mode) {
        debug_assert_eq!(
            self.rooms.room_count(),
            0,
            "the tiling mode must be fixed before the first room is built"
        );
        self.tiling_45 = mode;
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
        let query_tile = fr_board::shape::TileShape::Simplex(query.clone());
        self.overlapping_obstacles_tile(&query_tile, layer, net_no, obstacles, own_net);
    }

    /// [`overlapping_obstacles`](Self::overlapping_obstacles) with the query in
    /// its *natural* tile flavor.
    ///
    /// A caller whose query is an axis-parallel box must use this and pass
    /// `TileShape::Box`, not a `Simplex` built from the box: the tree's
    /// per-leaf filter is `TileShape::intersects`, and the simplex arm of that
    /// predicate runs a full exact `Simplex::intersection` — sort, then
    /// `remove_redundant_lines`, which is `O(edges²)` in `side_of_intersection`
    /// — where the box arm is four integer comparisons. On Issue034 that one
    /// dispatch was 52% of the router's run time (`/usr/bin/sample`, 45195
    /// samples: `TileShape::intersects` 23412 inclusive).
    pub fn overlapping_obstacles_tile(
        &mut self,
        query_tile: &fr_board::shape::TileShape,
        layer: Option<i32>,
        net_no: i32,
        obstacles: &mut Vec<Obstacle>,
        own_net: &mut Vec<(ItemId, u16)>,
    ) {
        obstacles.clear();
        own_net.clear();
        fr_metrics::counters::bump::tree_queries(1);

        let tree = self
            .board
            .trees()
            .compensated_tree(self.tree_class)
            .unwrap_or_else(|| self.board.default_tree());
        tree.overlapping_tree_entries(
            self.board.store(),
            query_tile,
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
        own_net.sort_unstable();

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
                    && match query_tile {
                        // Each arm is `room.shape.intersects(<query as a
                        // simplex>)`, so the room set is the same whichever
                        // flavor the caller handed in.
                        fr_board::shape::TileShape::Simplex(s) => room.shape.intersects(s),
                        fr_board::shape::TileShape::Box(b) => room.shape.intersects_box(b),
                        fr_board::shape::TileShape::Oct(o) => room.shape.intersects_octagon(o),
                    }
            })
            .collect();
        room_hits.sort_unstable();
        room_hits.dedup();
        obstacles.extend(room_hits.into_iter().map(Obstacle::Room));
        // One ascending `get_id_no()` key over items *and* rooms together —
        // `SortedRoomNeighbours.java:139-147`. See
        // [`sort_obstacles_for_neighbours`](Self::sort_obstacles_for_neighbours)
        // for why the two kinds interleave instead of items-then-rooms, and
        // `complete_shape` for the one caller that needs the *other* upstream
        // order and re-sorts.
        self.sort_obstacles_for_neighbours(obstacles);
    }

    /// Reorders an obstacle list into the visit order
    /// `ShapeSearchTree.complete_shape` uses (`ShapeSearchTree.java:576-601`).
    ///
    /// Upstream collects the overlapping leaves with one tree walk and then
    /// `Collections.sort`s them with `ShapeTree.Leaf.compareTo`
    /// (`datastructures/ShapeTree.java:234-240`), which delegates to the stored
    /// object's `compareTo`. Both implementations subtract *this* id from the
    /// other's — `Item.compareTo` is `item.id_no - id_no` (`board/Item.java:94-101`)
    /// and `CompleteFreeSpaceExpansionRoom.compareTo` is `other.id_no - this.id_no`
    /// (`autoroute/CompleteFreeSpaceExpansionRoom.java:48-56`) — so each class
    /// sorts by **descending** id. The cross-class arms disagree in sign on
    /// purpose (an item returns `1` against a non-item, a room returns `-1`
    /// against a non-room), which puts every room *before* every item. Ties
    /// within one item break on ascending shape index.
    ///
    /// This is deliberately **not** the order
    /// [`overlapping_obstacles_tile`](Self::overlapping_obstacles_tile) hands
    /// back: that one is `SortedRoomNeighbours`' ascending-id sort
    /// (`SortedRoomNeighbours.java:139-147`), and the two upstream call sites
    /// really do sort differently. The distinction is not cosmetic —
    /// [`restrain_shape`](Self::complete_shape) picks its cut line greedily
    /// from whichever obstacle it sees first, so the sequence decides the
    /// tiling: carving with the items before the rooms re-cuts free space the
    /// already-completed neighbour rooms had settled, which strands sliver
    /// tiles and multiplies the door count along a corridor.
    pub(crate) fn sort_obstacles_for_completion(&self, obstacles: &mut [Obstacle]) {
        obstacles.sort_unstable_by_key(|obstacle| match *obstacle {
            Obstacle::Room(room) => (0u8, -self.rooms.room(room).id_no, 0u16),
            Obstacle::Item(item, index) => (1u8, -(i64::from(item.raw())), index),
        });
    }

    /// Reorders an obstacle list into the visit order
    /// `SortedRoomNeighbours.calculate_neighbours` uses
    /// (`SortedRoomNeighbours.java:139-147`).
    ///
    /// Upstream sorts the overlapping tree entries on a *single* ascending
    /// `SearchTreeObject.get_id_no()` key, ties broken on ascending shape
    /// index. Items and complete free rooms share that key space — an item's
    /// key is its board id (`board/Item.java:107`), a room's is the
    /// per-connection counter `generate_room_id_no` handed it
    /// (`CompleteFreeSpaceExpansionRoom.java:105`) — so the two kinds
    /// *interleave*. Sorting items ahead of rooms, which is what
    /// [`overlapping_obstacles_tile`](Self::overlapping_obstacles_tile) hands
    /// back, changes which neighbour wins the counter-clockwise sort and
    /// therefore which doors the room ends up with.
    ///
    /// Obstacle rooms never reach here from the tree — `ObstacleExpansionRoom`
    /// is not a `SearchTreeObject` (`autoroute/ObstacleExpansionRoom.java:19`)
    /// — so their `(item << 10) | index` ids play no part in this order.
    pub(crate) fn sort_obstacles_for_neighbours(&self, obstacles: &mut [Obstacle]) {
        obstacles.sort_unstable_by_key(|obstacle| match *obstacle {
            // The room arm sorts first on an exact id tie: upstream's sort is
            // stable over a tree walk we do not reproduce, so the tie needs
            // *some* rule, and the rooms are the objects this connection just
            // built — visiting them before the board items keeps a freshly
            // completed neighbour from being re-cut by an item of the same id.
            Obstacle::Room(room) => (self.rooms.room(room).id_no, 0u8, 0u16),
            Obstacle::Item(item, index) => (i64::from(item.raw()), 1u8, index),
        });
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

    /// One tile shape of an item in its **octagon** flavor.
    ///
    /// Upstream's `curr_object.get_tree_shape(tree, i).bounding_octagon()`
    /// (`ShapeSearchTree45Degree.java:147-149`,
    /// `Sorted45DegreeRoomNeighbours.java:113-114`). Free when the tree already
    /// stores octagons (the [`TreeAngleFamily::FortyFive`] tree), a narrowing
    /// otherwise — which is what upstream pays on every read.
    pub fn item_tile_octagon(&self, item: ItemId, index: u16) -> Option<IntOctagon> {
        let slot = self.board.store().slot_of(item)?;
        let tile = self
            .tree()
            .item_shapes(slot, item)
            .get(index as usize)?
            .shape
            .as_ref()?;
        tile_octagon(tile)
    }

    /// [`obstacle_shape`](Self::obstacle_shape) in the octagon flavor.
    pub fn obstacle_octagon(&self, obstacle: Obstacle) -> Option<IntOctagon> {
        match obstacle {
            Obstacle::Item(item, index) => self.item_tile_octagon(item, index),
            Obstacle::Room(room) => {
                let room = self.rooms.room(room);
                room.alive.then(|| room.octagon())
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

/// The tiling mode an engine starts in.
///
/// Upstream has no choice here — `SearchTreeManager.java:166` reads the board's
/// angle restriction and that is that. The chain ships **off** because the flip
/// changes the tiling on every default board, so the gate can A/B it: it turns
/// on for a 45° board only when [`TILING_45_ENV`](fr_board::search_tree::TILING_45_ENV)
/// says so, and [`AutorouteEngine::set_tiling_45`] overrides either way.
pub fn default_tiling_45(board: &Board) -> Tiling45Mode {
    if board.angle_restriction() == AngleRestriction::FortyFive {
        fr_board::search_tree::tiling_45_mode()
    } else {
        Tiling45Mode::Off
    }
}

/// A tile shape as an octagon, `None` for one that has no bounding octagon.
fn tile_octagon(tile: &fr_board::shape::TileShape) -> Option<IntOctagon> {
    match tile {
        fr_board::shape::TileShape::Box(b) => Some(b.to_int_octagon().normalize()),
        fr_board::shape::TileShape::Oct(o) => Some(o.normalize()),
        fr_board::shape::TileShape::Simplex(s) => crate::rooms::complete45::simplex_octagon(s),
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
    use super::{Obstacle, RoomId};
    use crate::test_support::TestFixture;
    use fr_board::item::ItemId;
    use fr_geometry::planar::IntBox;

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

    /// Three rooms with `id_no` 0, 1, 2 on a fixture whose items are ids 1..n,
    /// so both comparators have something to interleave.
    fn ordering_fixture(engine: &mut crate::engine::AutorouteEngine<'_>) -> Vec<Obstacle> {
        let square = IntBox::new(0, 0, 10, 10).to_simplex();
        let rooms: Vec<RoomId> = (0..3)
            .map(|_| engine.rooms.add_complete_free_room(square.clone(), 0))
            .collect();
        assert_eq!(
            rooms
                .iter()
                .map(|r| engine.rooms.room(*r).id_no)
                .collect::<Vec<_>>(),
            vec![0, 1, 2],
            "the room counter is `generate_room_id_no`, so it starts at the bottom"
        );
        vec![
            Obstacle::Item(ItemId::from_raw(2), 1),
            Obstacle::Room(rooms[1]),
            Obstacle::Item(ItemId::from_raw(1), 0),
            Obstacle::Room(rooms[2]),
            Obstacle::Item(ItemId::from_raw(2), 0),
            Obstacle::Room(rooms[0]),
        ]
    }

    #[test]
    fn the_completion_order_is_rooms_first_by_descending_id() {
        // `ShapeSearchTree.java:598` sorts with `ShapeTree.Leaf.compareTo`,
        // which delegates to `Item.compareTo` / `CompleteFreeSpaceExpansionRoom
        // .compareTo` — both `other.id - this.id`, and both disagreeing in sign
        // on the cross-class arm.
        let fixture = TestFixture::two_layer_with_pads();
        let mut engine = fixture.engine();
        let mut obstacles = ordering_fixture(&mut engine);
        engine.sort_obstacles_for_completion(&mut obstacles);
        let kinds: Vec<(u8, i64, u16)> = obstacles
            .iter()
            .map(|obstacle| match *obstacle {
                Obstacle::Room(room) => (0, engine.rooms.room(room).id_no, 0),
                Obstacle::Item(item, index) => (1, i64::from(item.raw()), index),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                (0, 2, 0),
                (0, 1, 0),
                (0, 0, 0),
                (1, 2, 0),
                (1, 2, 1),
                (1, 1, 0),
            ],
            "rooms before items, each class descending, shape index ascending"
        );
    }

    #[test]
    fn the_neighbour_order_interleaves_rooms_and_items_by_ascending_id() {
        // `SortedRoomNeighbours.java:141-147` — one `get_id_no()` key over the
        // whole list, so a room whose counter value falls between two item ids
        // is visited between them.
        let fixture = TestFixture::two_layer_with_pads();
        let mut engine = fixture.engine();
        let mut obstacles = ordering_fixture(&mut engine);
        engine.sort_obstacles_for_neighbours(&mut obstacles);
        let keys: Vec<(i64, u8, u16)> = obstacles
            .iter()
            .map(|obstacle| match *obstacle {
                Obstacle::Room(room) => (engine.rooms.room(room).id_no, 0, 0),
                Obstacle::Item(item, index) => (i64::from(item.raw()), 1, index),
            })
            .collect();
        assert_eq!(
            keys,
            vec![
                (0, 0, 0),
                (1, 0, 0),
                (1, 1, 0),
                (2, 0, 0),
                (2, 1, 0),
                (2, 1, 1),
            ],
            "one ascending id key, rooms winning an exact tie"
        );
    }

    #[test]
    fn a_tree_query_hands_back_the_neighbour_order() {
        // The query is the only producer of obstacle lists, so the order it
        // establishes is the one `SortedRoomNeighbours` needs; `complete_shape`
        // is the single caller that re-sorts.
        let fixture = TestFixture::with_foreign_trace();
        let mut engine = fixture.engine();
        let square = IntBox::new(-100, -100, 100, 100).to_simplex();
        let room = engine.rooms.add_complete_free_room(square, 0);
        engine.insert_room_in_tree(room);
        let mut obstacles = Vec::new();
        let mut own_net = Vec::new();
        let query = engine.bounds().to_simplex();
        engine.overlapping_obstacles(
            &query,
            Some(0),
            crate::test_support::TEST_NET,
            &mut obstacles,
            &mut own_net,
        );
        let mut expected = obstacles.clone();
        engine.sort_obstacles_for_neighbours(&mut expected);
        assert_eq!(obstacles, expected, "the query already sorted the list");
        assert!(
            obstacles.contains(&Obstacle::Room(room)),
            "the room tree is part of the query"
        );
    }
}
