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
//! `generate_room_id_no` counter, minted at the same call sites (see
//! [`calculate_doors`](AutorouteEngine::calculate_doors)), so the numeric
//! interleave is reproducible even though the rooms sit in a separate tree.
//!
//! In practice the two halves of that space barely overlap, and the *sign* of
//! the overlap is what matters. A room counter restarts at 1 for every
//! connection while board item ids keep climbing for the whole run — every
//! inserted trace mints one — so after the first few connections nearly every
//! room sorts ahead of nearly every item. Making the shared space *exact* is
//! therefore almost free: reproducing upstream's `++count` (1-based, plus an
//! id burned per `ObstacleExpansionRoom` and per `try_remove_edge` restart)
//! leaves every counter on Issue026 and Issue034 bit-for-bit unchanged.
//! Reversing it is not: offsetting the counter so that rooms sort *behind*
//! items costs 3.2x expansions on Issue026 and 11.4x on Issue034.
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
use fr_board::store::SlotIndex;
use fr_datastructures::arena_shape_tree::{ArenaShapeTree, QueryStack};
use fr_geometry::planar::{IntBox, IntOctagon, Simplex};
use fr_rules::board_rules::BoardRules;

use crate::batch::budget::ConnectionClock;
use crate::ctrl::AutorouteControl;
use crate::geom;
use crate::maze::drills::{DrillArena, DrillPageArray};
use crate::maze::nodes::NodeArena;
use crate::rooms::{RoomArena, RoomId, RoomKind};

/// `Vec::dedup` on a sorted slice, returning how many elements were kept.
///
/// The caller truncates. This exists because the duplicates sit in the *tail*
/// of a longer `Vec` that must not be reordered, and `Vec::dedup` only works on
/// the whole vector.
fn dedup_in_place<T: PartialEq + Copy>(items: &mut [T]) -> usize {
    let mut kept = 0;
    for read in 0..items.len() {
        if kept == 0 || items[read] != items[kept - 1] {
            items[kept] = items[read];
            kept += 1;
        }
    }
    kept
}

/// One obstacle the room machinery has to tile around.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Obstacle {
    /// A board item's tile shape, plus the store slot the tree walk already
    /// resolved so the carve loop can read the tile without `slot_of`.
    Item(ItemId, u16, SlotIndex),
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
    /// The engine-local room trees, one per layer (see the module docs). Only
    /// complete free rooms are indexed, and a room never changes layer, so a
    /// layered query walks one tree instead of filtering the whole set
    /// (Issue039: the shared tree handed ~330 leaves per box query, half of
    /// them on the wrong layer).
    room_trees: Vec<ArenaShapeTree<RoomId>>,
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
    /// Per-layer item shapes of the compensated tree, built once per
    /// connection. The maze does not mutate the board, so this is valid for
    /// every `complete_shape` / neighbour query of the search. A box query
    /// then filters by `IntBox::intersects` instead of walking the item tree
    /// (Issue039: ~888 nodes per board-sized query, millions of queries).
    item_cache: Option<ItemCache>,
    /// Bounding boxes parallel to the last box-path collect, so the carve
    /// loop can reject on `IntBox::intersects` without `slot_of`.
    pub(crate) scratch_hit_bounds: Vec<IntBox>,
    /// Grid-query hit indices, sorted ascending to emit the presorted cache
    /// in completion order.
    scratch_grid_hits: Vec<u32>,
    /// Checked-out-between-queries collect buffers. `Option` because a nested
    /// collect (try_remove_edge → complete_shape inside calculate_doors while
    /// the caller still holds the own-net list) must fall back to allocating
    /// instead of aliasing the live buffer.
    scratch_collect_obstacles: Option<Vec<Obstacle>>,
    /// The own-net half of the pool.
    scratch_collect_own_net: Option<Vec<(ItemId, u16)>>,
    /// Per-connection memo of `tile_to_simplex` for every compensated-tree
    /// tile, flat over slots: `tile_simplex_base[slot] + shape_index`. The
    /// carve loop rebuilds these per bounds-surviving obstacle otherwise
    /// (tens of millions of times on Issue039); the board is immutable for
    /// the whole attempt, so the memo cannot go stale.
    tile_simplex_base: Vec<u32>,
    /// The memo cells for [`AutorouteEngine::tile_simplex_base`].
    tile_simplex_cache: Vec<std::cell::OnceCell<std::rc::Rc<Simplex>>>,
    /// Flat per-tile SAT vertex & verification memo, in lockstep with
    /// `tile_simplex_cache`.
    tile_sat_cache: Vec<
        std::cell::OnceCell<Option<std::rc::Rc<(fr_geometry::planar::SatCorners, Option<bool>)>>>,
    >,
    /// Same flat layout for the octagon flavor — the 45° carve octagonizes
    /// every collected obstacle, and `tile_octagon` of a Simplex tile is f64
    /// corner math per call.
    tile_octagon_cache: Vec<std::cell::OnceCell<Option<IntOctagon>>>,
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

/// One compensated-tree tile, ready for a box-filtered collect.
#[derive(Clone, Copy)]
struct CachedItem {
    item: ItemId,
    index: u16,
    slot: SlotIndex,
    bounds: IntBox,
    /// `is_trace_obstacle(net_no)` for the connection's net, resolved once at
    /// build time. Every box-query caller passes `ctrl.net_no`; a query for
    /// any other net recomputes instead of reading this.
    obstacle: bool,
}

/// The per-connection item cache: one [`LayerItems`] per layer, plus the
/// generation stamps the grid query uses to emit each item once.
struct ItemCache {
    layers: Vec<LayerItems>,
    /// The net the `obstacle` flags were resolved for.
    net_no: i32,
    /// One stamp slot per item of the largest layer; a slot holding the
    /// current `gen` means the item already went into the result.
    marks: Vec<u32>,
    gen: u32,
}

/// One layer's cached tiles, with a uniform grid over their bounding boxes
/// once the layer is big enough for a linear scan to hurt.
struct LayerItems {
    items: Vec<CachedItem>,
    grid: Option<LayerGrid>,
}

/// Uniform grid over a layer's item bounding boxes, in CSR form: cell `c`'s
/// items are `indices[offsets[c]..offsets[c + 1]]`, each an index into
/// [`LayerItems::items`]. An item is listed in every cell its bounds touch,
/// so a query that reads its overlapped cells cannot miss a hit; duplicates
/// are filtered by the [`ItemCache::marks`] stamps.
///
/// The grid only ever *narrows* the scan — the per-item `IntBox::intersects`
/// test still decides membership, so the collected set is exactly the linear
/// scan's, in a different (but order-insignificant) sequence: both ordered
/// entry points re-sort.
struct LayerGrid {
    min_x: i32,
    min_y: i32,
    cell_w: i64,
    cell_h: i64,
    cols: usize,
    rows: usize,
    offsets: Vec<u32>,
    indices: Vec<u32>,
}

/// Layers smaller than this scan linearly; grid bookkeeping costs more than
/// the handful of `intersects` tests it saves.
const GRID_MIN_ITEMS: usize = 64;

impl LayerGrid {
    fn build(items: &[CachedItem], board: IntBox) -> Option<Self> {
        let width = i64::from(board.ur.x) - i64::from(board.ll.x);
        let height = i64::from(board.ur.y) - i64::from(board.ll.y);
        if width <= 0 || height <= 0 {
            return None;
        }
        // Roughly one cell per item, scaled to the board aspect.
        let n = items.len() as f64;
        let cols = ((n * width as f64 / height as f64).sqrt().ceil() as usize).clamp(1, 256);
        let rows = (n as usize / cols).clamp(1, 256);
        let cell_w = (width / cols as i64).max(1);
        let cell_h = (height / rows as i64).max(1);
        let mut grid = LayerGrid {
            min_x: board.ll.x,
            min_y: board.ll.y,
            cell_w,
            cell_h,
            cols,
            rows,
            offsets: vec![0; cols * rows + 1],
            indices: Vec::new(),
        };
        // Counting sort into CSR.
        let mut counts = vec![0u32; cols * rows];
        for cached in items {
            let Some((x0, y0, x1, y1)) = grid.cell_range(&cached.bounds) else {
                continue;
            };
            for y in y0..=y1 {
                for x in x0..=x1 {
                    counts[y * cols + x] += 1;
                }
            }
        }
        let mut total = 0u32;
        for (cell, &count) in counts.iter().enumerate() {
            grid.offsets[cell] = total;
            total += count;
        }
        grid.offsets[cols * rows] = total;
        let mut indices = vec![0u32; total as usize];
        let mut cursor = vec![0u32; cols * rows];
        for (index, cached) in items.iter().enumerate() {
            let Some((x0, y0, x1, y1)) = grid.cell_range(&cached.bounds) else {
                continue;
            };
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let cell = y * cols + x;
                    indices[(grid.offsets[cell] + cursor[cell]) as usize] = index as u32;
                    cursor[cell] += 1;
                }
            }
        }
        grid.indices = indices;
        Some(grid)
    }

    /// The cell range `bounds` overlaps, clamped to the grid. `None` for an
    /// empty box; an out-of-board box clamps to the edge cells, matching the
    /// linear scan (an out-of-board item still intersects an out-of-board
    /// query and stays reachable).
    fn cell_range(&self, bounds: &IntBox) -> Option<(usize, usize, usize, usize)> {
        if bounds.is_empty() {
            return None;
        }
        let cell_of = |v: i32, min: i32, cell: i64, max: usize| -> usize {
            (((i64::from(v) - i64::from(min)) / cell).max(0) as usize).min(max)
        };
        Some((
            cell_of(bounds.ll.x, self.min_x, self.cell_w, self.cols - 1),
            cell_of(bounds.ll.y, self.min_y, self.cell_h, self.rows - 1),
            cell_of(bounds.ur.x, self.min_x, self.cell_w, self.cols - 1),
            cell_of(bounds.ur.y, self.min_y, self.cell_h, self.rows - 1),
        ))
    }
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
            room_trees: (0..ctrl.layer_count.max(1))
                .map(|_| ArenaShapeTree::new())
                .collect(),
            ctrl,
            rooms,
            nodes: NodeArena::new(),
            drills: DrillArena::new(),
            pages,
            item_stack: QueryStack::new(),
            room_stack: QueryStack::new(),
            obstacle_rooms: BTreeMap::new(),
            bounds,
            scratch_entries: Vec::new(),
            scratch_rooms: Vec::new(),
            item_cache: None,
            scratch_hit_bounds: Vec::new(),
            scratch_grid_hits: Vec::new(),
            scratch_collect_obstacles: None,
            scratch_collect_own_net: None,
            tile_simplex_base: Vec::new(),
            tile_simplex_cache: Vec::new(),
            tile_sat_cache: Vec::new(),
            tile_octagon_cache: Vec::new(),
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
        if self.board.angle_restriction() == AngleRestriction::Ninety {
            TreeAngleFamily::Ninety
        } else if self.tiling_45.is_on() {
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
        for tree in &mut self.room_trees {
            tree.clear();
        }
        self.obstacle_rooms.clear();
        self.item_cache = None;
        self.tile_simplex_base.clear();
        self.tile_simplex_cache.clear();
        self.tile_sat_cache.clear();
        self.tile_octagon_cache.clear();
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
        self.item_tile_at(slot, item, index)
    }

    fn item_tile_at(&self, slot: SlotIndex, item: ItemId, index: u16) -> Option<Simplex> {
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
        self.item_tile_layer_at(slot, item, index)
    }

    fn item_tile_layer_at(&self, slot: SlotIndex, item: ItemId, index: u16) -> Option<i32> {
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
                if board_item.fixed_state.is_user_fixed() {
                    let corner = if index == 0 {
                        trace.first_corner()
                    } else {
                        trace.last_corner()
                    };
                    Some(corner.surrounding_box().to_simplex())
                } else {
                    fr_geometry::planar::LineSegment::from_polyline(
                        &trace.polyline,
                        index as usize + 1,
                    )
                    .map(|segment| segment.to_simplex())
                }
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
        self.collect_obstacles_tile(query_tile, layer, net_no, obstacles, own_net);
        // Upstream sorts *one* list — items, rooms and own-net entries together
        // (`SortedRoomNeighbours.java:139-147`) — and then splits it, so the
        // own-net entries reach `calculate_target_doors` in ascending
        // `(id_no, shape_index)`. Our tree query splits first, so the own-net
        // half needs its own sort, and it needs it only here: this is the entry
        // point whose caller reads `own_net`.
        own_net.sort_unstable();
        // One ascending `get_id_no()` key over items *and* rooms together —
        // `SortedRoomNeighbours.java:139-147`. See
        // [`sort_obstacles_for_neighbours`](Self::sort_obstacles_for_neighbours)
        // for why the two kinds interleave instead of items-then-rooms.
        self.sort_obstacles_for_neighbours(obstacles);
    }

    /// [`overlapping_obstacles_tile`](Self::overlapping_obstacles_tile) left in
    /// `ShapeSearchTree.complete_shape`'s order
    /// ([`sort_obstacles_for_completion`](Self::sort_obstacles_for_completion))
    /// instead of `SortedRoomNeighbours`'.
    ///
    /// The two upstream call sites really do sort differently, and the
    /// completion one used to pay for both: the query sorted the whole list into
    /// the neighbour order and `complete_shape` immediately re-sorted it. On
    /// `Issue230` that list averages 508 obstacles over 205 k completion
    /// queries — 104 M elements through a comparison sort that nothing reads.
    ///
    /// `own_net` comes back **unsorted** here, for the same reason: nothing on
    /// this path reads it (`ShapeSearchTree.complete_shape` has no own-net
    /// concept at all — it filters on `is_trace_obstacle` inline and drops the
    /// rest), and on `Issue039` the sort it used to run was 36.4 M entries over
    /// 3.74 M completion queries.
    pub fn overlapping_obstacles_tile_for_completion(
        &mut self,
        query_tile: &fr_board::shape::TileShape,
        layer: Option<i32>,
        net_no: i32,
        obstacles: &mut Vec<Obstacle>,
        own_net: &mut Vec<(ItemId, u16)>,
    ) {
        self.scratch_hit_bounds.clear();
        let ordered = self.collect_obstacles_tile(query_tile, layer, net_no, obstacles, own_net);
        if !ordered {
            let sort_started = crate::rooms::attr::start();
            self.sort_obstacles_for_completion_paired(obstacles);
            crate::rooms::attr::add_completion_sort(sort_started);
        }
    }

    /// Box-query collect: scan the per-layer item cache and the complete-room
    /// list. Equivalent to a bounds-only tree walk — extras fail the carve
    /// loop's `dimension() == 2` gate or the neighbour loop's `dimension() < 0`
    /// continue — and skips the DFS of an item tree the board-sized completion
    /// query contains entirely.
    ///
    /// Returns with `obstacles` already in completion order: rooms first by
    /// descending `id_no`, then items by descending id, ascending shape index
    /// — the order `sort_obstacles_for_completion` would impose, produced
    /// without a per-query sort of the whole list (that sort was 42 % of the
    /// collect time on Issue039). The item cache is presorted into that order
    /// at build time, so the linear scan emits it directly; the grid path
    /// sorts only its (small) hit slice.
    fn collect_obstacles_box(
        &mut self,
        query_box: IntBox,
        layer: i32,
        net_no: i32,
        obstacles: &mut Vec<Obstacle>,
        own_net: &mut Vec<(ItemId, u16)>,
    ) {
        self.collect_room_obstacles_box(query_box, layer, obstacles);
        own_net.clear();
        self.collect_item_obstacles_box(query_box, layer, net_no, obstacles, own_net);
    }

    /// Octagon-query collect: reuses [`collect_obstacles_box`] (the `ItemCache`
    /// and per-layer [`LayerGrid`]) on `query_oct.bounding_box()`, then keeps
    /// only hits that intersect `query_oct` itself.
    fn collect_obstacles_oct(
        &mut self,
        query_oct: IntOctagon,
        layer: i32,
        net_no: i32,
        obstacles: &mut Vec<Obstacle>,
        own_net: &mut Vec<(ItemId, u16)>,
    ) {
        self.collect_obstacles_box(query_oct.bounding_box(), layer, net_no, obstacles, own_net);
        let family = self.angle_family();
        let tree = self
            .board
            .trees()
            .compensated_tree_with_family(self.tree_class, family)
            .or_else(|| self.board.trees().compensated_tree(self.tree_class))
            .unwrap_or_else(|| self.board.default_tree());
        let rooms = &self.rooms;
        let store = self.board.store();
        let query_tile = fr_board::shape::TileShape::Oct(query_oct);
        obstacles.retain(|&obs| match obs {
            Obstacle::Room(r) => rooms.room(r).shape.intersects_octagon(&query_oct),
            Obstacle::Item(item, index, slot) => tree
                .item_shapes(slot, item)
                .get(index as usize)
                .and_then(|tile| tile.shape.as_ref())
                .is_some_and(|shape| shape.intersects(&query_tile)),
        });
        own_net.retain(|&(item, index)| {
            let Some(slot) = store.slot_of(item) else {
                return false;
            };
            tree.item_shapes(slot, item)
                .get(index as usize)
                .and_then(|tile| tile.shape.as_ref())
                .is_some_and(|shape| shape.intersects(&query_tile))
        });
        self.scratch_hit_bounds.clear();
    }

    /// Room-only half of `collect_obstacles_box`. Resets `obstacles` and
    /// `self.scratch_hit_bounds` and emits matching [`Obstacle::Room`]s in
    /// completion order (descending `id_no`).
    pub(crate) fn collect_room_obstacles_box(
        &mut self,
        query_box: IntBox,
        layer: i32,
        obstacles: &mut Vec<Obstacle>,
    ) {
        self.ensure_item_cache();
        obstacles.clear();
        self.scratch_hit_bounds.clear();

        let rooms_started = crate::rooms::attr::start();
        // Rooms first: the completion key sorts every room ahead of every
        // item, so an ordered rooms run followed by an ordered items run is
        // the sorted list. The walk + filter runs against the room tree as
        // before, then sorts by (-id_no, RoomId) only to dedup without extra
        // allocations.
        self.scratch_rooms.clear();
        let walk_started = crate::rooms::attr::start();
        // The room's layer is fixed at insertion, so the walk only touches the
        // query layer's tree.
        if let Some(room_tree) = self.room_trees.get(layer.max(0) as usize) {
            room_tree.overlaps_with_stack(
                &fr_board::shape::TileShape::Box(query_box).bounds(),
                &mut self.room_stack,
                &mut self.scratch_rooms,
            );
        }
        crate::rooms::attr::add_room_walk(walk_started, self.scratch_rooms.len() as u64);
        let mut rooms = std::mem::take(&mut self.scratch_rooms);
        rooms.retain(|(_, room)| {
            let room = self.rooms.room(*room);
            room.alive
                && room.kind == RoomKind::CompleteFree
                && room.layer == layer
                && query_box.intersects(&room.octagon().bounding_box())
        });
        if rooms.len() > 1 {
            rooms.sort_unstable_by(|(_, a), (_, b)| {
                self.rooms
                    .room(*b)
                    .id_no
                    .cmp(&self.rooms.room(*a).id_no)
                    .then_with(|| a.cmp(b))
            });
            rooms.dedup_by_key(|(_, room)| *room);
        }
        for (_, room) in &rooms {
            obstacles.push(Obstacle::Room(*room));
            self.scratch_hit_bounds
                .push(self.rooms.room(*room).octagon().bounding_box());
        }
        rooms.clear();
        self.scratch_rooms = rooms;
        crate::rooms::attr::add_collect_rooms(rooms_started);
        crate::rooms::attr::add_room_survivors(obstacles.len() as u64);
    }

    /// Item-only half of `collect_obstacles_box`. Appends matching items and
    /// their bounds (`self.scratch_hit_bounds`) in completion order without
    /// clearing existing entries, so callers can run room carving first and
    /// query items against the shrunk candidate box.
    pub(crate) fn collect_item_obstacles_box(
        &mut self,
        query_box: IntBox,
        layer: i32,
        net_no: i32,
        obstacles: &mut Vec<Obstacle>,
        own_net: &mut Vec<(ItemId, u16)>,
    ) {
        self.ensure_item_cache();

        // Split borrows: the cache is read, the hit lists are written.
        let items_started = crate::rooms::attr::start();
        let item_cache = self.item_cache.as_mut().expect("cache just built");
        let hit_bounds: &mut Vec<IntBox> = &mut self.scratch_hit_bounds;
        let store = self.board.store();
        let net_matches = net_no == item_cache.net_no;
        let mut visit = |cached: &CachedItem,
                         obstacles_out: &mut Vec<Obstacle>,
                         own_net_out: &mut Vec<(ItemId, u16)>| {
            if !query_box.intersects(&cached.bounds) {
                return;
            }
            let obstacle = if net_matches {
                cached.obstacle
            } else {
                match store.item_at_slot(cached.slot) {
                    Some(item) if item.id == cached.item => item.is_trace_obstacle(net_no),
                    _ => return,
                }
            };
            if obstacle {
                obstacles_out.push(Obstacle::Item(cached.item, cached.index, cached.slot));
                hit_bounds.push(cached.bounds);
            } else {
                own_net_out.push((cached.item, cached.index));
            }
        };
        let ItemCache {
            layers, marks, gen, ..
        } = item_cache;
        let Some(layer_items) = layers.get(layer as usize) else {
            crate::rooms::attr::add_collect_items(items_started);
            return;
        };
        let grid_and_range = layer_items
            .grid
            .as_ref()
            .and_then(|grid| grid.cell_range(&query_box).map(|range| (grid, range)));
        let use_grid = grid_and_range
            .map(|(grid, (x0, y0, x1, y1))| {
                // The grid pays off only when it skips a real fraction of the
                // layer; a box covering half the cells is scanned linearly,
                // which also emits the items in completion order directly.
                (x1 - x0 + 1) * (y1 - y0 + 1) * 2 <= grid.cols * grid.rows
            })
            .unwrap_or(false);
        if let Some((grid, (x0, y0, x1, y1))) = grid_and_range.filter(|_| use_grid) {
            *gen = (*gen).wrapping_add(1);
            if *gen == 0 {
                marks.fill(0);
                *gen = 1;
            }
            let gen = *gen;
            let mut hits = std::mem::take(&mut self.scratch_grid_hits);
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let cell = y * grid.cols + x;
                    for index in
                        &grid.indices[grid.offsets[cell] as usize..grid.offsets[cell + 1] as usize]
                    {
                        let index = *index as usize;
                        if marks[index] == gen {
                            continue;
                        }
                        marks[index] = gen;
                        hits.push(index as u32);
                    }
                }
            }
            // The cache is presorted in completion order, so emitting the
            // grid hits by ascending cache index reproduces it exactly.
            hits.sort_unstable();
            for index in &hits {
                visit(&layer_items.items[*index as usize], obstacles, own_net);
            }
            hits.clear();
            self.scratch_grid_hits = hits;
        } else {
            for cached in &layer_items.items {
                visit(cached, obstacles, own_net);
            }
        }
        crate::rooms::attr::add_collect_items(items_started);
    }

    fn ensure_item_cache(&mut self) {
        if self.item_cache.is_some() {
            return;
        }
        let family = self.angle_family();
        let tree = self
            .board
            .trees()
            .compensated_tree_with_family(self.tree_class, family)
            .or_else(|| self.board.trees().compensated_tree(self.tree_class))
            .unwrap_or_else(|| self.board.default_tree());
        let mut layers: Vec<Vec<CachedItem>> = vec![Vec::new(); self.ctrl.layer_count.max(1)];
        let net_no = self.ctrl.net_no;
        for (id, item) in self.board.store().iter_ascending() {
            let Some(slot) = self.board.store().slot_of(id) else {
                continue;
            };
            let tile_count = tree.item_shapes(slot, id).len();
            if slot.index() >= self.tile_simplex_base.len() {
                self.tile_simplex_base.resize(slot.index() + 1, 0);
            }
            self.tile_simplex_base[slot.index()] = self.tile_simplex_cache.len() as u32;
            self.tile_simplex_cache
                .resize_with(self.tile_simplex_cache.len() + tile_count, Default::default);
            self.tile_sat_cache
                .resize_with(self.tile_sat_cache.len() + tile_count, Default::default);
            self.tile_octagon_cache
                .resize_with(self.tile_octagon_cache.len() + tile_count, Default::default);
            let obstacle = item.is_trace_obstacle(net_no);
            for (index, tile) in tree.item_shapes(slot, id).iter().enumerate() {
                let Some(shape) = tile.shape.as_ref() else {
                    continue;
                };
                if tile.layer < 0 {
                    continue;
                }
                let layer = tile.layer as usize;
                if layer >= layers.len() {
                    layers.resize(layer + 1, Vec::new());
                }
                let bounds = match shape {
                    fr_board::shape::TileShape::Box(b) => *b,
                    fr_board::shape::TileShape::Oct(o) => o.bounding_box(),
                    fr_board::shape::TileShape::Simplex(s) => s.bounding_box(),
                };
                layers[layer].push(CachedItem {
                    item: id,
                    index: index as u16,
                    slot,
                    bounds,
                    obstacle,
                });
            }
        }
        let board = self.bounds;
        let mut marks = 0usize;
        let layers = layers
            .into_iter()
            .map(|mut items| {
                // Completion visit order (`Leaf.compareTo`): descending item
                // id, ascending shape index. The linear scan then emits an
                // ordered run and the grid only has to sort its hit indices.
                items.sort_unstable_by_key(|cached| {
                    (
                        std::cmp::Reverse(i64::from(cached.item.raw())),
                        cached.index,
                    )
                });
                marks = marks.max(items.len());
                let grid = if items.len() >= GRID_MIN_ITEMS {
                    LayerGrid::build(&items, board)
                } else {
                    None
                };
                LayerItems { items, grid }
            })
            .collect();
        self.item_cache = Some(ItemCache {
            layers,
            net_no,
            marks: vec![0; marks],
            gen: 0,
        });
    }

    /// Checks out the collect buffers (fresh when a nested collect is holding
    /// them). Pair with [`give_back_collect_vecs`](Self::give_back_collect_vecs).
    pub(crate) fn take_collect_vecs(&mut self) -> (Vec<Obstacle>, Vec<(ItemId, u16)>) {
        (
            self.scratch_collect_obstacles.take().unwrap_or_default(),
            self.scratch_collect_own_net.take().unwrap_or_default(),
        )
    }

    /// Returns the buffers to the pool. A nested user finding the slot taken
    /// keeps its own — the pool holds at most one pair.
    pub(crate) fn give_back_collect_vecs(
        &mut self,
        obstacles: Vec<Obstacle>,
        own_net: Vec<(ItemId, u16)>,
    ) {
        if self.scratch_collect_obstacles.is_none() {
            let mut obstacles = obstacles;
            obstacles.clear();
            self.scratch_collect_obstacles = Some(obstacles);
        }
        if self.scratch_collect_own_net.is_none() {
            let mut own_net = own_net;
            own_net.clear();
            self.scratch_collect_own_net = Some(own_net);
        }
    }

    /// The body the two ordered entry points share: the tree walk, the
    /// obstacle/own-net split and the room-tree tail, in no particular order.
    ///
    /// `own_net` is left unsorted; the entry point whose caller reads it sorts
    /// it.
    ///
    /// Returns `true` when `obstacles` is already in completion visit order
    /// (the box path emits it directly), so the completion entry point can
    /// skip its re-sort.
    fn collect_obstacles_tile(
        &mut self,
        query_tile: &fr_board::shape::TileShape,
        layer: Option<i32>,
        net_no: i32,
        obstacles: &mut Vec<Obstacle>,
        own_net: &mut Vec<(ItemId, u16)>,
    ) -> bool {
        obstacles.clear();
        own_net.clear();
        fr_metrics::counters::bump::tree_queries(1);

        if let (fr_board::shape::TileShape::Box(query_box), Some(layer)) = (query_tile, layer) {
            self.collect_obstacles_box(*query_box, layer, net_no, obstacles, own_net);
            return true;
        }
        if let (fr_board::shape::TileShape::Oct(query_oct), Some(layer)) = (query_tile, layer) {
            self.collect_obstacles_oct(*query_oct, layer, net_no, obstacles, own_net);
            return false;
        }

        let family = self.angle_family();
        let tree = self
            .board
            .trees()
            .compensated_tree_with_family(self.tree_class, family)
            .or_else(|| self.board.trees().compensated_tree(self.tree_class))
            .unwrap_or_else(|| self.board.default_tree());
        // Both ordered entry points re-sort the result
        // (`sort_obstacles_for_completion` / `sort_obstacles_for_neighbours`),
        // so the TreeSet order `overlapping_tree_entries` would impose is
        // thrown away. Upstream matches: `complete_shape` walks then
        // `Collections.sort`s with `Leaf.compareTo`, and
        // `SortedRoomNeighbours.calculate_neighbours` re-sorts after the query
        // (`:173-179`). `insert_forced` / `pull_tight` still use the sorted
        // walk — they consume in query order.
        tree.overlapping_tree_entries_unsorted(
            self.board.store(),
            query_tile,
            layer,
            &[],
            &mut self.item_stack,
            &mut self.scratch_entries,
        );
        for entry in &self.scratch_entries {
            // The walk already proved `entry.slot` is this item: `visit` checks
            // `SlotEntry::id` and `item_at_slot`. Indexing by slot here is the
            // same proof without `store.get`'s binary search over the ordered
            // index — which the completion carve then used to repeat per
            // obstacle (76 M times on a 25 s Issue039 slice).
            let Some(item) = self.board.store().item_at_slot(entry.slot) else {
                continue;
            };
            if item.id != entry.item {
                continue;
            }
            if item.is_trace_obstacle(net_no) {
                obstacles.push(Obstacle::Item(entry.item, entry.shape_index, entry.slot));
            } else {
                own_net.push((entry.item, entry.shape_index));
            }
        }
        self.scratch_rooms.clear();
        // A layered query walks only that layer's tree; an all-layer query
        // (the drill paths) walks each in turn. A room lives in exactly one
        // tree, so no cross-tree duplicates appear.
        match layer {
            Some(l) => {
                if let Some(room_tree) = self.room_trees.get(l.max(0) as usize) {
                    room_tree.overlaps_with_stack(
                        &query_tile.bounds(),
                        &mut self.room_stack,
                        &mut self.scratch_rooms,
                    );
                }
            }
            None => {
                // `overlaps_with_stack` clears `out` first, so an all-layer
                // query accumulates through `visit_overlaps` instead.
                for room_tree in &self.room_trees {
                    let out = &mut self.scratch_rooms;
                    room_tree.visit_overlaps(
                        &query_tile.bounds(),
                        &mut self.room_stack,
                        |id, payload| out.push((id, payload)),
                    );
                }
            }
        }
        // The room hits are appended in place and de-duplicated as a tail
        // slice, not collected into a `Vec<RoomId>` first: this runs once per
        // tree query — 604k times on Issue034 — and the temporary was one heap
        // allocation each time.
        let rooms_from = obstacles.len();
        obstacles.extend(
            self.scratch_rooms
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
                            // A box query answers itself off the room's
                            // *memoized* octagon: a room the box already
                            // contains overlaps it, and the shape test is
                            // skipped. This is not a heuristic — the room is a
                            // non-empty 2-dimensional tile inside its own
                            // bounding box — and it is the whole cost of this
                            // filter on the completion path, where the query
                            // box starts as the board bounding box and
                            // therefore contains every room there is.
                            // `Simplex::intersects_box` has the same
                            // short-circuit inside it, but only after
                            // `bounded_bounding_box` has walked the room's
                            // border lines and intersected each consecutive
                            // pair; `Room::octagon` walks them once per room
                            // per connection instead of once per query.
                            // Measured on `Issue039`: 770 M room candidates
                            // reach this test over one 285 s run.
                            fr_board::shape::TileShape::Box(b) => {
                                b.contains_box(&room.octagon().bounding_box())
                                    || room.shape.intersects_box(b)
                            }
                            fr_board::shape::TileShape::Oct(o) => room.shape.intersects_octagon(o),
                        }
                })
                .map(Obstacle::Room),
        );
        // `Obstacle`'s derived order puts `Room` last and orders rooms by id, so
        // over a tail that is all rooms this is the old `sort + dedup` on
        // `RoomId`. A room can be reached through more than one leaf, so the
        // dedup is load-bearing, not defensive.
        obstacles[rooms_from..].sort_unstable();
        let kept = dedup_in_place(&mut obstacles[rooms_from..]);
        obstacles.truncate(rooms_from + kept);
        false
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
        // `sort_by_cached_key`: the key chases `rooms.room()` — once per
        // element here, not once per comparison.
        obstacles.sort_by_cached_key(|obstacle| self.completion_key(*obstacle));
    }

    fn completion_key(&self, obstacle: Obstacle) -> (u8, i64, u16) {
        match obstacle {
            Obstacle::Room(room) => (0u8, -self.rooms.room(room).id_no, 0u16),
            Obstacle::Item(item, index, _) => (1u8, -(i64::from(item.raw())), index),
        }
    }

    fn sort_obstacles_for_completion_paired(&mut self, obstacles: &mut Vec<Obstacle>) {
        if self.scratch_hit_bounds.len() != obstacles.len() {
            self.sort_obstacles_for_completion(obstacles);
            return;
        }
        let bounds = std::mem::take(&mut self.scratch_hit_bounds);
        let mut paired: Vec<(Obstacle, IntBox)> = obstacles.drain(..).zip(bounds).collect();
        paired.sort_by_cached_key(|(obstacle, _)| self.completion_key(*obstacle));
        for (obstacle, bounds) in paired {
            obstacles.push(obstacle);
            self.scratch_hit_bounds.push(bounds);
        }
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
        obstacles.sort_unstable_by(|&a, &b| {
            // The room arm sorts first on an exact id tie: upstream's sort is
            // stable over a tree walk we do not reproduce, so the tie needs
            // *some* rule, and the rooms are the objects this connection just
            // built — visiting them before the board items keeps a freshly
            // completed neighbour from being re-cut by an item of the same id.
            let ka = match a {
                Obstacle::Room(room) => (self.rooms.room(room).id_no, 0u8, 0u16),
                Obstacle::Item(item, index, _) => (i64::from(item.raw()), 1u8, index),
            };
            let kb = match b {
                Obstacle::Room(room) => (self.rooms.room(room).id_no, 0u8, 0u16),
                Obstacle::Item(item, index, _) => (i64::from(item.raw()), 1u8, index),
            };
            ka.cmp(&kb)
        });
    }

    /// The shape of one obstacle.
    pub fn obstacle_shape(&self, obstacle: Obstacle) -> Option<Simplex> {
        match obstacle {
            Obstacle::Item(item, index, slot) => self.item_tile_at(slot, item, index),
            Obstacle::Room(room) => {
                let room = self.rooms.room(room);
                room.alive.then(|| room.shape.clone())
            }
        }
    }

    /// [`obstacle_shape`](Self::obstacle_shape) with the item arm served from
    /// the per-connection tile memo — identical shapes, one `tile_to_simplex`
    /// per tile per connection instead of one per carve hit. Rooms still
    /// clone: they churn too fast for a memo.
    /// [`obstacle_shape`](Self::obstacle_shape) paired with its memoized
    /// SAT data `(corners, verified)`. Items cache both simplex and SAT per
    /// connection; rooms memoize SAT on [`Room::cached_sat`].
    pub(crate) fn obstacle_shape_and_sat(
        &mut self,
        obstacle: Obstacle,
    ) -> Option<(
        std::rc::Rc<Simplex>,
        Option<std::rc::Rc<(fr_geometry::planar::SatCorners, Option<bool>)>>,
    )> {
        match obstacle {
            Obstacle::Item(item, index, slot) => {
                let shape = self.item_tile_simplex_rc(slot, item, index)?;
                let base = *self.tile_simplex_base.get(slot.index())?;
                let sat = self
                    .tile_sat_cache
                    .get(base as usize + index as usize)?
                    .get_or_init(|| {
                        shape.sat_corners().map(|corners| {
                            let verified = shape.sat_verify(&corners);
                            std::rc::Rc::new((corners, verified))
                        })
                    })
                    .clone();
                Some((shape, sat))
            }
            Obstacle::Room(room) => {
                let room = self.rooms.room(room);
                if !room.alive {
                    return None;
                }
                Some((
                    std::rc::Rc::new(room.shape.clone()),
                    room.cached_sat().cloned(),
                ))
            }
        }
    }

    fn item_tile_simplex_rc(
        &mut self,
        slot: SlotIndex,
        item: ItemId,
        index: u16,
    ) -> Option<std::rc::Rc<Simplex>> {
        self.ensure_item_cache();
        let base = *self.tile_simplex_base.get(slot.index())?;
        let cell = self
            .tile_simplex_cache
            .get(base as usize + index as usize)?;
        if let Some(rc) = cell.get() {
            return Some(rc.clone());
        }
        let simplex = self.item_tile_at(slot, item, index)?;
        let rc = std::rc::Rc::new(simplex);
        let _ = cell.set(rc.clone());
        Some(rc)
    }

    /// The layer *and* the bounding box of one obstacle, read straight off the
    /// stored tile shape — without materializing
    /// [`obstacle_shape`](Self::obstacle_shape).
    ///
    /// `complete_shape` gathers its obstacles with one tree walk against the
    /// *initial* bound (upstream's `ShapeSearchTree.java:578-595`) and then
    /// discards almost all of them: on `Issue230` the route stage sees 104.3 M
    /// obstacles, 72.4 M of them survive the layer test, and **69.3 M of those
    /// 72.4 M — 95.7 % — are then bounded away** because the running bound has
    /// shrunk since the walk. Materializing the simplex before that rejection
    /// cost 60 M `IntOctagon::to_simplex` calls, 18 % of the whole run.
    ///
    /// The two answers the rejection needs are both O(1) on the tile:
    ///
    /// | tile | bounding box |
    /// |---|---|
    /// | `Box(b)` | `b` — `b.to_simplex()`'s corners are exactly `ll`/`ur` |
    /// | `Oct(o)` | `o.bounding_box()`, i.e. `[left_x, right_x] × [bottom_y, top_y]` |
    /// | `Simplex(s)` | `s.bounding_box()`, the same call the caller made |
    ///
    /// The octagon row is a **superset** of `o.to_simplex().bounding_box()`
    /// (which floors/ceils float corners that already lie inside those integer
    /// bounds), never a subset, so the guard it feeds can only become *more*
    /// permissive. That cannot change an answer: an obstacle that passes the
    /// wider box but not the narrow one has an empty intersection with every
    /// surviving candidate, so it fails the `dimension() == 2` gate and leaves
    /// both `result` and `bounding` exactly as it found them.
    pub fn obstacle_layer_and_bounds(&self, obstacle: Obstacle) -> Option<(i32, IntBox)> {
        match obstacle {
            Obstacle::Item(item, index, slot) => {
                let tile = self.tree().item_shapes(slot, item).get(index as usize)?;
                let bounds = match tile.shape.as_ref()? {
                    fr_board::shape::TileShape::Box(b) => *b,
                    fr_board::shape::TileShape::Oct(o) => o.bounding_box(),
                    fr_board::shape::TileShape::Simplex(s) => s.bounding_box(),
                };
                Some((tile.layer, bounds))
            }
            Obstacle::Room(room) => {
                let room = self.rooms.room(room);
                // `Room::octagon` is the memoized bounding octagon, and its
                // `[left_x, right_x] × [bottom_y, top_y]` is the same box
                // `shape.bounding_box()` computes — from the same corners, once
                // per room instead of once per carve-loop visit. The unbounded
                // case answers the coordinate envelope, which is what
                // `Simplex::bounding_box` answers for an unbounded simplex too;
                // and the table above already sanctions a wider box here.
                room.alive
                    .then(|| (room.layer, room.octagon().bounding_box()))
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
        self.item_tile_octagon_at(slot, item, index)
    }

    fn item_tile_octagon_at(
        &self,
        slot: SlotIndex,
        item: ItemId,
        index: u16,
    ) -> Option<IntOctagon> {
        if let Some(cell) = self
            .tile_simplex_base
            .get(slot.index())
            .and_then(|base| self.tile_octagon_cache.get(*base as usize + index as usize))
        {
            return *cell.get_or_init(|| {
                self.tree()
                    .item_shapes(slot, item)
                    .get(index as usize)
                    .and_then(|tile| tile.shape.as_ref())
                    .and_then(tile_octagon)
            });
        }
        // The cache only exists once the connection's item cache is built;
        // every 45°-chain caller has collected first, so this is the cold
        // path for direct callers.
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
            Obstacle::Item(item, index, slot) => self.item_tile_octagon_at(slot, item, index),
            Obstacle::Room(room) => {
                let room = self.rooms.room(room);
                room.alive.then(|| room.octagon())
            }
        }
    }

    /// [`obstacle_shape`](Self::obstacle_shape) in the box flavor.
    pub fn obstacle_box(&self, obstacle: Obstacle) -> Option<IntBox> {
        match obstacle {
            Obstacle::Item(item, index, slot) => {
                let tile = self
                    .tree()
                    .item_shapes(slot, item)
                    .get(index as usize)?
                    .shape
                    .as_ref()?;
                Some(tile.bounding_box())
            }
            Obstacle::Room(room) => {
                let room = self.rooms.room(room);
                room.alive.then(|| room.shape.bounding_box())
            }
        }
    }

    /// The layer of one obstacle.
    pub fn obstacle_layer(&self, obstacle: Obstacle) -> Option<i32> {
        match obstacle {
            Obstacle::Item(item, index, slot) => self.item_tile_layer_at(slot, item, index),
            Obstacle::Room(room) => Some(self.rooms.room(room).layer),
        }
    }

    /// `Pin.get_trace_neckdown_halfwidth(layer)` (`board/Pin.java:534-537`):
    /// `max(0.5 * min_width - 1, 1)` of the pad's bounding box on that layer.
    /// `None` when the item is not a pin or has no pad shape on `layer` —
    /// callers then leave the half width alone (the pin is not on the layer,
    /// so no neckdown applies).
    pub fn pin_trace_neckdown_halfwidth(&self, pin: ItemId, layer: i32) -> Option<i32> {
        let item = self.board.store().get(pin)?;
        let ItemKind::Pin(pin_payload) = &item.kind else {
            return None;
        };
        if !item.is_on_layer(layer) {
            return None;
        }
        let pad = self
            .board
            .padstacks()
            .get(pin_payload.padstack_no)?
            .shape_on_layer(layer)?;
        let min_width = pad.bounding_box().min_width();
        Some((0.5 * min_width - 1.0).max(1.0) as i32)
    }

    /// Inserts a completed room into the engine-local tree of its layer.
    pub(crate) fn insert_room_in_tree(&mut self, room_id: RoomId) {
        let bounds =
            fr_board::shape::TileShape::Simplex(self.rooms.room(room_id).shape.clone()).bounds();
        let layer = self.rooms.room(room_id).layer.max(0) as usize;
        if layer >= self.room_trees.len() {
            self.room_trees.resize(layer + 1, ArenaShapeTree::new());
        }
        let leaf = self.room_trees[layer].insert(bounds, room_id);
        self.rooms.room_mut(room_id).leaf = Some(leaf);
    }

    /// Removes a completed room from the engine-local tree of its layer.
    pub(crate) fn remove_room_from_tree(&mut self, room_id: RoomId) {
        if let Some(leaf) = self.rooms.room_mut(room_id).leaf.take() {
            let layer = self.rooms.room(room_id).layer.max(0) as usize;
            self.room_trees[layer].remove(leaf);
        }
    }

    pub(crate) fn enter_restrain(&mut self) -> bool {
        if self.restrain_depth >= MAX_RESTRAIN_DEPTH {
            return false;
        }
        self.restrain_depth += 1;
        true
    }

    pub(crate) fn restrain_nesting(&self) -> u32 {
        self.restrain_depth
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
    use super::{AutorouteEngine, Obstacle, RoomId};
    use crate::test_support::TestFixture;
    use fr_board::item::ItemId;
    use fr_board::store::SlotIndex;
    use fr_geometry::planar::{IntBox, IntOctagon, Simplex};

    #[test]
    fn dedup_in_place_is_vec_dedup_over_a_slice() {
        let mut items = [1, 1, 2, 3, 3, 3, 4];
        let kept = super::dedup_in_place(&mut items);
        assert_eq!(&items[..kept], &[1, 2, 3, 4]);

        let mut empty: [u32; 0] = [];
        assert_eq!(super::dedup_in_place(&mut empty), 0);

        let mut single = [7];
        assert_eq!(super::dedup_in_place(&mut single), 1);

        // Only *adjacent* runs collapse, exactly like `Vec::dedup`.
        let mut unsorted = [1, 2, 1];
        let kept = super::dedup_in_place(&mut unsorted);
        assert_eq!(&unsorted[..kept], &[1, 2, 1]);
    }

    #[test]
    fn a_room_reachable_through_two_leaves_is_returned_once() {
        // The room hits are appended straight onto `obstacles` and deduped as a
        // tail slice; a room the tree hands back twice must still appear once,
        // and the rooms must stay in ascending id order behind the items.
        let fixture = TestFixture::two_layer_with_pads();
        let mut engine = fixture.engine();
        let square = IntBox::new(-100, -100, 100, 100).to_simplex();
        let rooms: Vec<RoomId> = (0..3)
            .map(|_| {
                let room = engine.rooms.add_complete_free_room(square.clone(), 0);
                engine.mint_room_id(room);
                engine.insert_room_in_tree(room);
                room
            })
            .collect();
        let mut obstacles = Vec::new();
        let mut own_net = Vec::new();
        engine.overlapping_obstacles(
            &square,
            Some(0),
            crate::test_support::TEST_NET,
            &mut obstacles,
            &mut own_net,
        );
        let hits: Vec<RoomId> = obstacles
            .iter()
            .filter_map(|obstacle| match *obstacle {
                Obstacle::Room(room) => Some(room),
                Obstacle::Item(..) => None,
            })
            .collect();
        assert_eq!(hits, rooms, "each room once, in ascending id order");
    }

    /// The two ordered entry points differ **only** in the final sort. The
    /// completion one used to be `overlapping_obstacles_tile` followed by
    /// `sort_obstacles_for_completion` at the call site, which sorted the whole
    /// list twice; splitting the shared gather out has to leave both the set
    /// and each entry point's order exactly as they were.
    #[test]
    fn the_two_obstacle_orders_are_one_gather_sorted_two_ways() {
        let fixture = TestFixture::with_foreign_trace();
        let mut engine = fixture.engine();
        let room = engine
            .rooms
            .add_complete_free_room(IntBox::new(-100, -100, 100, 100).to_simplex(), 0);
        engine.mint_room_id(room);
        engine.insert_room_in_tree(room);

        let query = fr_board::shape::TileShape::Box(engine.bounds());
        let mut neighbour_order = Vec::new();
        let mut neighbour_own_net = Vec::new();
        engine.overlapping_obstacles_tile(
            &query,
            Some(0),
            crate::test_support::TEST_NET,
            &mut neighbour_order,
            &mut neighbour_own_net,
        );
        let mut completion_order = Vec::new();
        let mut completion_own_net = Vec::new();
        engine.overlapping_obstacles_tile_for_completion(
            &query,
            Some(0),
            crate::test_support::TEST_NET,
            &mut completion_order,
            &mut completion_own_net,
        );

        assert!(
            completion_order.len() > 1,
            "the probe must return more than one obstacle to say anything about order"
        );
        let mut sorted_completion_own_net = completion_own_net.clone();
        sorted_completion_own_net.sort_unstable();
        assert_eq!(
            neighbour_own_net, sorted_completion_own_net,
            "the own-net split is part of the shared gather; only the sort is not"
        );
        assert!(
            neighbour_own_net.windows(2).all(|pair| pair[0] <= pair[1]),
            "the neighbour entry point owes `calculate_target_doors` ascending order"
        );
        let mut a = neighbour_order.clone();
        let mut b = completion_order.clone();
        a.sort_unstable();
        b.sort_unstable();
        assert_eq!(
            a, b,
            "the two entry points must gather the same obstacle set"
        );

        // And each is its own upstream order, not the other's: rooms lead in the
        // completion order (`CompleteFreeSpaceExpansionRoom.compareTo`) and
        // interleave by ascending id in the neighbour one
        // (`SortedRoomNeighbours.java:139-147`).
        let mut resorted = neighbour_order.clone();
        engine.sort_obstacles_for_completion(&mut resorted);
        assert_eq!(resorted, completion_order);
        let mut resorted = completion_order.clone();
        engine.sort_obstacles_for_neighbours(&mut resorted);
        assert_eq!(resorted, neighbour_order);
    }

    /// A room shape whose corners are *not* its bounding box's corners, so the
    /// two claims the octagon memo rests on are actually exercised.
    ///
    /// `[-100,100]²` with the four corners clipped at `|x ± y| ≤ 150`: the
    /// extreme x is still ±100 (reached at `|y| ≤ 50`), so the bounding box is
    /// the whole square, but `(95, 95)` — inside that box — is outside the room.
    fn clipped_square() -> Simplex {
        IntOctagon::new(-100, -100, 100, 100, -150, 150, -150, 150).to_simplex()
    }

    /// [`AutorouteEngine::obstacle_layer_and_bounds`] answers a room off
    /// `Room::octagon`, which is memoized; the claim that makes that legal is
    /// that the octagon's box *is* the shape's bounding box.
    #[test]
    fn a_rooms_obstacle_bounds_are_its_shape_bounding_box() {
        let fixture = TestFixture::two_layer_with_pads();
        let mut engine = fixture.engine();
        let shape = clipped_square();
        let room = engine.rooms.add_complete_free_room(shape.clone(), 0);
        let (layer, bounds) = engine
            .obstacle_layer_and_bounds(Obstacle::Room(room))
            .expect("a live room has bounds");
        assert_eq!(layer, 0);
        assert_eq!(bounds, shape.bounding_box());
        // Twice, because the second call is the memoized one and a memo that
        // answers differently is the whole risk.
        assert_eq!(
            engine.obstacle_layer_and_bounds(Obstacle::Room(room)),
            Some((0, shape.bounding_box()))
        );
    }

    /// The box arm's containment short circuit accepts a room the query box
    /// contains without asking the geometry — and, crucially, does **not**
    /// accept a room that only overlaps the box's *bounds*.
    #[test]
    fn a_box_query_short_circuits_containment_without_widening_the_room_set() {
        let fixture = TestFixture::two_layer_with_pads();
        let mut engine = fixture.engine();
        let shape = clipped_square();
        let room = engine.rooms.add_complete_free_room(shape.clone(), 0);
        engine.mint_room_id(room);
        engine.insert_room_in_tree(room);

        let rooms_for = |engine: &mut AutorouteEngine<'_>, query: &fr_board::shape::TileShape| {
            let mut obstacles = Vec::new();
            let mut own_net = Vec::new();
            engine.overlapping_obstacles_tile_for_completion(
                query,
                Some(0),
                crate::test_support::TEST_NET,
                &mut obstacles,
                &mut own_net,
            );
            obstacles
                .into_iter()
                .filter_map(|obstacle| match obstacle {
                    Obstacle::Room(room) => Some(room),
                    Obstacle::Item(..) => None,
                })
                .collect::<Vec<_>>()
        };

        // The whole board contains the room: the short circuit fires.
        let bounds = engine.bounds();
        assert_eq!(
            rooms_for(&mut engine, &fr_board::shape::TileShape::Box(bounds)),
            vec![room]
        );
        // The clipped corner: the box meets the room's *bounding box* and the
        // tree hands the room over, but no point of the room is in the box, so
        // the exact test still has to run and still has to say no.
        let corner = IntBox::new(90, 90, 100, 100);
        assert!(
            corner.intersects(&shape.bounding_box()),
            "the probe only means something if the bounds do overlap"
        );
        assert!(
            rooms_for(&mut engine, &fr_board::shape::TileShape::Box(corner)).is_empty(),
            "a room the box misses must not survive the containment short circuit"
        );
    }

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

    /// Three rooms with `id_no` 1, 2, 3 on a fixture whose items are ids 1..n,
    /// so both comparators have something to interleave.
    ///
    /// The ids are minted here rather than by `add_complete_free_room`, which
    /// is what `calculate_doors` does on the real path (upstream mints in
    /// `SortedRoomNeighbours.calculate`, not at room construction).
    fn ordering_fixture(engine: &mut crate::engine::AutorouteEngine<'_>) -> Vec<Obstacle> {
        let square = IntBox::new(0, 0, 10, 10).to_simplex();
        let rooms: Vec<RoomId> = (0..3)
            .map(|_| {
                let room = engine.rooms.add_complete_free_room(square.clone(), 0);
                engine.mint_room_id(room);
                room
            })
            .collect();
        assert_eq!(
            rooms
                .iter()
                .map(|r| engine.rooms.room(*r).id_no)
                .collect::<Vec<_>>(),
            vec![1, 2, 3],
            "`generate_room_id_no` is upstream's `++count`, so it starts at 1"
        );
        vec![
            Obstacle::Item(ItemId::from_raw(2), 1, SlotIndex::from_raw(0)),
            Obstacle::Room(rooms[1]),
            Obstacle::Item(ItemId::from_raw(1), 0, SlotIndex::from_raw(0)),
            Obstacle::Room(rooms[2]),
            Obstacle::Item(ItemId::from_raw(2), 0, SlotIndex::from_raw(0)),
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
                Obstacle::Item(item, index, _) => (1, i64::from(item.raw()), index),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                (0, 3, 0),
                (0, 2, 0),
                (0, 1, 0),
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
                Obstacle::Item(item, index, _) => (i64::from(item.raw()), 1, index),
            })
            .collect();
        assert_eq!(
            keys,
            vec![
                (1, 0, 0),
                (1, 1, 0),
                (2, 0, 0),
                (2, 1, 0),
                (2, 1, 1),
                (3, 0, 0),
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
