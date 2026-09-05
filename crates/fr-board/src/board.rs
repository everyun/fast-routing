//! The board: the item store, the layer stack, the design rules the geometry
//! needs, the search trees and the connectivity index — plus the mutation
//! contract that keeps all four in step.
//!
//! Ported from `BasicBoard` / `BoardItemRepository`
//! (`v2-vs-v19-and-scope.md` §11) with the storage and undo concerns split out
//! into [`ItemStore`] and its journal, as `search-tree-obstacles.md` §10 calls
//! for.
//!
//! # The mutation protocol (§11.2)
//!
//! Every change goes through exactly two primitives, and their asymmetry is
//! load-bearing:
//!
//! ```text
//! insert_item:  clearance-class range check → store insert → search trees
//!               → connectivity                       (trees AFTER the store)
//! remove_item:  isDeletionForbidden? → no-op
//!               connectivity → search trees → store delete
//!                                                    (trees BEFORE the store)
//! ```
//!
//! The tree removal has to run *before* the store deletes the item, because the
//! tree bookkeeping is keyed by the item's arena slot and the slot stops
//! resolving the moment the store frees it. Upstream has the same ordering for
//! the same reason (its `additionalUpdateAfterChange` must see the shapes while
//! they are still known).
//!
//! # Transactions
//!
//! [`Board::begin`] opens a store transaction *and* starts recording which items
//! the transaction touched. [`Board::rollback_to`] then restores the derived
//! state in three steps: detach every touched item from the trees while its
//! slot is still valid, roll the store back, and re-index whatever is live
//! again. That is O(touched items), not O(board), and it is why the trees never
//! need their own undo log — the store is the single source of truth and the
//! trees are a pure function of it.
//!
//! # What is not here yet
//!
//! * `ChangedArea` — upstream's per-layer mutable octagon of "what this
//!   operation touched", which supplies the clip to
//!   [`insert_trace`](Board::insert_trace) and drives regional pull-tight. The
//!   clip is a parameter here instead; the type belongs with the shove/optimize
//!   wave that consumes it.
//! * `removeIfCycle` / `getConnectionItems` / trace-tail removal (§10.3-10.4) —
//!   the ripup unit, which belongs with the router.
//! * `reduceNetsOfRouteItems` (§10.5) — a board-load pass, which belongs with
//!   the importer.

use std::collections::BTreeSet;

use fr_geometry::planar::{IntOctagon, IntPoint, IntVector, Point, Polyline};

use crate::clearance::{ClearanceSource, ClearanceTable};
use crate::connectivity::{self, Connectivity};
use crate::item::{FixedState, Item, ItemId, ItemKind, PolylineTrace, Via};
use crate::journal::Mark;
use crate::net_nos::NetNos;
use crate::search_tree::{
    SearchTree, SearchTreeManager, ShapeContext, TreeEntry, MAX_TREE_SHAPE_WIDTH,
};
use crate::shape::{AngleRestriction, PadstackTable, TileShape};
use crate::store::{ItemStore, StoreDelta};

/// Upstream `BasicBoard.maxTraceHalfWidth`'s initial value
/// (`BasicBoard.java:106-109`) — deliberately *small*, so the first inserted
/// trace raises it.
const INITIAL_MAX_TRACE_HALF_WIDTH: i32 = 1000;

/// Upstream `BasicBoard.minTraceHalfWidth`'s initial value — deliberately
/// *large*, so the first inserted trace lowers it.
const INITIAL_MIN_TRACE_HALF_WIDTH: i32 = 10_000;

/// One board layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layer {
    /// The layer's name as it appeared in the DSN.
    pub name: String,
    /// False for a power/ground plane. Only signal layers are routable, and a
    /// conduction area on a non-signal layer cannot be deleted
    /// (`Item.isDeletionForbidden`, §9.1).
    pub is_signal: bool,
}

impl Layer {
    /// A signal layer.
    pub fn signal(name: impl Into<String>) -> Layer {
        Layer {
            name: name.into(),
            is_signal: true,
        }
    }

    /// A plane layer.
    pub fn plane(name: impl Into<String>) -> Layer {
        Layer {
            name: name.into(),
            is_signal: false,
        }
    }
}

/// The board's layer stack, indexed from 0 (front) upwards.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LayerStructure {
    layers: Vec<Layer>,
}

impl LayerStructure {
    /// A stack from an explicit layer list.
    pub fn new(layers: Vec<Layer>) -> Self {
        LayerStructure { layers }
    }

    /// `count` signal layers named `signal0 … signal{count-1}` — the shape
    /// almost every fixture wants.
    pub fn signal_layers(count: usize) -> Self {
        LayerStructure::new(
            (0..count)
                .map(|i| Layer::signal(format!("signal{i}")))
                .collect(),
        )
    }

    /// Number of layers.
    #[inline]
    pub fn len(&self) -> usize {
        self.layers.len()
    }

    /// True when the stack is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    /// The layer at `index`, or `None` when out of range.
    pub fn get(&self, index: i32) -> Option<&Layer> {
        usize::try_from(index).ok().and_then(|i| self.layers.get(i))
    }

    /// The layer's name.
    pub fn name(&self, index: i32) -> Option<&str> {
        self.get(index).map(|layer| layer.name.as_str())
    }

    /// Whether `index` is a signal layer.
    ///
    /// An out-of-range index answers `true`, which is the permissive direction:
    /// it makes `isDeletionForbidden` treat an unknown layer as ordinary
    /// copper rather than silently protecting an item on it.
    // `Option::is_none_or` would read better but needs Rust 1.82; the
    // workspace MSRV is 1.75 (see `store.rs` for the same note).
    #[allow(clippy::unnecessary_map_or)]
    pub fn is_signal(&self, index: i32) -> bool {
        self.get(index).map_or(true, |layer| layer.is_signal)
    }

    /// Every layer with its index.
    pub fn iter(&self) -> impl Iterator<Item = (i32, &Layer)> {
        self.layers
            .iter()
            .enumerate()
            .map(|(i, layer)| (i as i32, layer))
    }
}

/// A transaction handle: the store's [`Mark`] plus the board's own
/// touched-item watermark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoardMark {
    store: Mark,
    touched: usize,
}

/// Reusable id buffers, so the split/combine/normalize paths allocate nothing
/// once warm.
///
/// They are handed out by `mem::take` and handed back
/// ([`Board::take_scratch_items`]): a `&mut Board` method that owns the buffer
/// outright can still call other `&mut self` methods, which a borrowed buffer
/// would forbid. Two buffers because split nests one level (the pieces of the
/// trace being split, and the candidates found for one of its segments).
#[derive(Debug, Clone, Default)]
struct Scratch {
    items: Vec<ItemId>,
    ids: Vec<ItemId>,
}

/// The routing board.
#[derive(Debug, Clone)]
pub struct Board {
    store: ItemStore,
    layers: LayerStructure,
    clearances: ClearanceTable,
    padstacks: PadstackTable,
    trees: SearchTreeManager,
    connectivity: Connectivity,
    angle_restriction: AngleRestriction,
    max_tree_shape_width: f64,
    max_trace_half_width: i32,
    min_trace_half_width: i32,
    /// §11.6's oscillation valve: once a net trips
    /// [`MAX_NORMALIZE_ITERATIONS`](crate::trace_ops::MAX_NORMALIZE_ITERATIONS),
    /// every later `normalize_traces` for it returns immediately, for the
    /// lifetime of the board.
    normalize_suppressed: BTreeSet<i32>,
    /// Items touched since the outermost open transaction began.
    touched: Vec<ItemId>,
    scratch: Scratch,
}

impl Board {
    /// A board with the given layer stack and clearance matrix, and no items.
    pub fn new(layers: LayerStructure, clearances: ClearanceTable) -> Self {
        Board {
            store: ItemStore::new(),
            layers,
            clearances,
            padstacks: PadstackTable::new(),
            trees: SearchTreeManager::new(),
            connectivity: Connectivity::new(),
            angle_restriction: AngleRestriction::default(),
            max_tree_shape_width: MAX_TREE_SHAPE_WIDTH,
            max_trace_half_width: INITIAL_MAX_TRACE_HALF_WIDTH,
            min_trace_half_width: INITIAL_MIN_TRACE_HALF_WIDTH,
            normalize_suppressed: BTreeSet::new(),
            touched: Vec::new(),
            scratch: Scratch::default(),
        }
    }

    // ---- accessors -------------------------------------------------------

    /// The item store.
    #[inline]
    pub fn store(&self) -> &ItemStore {
        &self.store
    }

    /// The layer stack.
    #[inline]
    pub fn layers(&self) -> &LayerStructure {
        &self.layers
    }

    /// The clearance matrix.
    #[inline]
    pub fn clearances(&self) -> &ClearanceTable {
        &self.clearances
    }

    /// The padstack table, for filling at board load.
    #[inline]
    pub fn padstacks_mut(&mut self) -> &mut PadstackTable {
        &mut self.padstacks
    }

    /// The padstack table.
    #[inline]
    pub fn padstacks(&self) -> &PadstackTable {
        &self.padstacks
    }

    /// The search-tree registry.
    #[inline]
    pub fn trees(&self) -> &SearchTreeManager {
        &self.trees
    }

    /// The default (interactive) search tree — upstream's
    /// `BasicBoard.search_tree_manager.get_default_tree()`.
    #[inline]
    pub fn default_tree(&self) -> &SearchTree {
        self.trees.default_tree()
    }

    /// The connectivity index.
    #[inline]
    pub fn connectivity(&self) -> &Connectivity {
        &self.connectivity
    }

    /// The connectivity index, for the lazily-rebuilding queries.
    #[inline]
    pub fn connectivity_mut(&mut self) -> &mut Connectivity {
        &mut self.connectivity
    }

    /// Number of connected components of `net_no`, rebuilding the net if it is
    /// dirty. One means fully connected.
    pub fn net_component_count(&mut self, net_no: i32) -> usize {
        let Board {
            store,
            connectivity,
            ..
        } = self;
        connectivity.component_count(net_no, store)
    }

    /// The board-wide trace angle restriction.
    #[inline]
    pub fn angle_restriction(&self) -> AngleRestriction {
        self.angle_restriction
    }

    /// The cap on one indexed tile shape (`search-tree-obstacles.md` §3.5).
    #[inline]
    pub fn max_tree_shape_width(&self) -> f64 {
        self.max_tree_shape_width
    }

    /// The context tile-shape computation runs in — the same one every board
    /// mutation hands the tree manager.
    ///
    /// Public so that a differential test can recompute an item's shapes from
    /// scratch and compare them against what the trees actually hold.
    pub fn shape_ctx(&self) -> ShapeContext<'_> {
        Board::shape_context(
            &self.clearances,
            &self.padstacks,
            &self.layers,
            self.angle_restriction,
            self.max_tree_shape_width,
        )
    }

    /// Sets the angle restriction and rebuilds every tree, since it decides the
    /// tile-shape flavor of drill items and areas (§3.4).
    pub fn set_angle_restriction(&mut self, restriction: AngleRestriction) {
        if restriction == self.angle_restriction {
            return;
        }
        self.angle_restriction = restriction;
        self.rebuild_trees();
    }

    /// Largest trace half width inserted so far (upstream
    /// `BasicBoard.maxTraceHalfWidth`).
    #[inline]
    pub fn max_trace_half_width(&self) -> i32 {
        self.max_trace_half_width
    }

    /// Smallest trace half width inserted so far.
    #[inline]
    pub fn min_trace_half_width(&self) -> i32 {
        self.min_trace_half_width
    }

    /// True when `net_no` tripped the normalize iteration cap and is now
    /// permanently skipped (§11.6, `MAX_NORMALIZE_ITERATIONS`).
    pub fn is_normalize_suppressed(&self, net_no: i32) -> bool {
        self.normalize_suppressed.contains(&net_no)
    }

    /// Upstream `Item.isDeletionForbidden()`, with the layer structure supplied.
    pub fn is_deletion_forbidden(&self, id: ItemId) -> bool {
        self.store
            .get(id)
            .is_some_and(|item| item.is_deletion_forbidden(|layer| self.layers.is_signal(layer)))
    }

    // ---- the mutation protocol (§11.2) -----------------------------------

    /// The context tile-shape computation needs, borrowed field by field so it
    /// can coexist with a `&mut` borrow of the trees.
    fn shape_context<'a>(
        clearances: &'a ClearanceTable,
        padstacks: &'a PadstackTable,
        layers: &LayerStructure,
        angle_restriction: AngleRestriction,
        max_tree_shape_width: f64,
    ) -> ShapeContext<'a> {
        ShapeContext {
            clearances,
            padstacks,
            layer_count: layers.len(),
            angle_restriction,
            max_tree_shape_width,
        }
    }

    fn index_in_trees(&mut self, id: ItemId) {
        let Board {
            store,
            trees,
            clearances,
            padstacks,
            layers,
            angle_restriction,
            max_tree_shape_width,
            ..
        } = self;
        let (Some(slot), Some(item)) = (store.slot_of(id), store.get(id)) else {
            return;
        };
        let ctx = Board::shape_context(
            clearances,
            padstacks,
            layers,
            *angle_restriction,
            *max_tree_shape_width,
        );
        trees.insert(slot, item, ctx);
    }

    fn unindex_from_trees(&mut self, id: ItemId) {
        if let Some(slot) = self.store.slot_of(id) {
            self.trees.remove(slot, id);
        }
    }

    fn mark_nets_dirty(&mut self, id: ItemId) {
        let Board {
            store,
            connectivity,
            ..
        } = self;
        if let Some(item) = store.get(id) {
            connectivity.mark_item_dirty(item);
        }
    }

    fn note_touched(&mut self, id: ItemId) {
        if self.store.transaction_depth() > 0 {
            self.touched.push(id);
        }
    }

    /// Inserts an item, upstream `BoardItemRepository.insertItem` (`:135-167`).
    ///
    /// A clearance class outside the matrix is forced to 0, exactly as upstream
    /// does (it warns; this crate has no logger yet).
    pub fn insert_item(&mut self, mut item: Item) -> ItemId {
        if item.clearance_class < 0 || item.clearance_class >= self.clearances.class_count() as i32
        {
            item.clearance_class = 0;
        }
        let id = self.store.insert(item);
        self.note_touched(id);
        self.index_in_trees(id);
        self.mark_nets_dirty(id);
        id
    }

    /// Removes an item, upstream `BoardItemRepository.removeItem` (`:170-201`).
    ///
    /// **Silently does nothing** when the item's deletion is forbidden — the
    /// no-op is upstream's, and several callers depend on it (§11.4's split
    /// guard, §11.6's degenerate-trace skip).
    pub fn remove_item(&mut self, id: ItemId) -> bool {
        if self.is_deletion_forbidden(id) {
            return false;
        }
        self.remove_item_unchecked(id)
    }

    /// Removes an item without consulting `isDeletionForbidden`.
    ///
    /// Only for call sites that have already made the check; keeping it
    /// `pub(crate)` is what stops the guard from being skipped by accident.
    pub(crate) fn remove_item_unchecked(&mut self, id: ItemId) -> bool {
        if !self.store.contains(id) {
            return false;
        }
        self.mark_nets_dirty(id);
        self.note_touched(id);
        self.unindex_from_trees(id);
        self.store.remove(id)
    }

    /// Replaces a trace's polyline in place, keeping its identity and
    /// **reusing** the search-tree leaves and tile shapes of every segment that
    /// did not move — upstream's `PolylineTrace.change(Polyline)` (`:942-1009`)
    /// plus the `ShapeSearchTree.change_entries` it delegates to.
    ///
    /// The naive version of this is remove-from-the-trees → mutate →
    /// re-insert, which re-offsets every segment of the trace on every call.
    /// What this does instead:
    ///
    /// * the [`ItemId`] does not change, so no caller has to revalidate ids —
    ///   **and the board can see that it did not**, see "This is not
    ///   result-neutral" below;
    /// * the two line arrays are compared end-inwards to find the longest equal
    ///   prefix and suffix; `k` equal leading lines yield up to `k - 2` reusable
    ///   leading shapes and their leaves are never touched (`:983-987`). Each
    ///   tree then narrows that further to the shapes whose dog-ear scan cannot
    ///   have read a line that moved — see
    ///   `dogear_safe_keep_at_start` in [`search_tree`](crate::search_tree), which is a
    ///   correctness fix over upstream's rule, not an optimization;
    /// * only the segments in between are re-offset and re-inserted;
    /// * the pre-image reaches the journal **once per open transaction**
    ///   (`store.mutate` → `UndoableObjects.save_for_undo`, whose
    ///   `curr_node.level < stack_level` guard is exactly that rule), so a
    ///   pull-tight that moves one trace a hundred times inside one transaction
    ///   clones it once, not a hundred times;
    /// * connectivity is dirtied **only when an end corner moved**. A trace's
    ///   connection points are its two end corners ([`connectivity`]), so a
    ///   move that leaves both where they were cannot change any net's
    ///   components, and the whole-net union-find rebuild it would otherwise
    ///   have forced is skipped.
    ///
    /// Returns `false` — having changed nothing at all — when `id` is not a
    /// trace or when the polyline is already the one asked for (upstream's
    /// "both polylines are equal, no change necessary", `:970`).
    ///
    /// # This is not result-neutral, and it was never going to be
    ///
    /// Two independent things landed here at once, and only one of them is an
    /// optimization. Keeping them apart is the difference between a defect and
    /// a parity fix:
    ///
    /// 1. **The leaf and shape reuse is index-neutral.** The set of shapes
    ///    indexed, the item each belongs to, and the order every query reports
    ///    them in are the same as a full rebuild's; only *which arena leaves*
    ///    hold them differs, and no query can observe that, because
    ///    [`SearchTree::overlapping_tree_entries`](crate::search_tree::SearchTree::overlapping_tree_entries)
    ///    sorts by `(descending item id, ascending shape index)` explicitly
    ///    rather than reporting traversal order. This is not an assertion:
    ///    `FR_CHANGE_ENTRIES=rebuild` (see
    ///    [`CHANGE_ENTRIES_ENV`](crate::search_tree::CHANGE_ENTRIES_ENV))
    ///    forces the rebuild path on a real board, and the session file must
    ///    come back byte-identical.
    ///
    /// 2. **Keeping the id is board-visible, on purpose.** Before this existed,
    ///    a pull-tight was `remove_item` + `insert_trace_without_cleaning`, so
    ///    the moved trace came back with a *fresh, maximal* id. Item id is not
    ///    decoration here: `ItemStore::iter` walks **descending id**, and so
    ///    does every obstacle list a query returns. A trace that had just been
    ///    pulled tight therefore jumped to the front of every obstacle list and
    ///    to the front of the store scan — which decides which obstacle the
    ///    shove engine tries first, which item the optimizer sweep picks, and
    ///    which connection is ripped up. Now it keeps its place.
    ///
    ///    That is upstream's behaviour, not a choice: `PolylineTrace.change`
    ///    assigns `lines = p_new_polyline` on the *same object*, so `get_id_no()`
    ///    is untouched (`:942-1009`). The remove + insert was the divergence.
    ///
    /// So a board's routed result can and does move across this change. It is a
    /// **re-roll of a chaotic search under a corrected traversal order**, not
    /// drift from a stale index, and the two are told apart by (1) — not by
    /// whether the new number is better. Issue110-RelayModule is the worked
    /// example, and it is worked all the way through in the module docs of
    /// [`crate::search_tree`]: re-arming the id churn alone reproduces the old
    /// session byte for byte, so the ordering is the whole of the difference.
    ///
    /// > **One upstream quirk is deliberately not reproduced.** `:966-978`
    /// > compares lines by *reference*, and bails out of `change` entirely when
    /// > the scan runs off the end of the shorter array — which silently refuses
    /// > a new polyline that is a strict prefix or suffix of the old one. Here
    /// > the equal run is simply clamped to the shorter array and the change is
    /// > applied, which is what this port has always done through
    /// > `replace_trace_polyline`. Value comparison also finds *longer* equal
    /// > runs than Java's identity comparison does; that only ever reuses more
    /// > leaves, and equal lines produce equal shapes, so the index is the same.
    pub fn change_trace_polyline(&mut self, id: ItemId, polyline: Polyline) -> bool {
        let Some(item) = self.store.get(id) else {
            return false;
        };
        let ItemKind::Trace(trace) = &item.kind else {
            return false;
        };
        let Some((keep_at_start, keep_at_end)) =
            kept_shape_counts(&trace.polyline.arr, &polyline.arr)
        else {
            return false;
        };
        let endpoints_moved = trace.polyline.first_corner() != polyline.first_corner()
            || trace.polyline.last_corner() != polyline.last_corner();

        self.note_touched(id);
        self.store.mutate(id, move |item| {
            if let ItemKind::Trace(trace) = &mut item.kind {
                trace.polyline = polyline;
            }
        });

        let Board {
            store,
            trees,
            clearances,
            padstacks,
            layers,
            angle_restriction,
            max_tree_shape_width,
            ..
        } = self;
        if let (Some(slot), Some(item)) = (store.slot_of(id), store.get(id)) {
            let ctx = Board::shape_context(
                clearances,
                padstacks,
                layers,
                *angle_restriction,
                *max_tree_shape_width,
            );
            trees.change_trace_entries(slot, item, keep_at_start, keep_at_end, ctx);
        }

        if endpoints_moved {
            self.mark_nets_dirty(id);
        }
        true
    }

    /// Translates a via or pin in place, keeping its identity — upstream's
    /// `DrillItem.move_by(Vector)` (`Item.java`), which is how the shove engine
    /// pushes a via out of the way ([`shove::drill`](crate::shove::drill)).
    ///
    /// Same mandatory order as the trace-polyline replacement: out of the
    /// trees, mutate, back in. Returns false for anything that is not a drill
    /// item, or for a translation that would leave the integer coordinate
    /// range.
    pub fn translate_drill_item(&mut self, id: ItemId, vector: &IntVector) -> bool {
        let Some(item) = self.store.get(id) else {
            return false;
        };
        let new_center = match &item.kind {
            ItemKind::Via(via) => via.center.translate_by_int(vector),
            ItemKind::Pin(pin) => pin.center.translate_by_int(vector),
            _ => return false,
        };
        self.note_touched(id);
        self.unindex_from_trees(id);
        self.store.mutate(id, move |item| match &mut item.kind {
            ItemKind::Via(via) => via.center = new_center,
            ItemKind::Pin(pin) => pin.center = new_center,
            _ => {}
        });
        self.index_in_trees(id);
        self.mark_nets_dirty(id);
        true
    }

    /// Rebuilds every search tree from the store.
    pub fn rebuild_trees(&mut self) {
        let Board {
            store,
            trees,
            clearances,
            padstacks,
            layers,
            angle_restriction,
            max_tree_shape_width,
            ..
        } = self;
        let ctx = Board::shape_context(
            clearances,
            padstacks,
            layers,
            *angle_restriction,
            *max_tree_shape_width,
        );
        trees.rebuild(store, ctx);
    }

    /// Creates the compensated tree for `clearance_class` if it does not exist,
    /// upstream's `getAutorouteTree`.
    pub fn ensure_compensated_tree(&mut self, clearance_class: i32) {
        let Board {
            store,
            trees,
            clearances,
            padstacks,
            layers,
            angle_restriction,
            max_tree_shape_width,
            ..
        } = self;
        let ctx = Board::shape_context(
            clearances,
            padstacks,
            layers,
            *angle_restriction,
            *max_tree_shape_width,
        );
        trees.ensure_compensated_tree(clearance_class, store, ctx);
        // `SearchTreeManager.java:160-172`: which subclass `getAutorouteTree`
        // constructs is the board's angle restriction's decision. The 45° family
        // is a *second* tree rather than a replacement, because the any-angle one
        // is what shove, pull-tight and insert read; it is therefore built only
        // when the chain is switched on (`search_tree::TILING_45_ENV`).
        if *angle_restriction == AngleRestriction::FortyFive
            && crate::search_tree::tiling_45_mode().is_on()
        {
            trees.ensure_compensated_tree_with_family(
                clearance_class,
                crate::search_tree::TreeAngleFamily::FortyFive,
                store,
                ctx,
            );
        } else if *angle_restriction == AngleRestriction::Ninety {
            trees.ensure_compensated_tree_with_family(
                clearance_class,
                crate::search_tree::TreeAngleFamily::Ninety,
                store,
                ctx,
            );
        }
    }

    /// Turns clearance compensation of the default tree on or off, upstream's
    /// `set_clearance_compensation_used`.
    pub fn set_clearance_compensation_used(&mut self, used: bool) {
        let Board {
            store,
            trees,
            clearances,
            padstacks,
            layers,
            angle_restriction,
            max_tree_shape_width,
            ..
        } = self;
        let ctx = Board::shape_context(
            clearances,
            padstacks,
            layers,
            *angle_restriction,
            *max_tree_shape_width,
        );
        trees.set_clearance_compensation_used(used, store, ctx);
    }

    // ---- snapshot adoption (state mechanism 2) ---------------------------

    /// Replaces the board's items with `store`, reconciling the search trees and
    /// the connectivity index from the difference — the [`Board`]-level form of
    /// [`BoardHistory::restore_back`](crate::history::BoardHistory::restore_back).
    ///
    /// This is `ARCHITECTURE.md`'s state mechanism 2 as the batch loop needs it:
    ///
    /// > 2. board history = arena snapshot (memcpy of item+node Vecs), O(board)
    /// >    but ~ms; restore must also restore tree state and preserves the room
    /// >    DB interaction
    ///
    /// A caller that holds a pooled snapshot of the items — and *only* the items
    /// — can put it back without cloning a whole [`Board`] per retained pass.
    /// The returned [`StoreDelta`] is what the router's own dependent state
    /// (the autoroute room database and its per-item caches) subscribes to; the
    /// trees and the connectivity index are already reconciled when it lands.
    ///
    /// # Cost
    ///
    /// One merge walk over the two ordered indices plus a tile-shape rebuild for
    /// the items that actually moved — *not* [`rebuild_trees`](Board::rebuild_trees),
    /// which is O(board) in shape computation. On a pass that changed a hundred
    /// traces on a twenty-thousand-item board that is a hundred rebuilds, not
    /// twenty thousand.
    ///
    /// # What is *not* restored
    ///
    /// [`max_trace_half_width`](Board::max_trace_half_width) and its minimum
    /// counterpart are high-water marks fed by every trace ever inserted, and
    /// they stay at the live board's values rather than rewinding to the
    /// snapshot's. Both only ever *widen* a tree query, so keeping the larger
    /// envelope is the conservative direction; upstream never rewinds them
    /// either, because its restore constructs a fresh board and recomputes them
    /// from the restored items — which cannot be higher than what we keep.
    ///
    /// # Panics
    ///
    /// If a transaction is open on either store: the journal's pre-images would
    /// outlive the state they describe (`history`, module docs).
    pub fn adopt_store(&mut self, store: ItemStore) -> StoreDelta {
        let mut incoming = store;
        let mut delta = StoreDelta::new();
        self.swap_store(&mut incoming, &mut delta);
        delta
    }

    /// [`adopt_store`](Board::adopt_store) that hands the *displaced* store back
    /// through the same reference and reuses the caller's [`StoreDelta`].
    ///
    /// The allocation-free form: a snapshot store scratch buffer goes in, the
    /// board's previous items come out in it, and the caller pools the buffer
    /// for the next snapshot. `delta` is cleared first.
    pub fn swap_store(&mut self, store: &mut ItemStore, delta: &mut StoreDelta) {
        assert_eq!(
            self.store.transaction_depth(),
            0,
            "fr-board: cannot adopt a store while a transaction is open on the board"
        );
        assert_eq!(
            store.transaction_depth(),
            0,
            "fr-board: cannot adopt a store that has an open transaction"
        );
        // Slot-aware, because the search trees are slot-indexed: see
        // `ItemStore::diff_into_relocating`.
        self.store.diff_into_relocating(store, delta);

        // Detach everything that is about to move, while the *old* store still
        // resolves its slot — the `remove_item` half of the mutation protocol.
        for id in delta.removed.iter().chain(delta.changed.iter()).copied() {
            self.mark_nets_dirty(id);
            self.unindex_from_trees(id);
        }

        let (next_id, next_revision) = self.store.raw_counters();
        std::mem::swap(&mut self.store, store);
        // The adopted state carries the counters it was taken with; ids and
        // revisions must never move backwards, or a search tree that outlived
        // the swap would see two different items under one identity.
        self.store.raise_counters(next_id, next_revision);
        self.touched.clear();

        // Re-index against the new store — the `insert_item` half.
        for id in delta.added.iter().chain(delta.changed.iter()).copied() {
            self.index_in_trees(id);
            self.mark_nets_dirty(id);
        }
    }

    // ---- transactions ----------------------------------------------------

    /// Opens a transaction over the board — the store *and* everything derived
    /// from it. O(1).
    pub fn begin(&mut self) -> BoardMark {
        BoardMark {
            store: self.store.begin(),
            touched: self.touched.len(),
        }
    }

    /// True when any item on the board was modified, created, or deleted since `mark`.
    #[inline]
    pub fn touched_since(&self, mark: &BoardMark) -> bool {
        self.touched.len() > mark.touched
    }

    /// Accepts everything done since `mark`. O(1).
    pub fn commit(&mut self, mark: BoardMark) {
        self.store.commit(mark.store);
        if self.store.transaction_depth() == 0 {
            self.touched.clear();
        }
    }

    /// Undoes everything done since `mark`, board-wide: items, search trees and
    /// connectivity.
    ///
    /// O(items touched since the mark). The trees are resynchronized rather
    /// than journalled: they are a pure function of the store, so "detach the
    /// touched items, roll the store back, re-index whatever is live" restores
    /// them exactly, and a touched item's slot is guaranteed to be free of
    /// stale tree state because the detach cleared it.
    pub fn rollback_to(&mut self, mark: BoardMark) {
        let mut touched = self.touched.split_off(mark.touched);
        touched.sort_unstable();
        touched.dedup();

        for id in &touched {
            self.mark_nets_dirty(*id);
            self.unindex_from_trees(*id);
        }
        self.store.rollback_to(mark.store);
        for id in &touched {
            if self.store.contains(*id) {
                self.index_in_trees(*id);
                self.mark_nets_dirty(*id);
            }
        }
        if self.store.transaction_depth() == 0 {
            self.touched.clear();
        }
    }

    /// Number of open transactions.
    #[inline]
    pub fn transaction_depth(&self) -> usize {
        self.store.transaction_depth()
    }

    // ---- insertion entry points (§11.1, §11.3) ---------------------------

    /// Upstream `BasicBoard.insertTraceWithoutCleaning` (`:178-202`) — the raw
    /// entry point, used by `split()`, by shove, and by the connection
    /// inserter.
    ///
    /// Returns `None` without inserting anything when
    ///
    /// 1. the polyline has fewer than two corners, or
    /// 2. it is **degenerate** (first corner equals last) and the fixed state is
    ///    below [`FixedState::UserFixed`]. A `USER_FIXED` zero-length trace *is*
    ///    inserted — which is exactly why normalization needs the matching skip
    ///    (§11.6).
    #[allow(clippy::too_many_arguments)]
    pub fn insert_trace_without_cleaning(
        &mut self,
        polyline: Polyline,
        layer: i32,
        half_width: i32,
        nets: NetNos,
        clearance_class: i32,
        fixed_state: FixedState,
    ) -> Option<ItemId> {
        if polyline.corner_count() < 2 {
            return None;
        }
        let trace = PolylineTrace {
            polyline,
            layer,
            half_width,
        };
        if trace.first_corner() == trace.last_corner() && fixed_state < FixedState::UserFixed {
            return None;
        }
        let item = Item::new(nets, clearance_class, fixed_state, ItemKind::Trace(trace));
        let nets_normal = item.nets_normal();
        let id = self.insert_item(item);
        if nets_normal {
            self.max_trace_half_width = self.max_trace_half_width.max(half_width);
            self.min_trace_half_width = self.min_trace_half_width.min(half_width);
        }
        Some(id)
    }

    /// Upstream `BasicBoard.insertTrace` (`:209-242`) — insert, then normalize
    /// against `clip`.
    ///
    /// `clip` is upstream's `changedArea.getArea(layer)`; this crate has no
    /// `ChangedArea` yet, so it is a parameter (`None` = normalize against the
    /// whole board).
    #[allow(clippy::too_many_arguments)]
    pub fn insert_trace(
        &mut self,
        polyline: Polyline,
        layer: i32,
        half_width: i32,
        nets: NetNos,
        clearance_class: i32,
        fixed_state: FixedState,
        clip: Option<&IntOctagon>,
    ) -> Option<ItemId> {
        let id = self.insert_trace_without_cleaning(
            polyline,
            layer,
            half_width,
            nets,
            clearance_class,
            fixed_state,
        )?;
        self.normalize_trace(id, clip);
        Some(id)
    }

    /// Upstream's convenience overload: corners → polyline →
    /// [`insert_trace`](Board::insert_trace).
    #[allow(clippy::too_many_arguments)]
    pub fn insert_trace_points(
        &mut self,
        corners: &[IntPoint],
        layer: i32,
        half_width: i32,
        nets: NetNos,
        clearance_class: i32,
        fixed_state: FixedState,
        clip: Option<&IntOctagon>,
    ) -> Option<ItemId> {
        self.insert_trace(
            Polyline::from_int_points(corners),
            layer,
            half_width,
            nets,
            clearance_class,
            fixed_state,
            clip,
        )
    }

    /// Upstream `BasicBoard.insertVia` (`:268-295`): insert the via, then split
    /// same-net traces at its centre.
    ///
    /// > **The layer range is HALF-OPEN.** Upstream loops
    /// > `for layer in padstack.fromLayer() .. padstack.toLayer()`, so a trace
    /// > on the via's **last** layer is *not* split. The `<` is inherited
    /// > verbatim from `v19:BasicBoard.java:306`; §11.3 says to preserve it, and
    /// > [`insert_escape_via`](Board::insert_escape_via) is the closed-interval
    /// > counterpart.
    pub fn insert_via(
        &mut self,
        via: Via,
        nets: NetNos,
        clearance_class: i32,
        fixed_state: FixedState,
    ) -> ItemId {
        self.insert_via_impl(via, nets, clearance_class, fixed_state, false)
    }

    /// Upstream `BasicBoard.insertEscapeVia` (`:310-330`, v2 only) — as
    /// [`insert_via`](Board::insert_via) but splitting over the **closed**
    /// layer interval, `fromLayer ..= toLayer`.
    pub fn insert_escape_via(
        &mut self,
        via: Via,
        nets: NetNos,
        clearance_class: i32,
        fixed_state: FixedState,
    ) -> ItemId {
        self.insert_via_impl(via, nets, clearance_class, fixed_state, true)
    }

    fn insert_via_impl(
        &mut self,
        via: Via,
        nets: NetNos,
        clearance_class: i32,
        fixed_state: FixedState,
        closed_layer_range: bool,
    ) -> ItemId {
        let center = via.center;
        let layers = via.layers;
        let split_nets = nets.clone();
        let id = self.insert_item(Item::new(
            nets,
            clearance_class,
            fixed_state,
            ItemKind::Via(via),
        ));

        let last = if closed_layer_range {
            layers.last
        } else {
            layers.last - 1
        };
        let center = Point::Int(center);
        for layer in layers.first..=last {
            for net in split_nets.iter().copied() {
                self.split_traces(&center, layer, net);
            }
        }
        id
    }

    /// Upstream `BasicBoard.insertPin` (`:336-346`) — note that inserting a pin
    /// **does not** split traces.
    pub fn insert_pin(
        &mut self,
        pin: crate::item::Pin,
        nets: NetNos,
        clearance_class: i32,
    ) -> ItemId {
        self.insert_item(Item::new(
            nets,
            clearance_class,
            FixedState::SystemFixed,
            ItemKind::Pin(pin),
        ))
    }

    // ---- queries ---------------------------------------------------------

    /// Every item whose indexed shape covers `location`, upstream's
    /// `BasicBoard.pickItems` (`:1181-1194`). Descending id order.
    ///
    /// `layer` of `None` is upstream's `-1`, "any layer".
    pub fn pick_items(&self, location: &Point, layer: Option<i32>, out: &mut Vec<ItemId>) {
        let mut stack = fr_datastructures::arena_shape_tree::QueryStack::new();
        self.trees.default_tree().overlapping_items(
            &self.store,
            &TileShape::from_point(location),
            layer,
            &[],
            &mut stack,
            out,
        );
    }

    /// Items overlapping `shape` on `layer`, upstream's
    /// `BasicBoard.overlappingObjects`. Descending id order.
    pub fn overlapping_items(
        &self,
        shape: &TileShape,
        layer: Option<i32>,
        ignore_nets: &[i32],
        out: &mut Vec<ItemId>,
    ) {
        let mut stack = fr_datastructures::arena_shape_tree::QueryStack::new();
        self.trees.default_tree().overlapping_items(
            &self.store,
            shape,
            layer,
            ignore_nets,
            &mut stack,
            out,
        );
    }

    /// Tree entries overlapping `shape` on `layer`, sorted descending by id
    /// then ascending by shape index.
    pub fn overlapping_tree_entries(
        &self,
        shape: &TileShape,
        layer: Option<i32>,
        ignore_nets: &[i32],
        out: &mut Vec<TreeEntry>,
    ) {
        let mut stack = fr_datastructures::arena_shape_tree::QueryStack::new();
        self.trees.default_tree().overlapping_tree_entries(
            &self.store,
            shape,
            layer,
            ignore_nets,
            &mut stack,
            out,
        );
    }

    /// Upstream `Item.getNormalContacts(point)` — the **strict** connection
    /// predicate (`drc-connectivity.md` §4.3).
    ///
    /// Items sharing a net *and* a layer with `id` that have a connection point
    /// exactly at `point`: a trace end corner, a drill centre, or a conduction
    /// area containing it. Exact integer equality, no tolerance.
    ///
    /// `out` comes back in descending id order.
    pub fn normal_contacts_at(&self, id: ItemId, point: &Point, out: &mut Vec<ItemId>) {
        out.clear();
        let Some(item) = self.store.get(id) else {
            return;
        };
        let mut candidates = Vec::new();
        self.pick_items(point, None, &mut candidates);
        for candidate in candidates {
            if candidate == id {
                continue;
            }
            let Some(other) = self.store.get(candidate) else {
                continue;
            };
            if !other.shares_net(item)
                || !connectivity::shares_layer(item, other)
                || !connectivity::touches_at(other, point)
            {
                continue;
            }
            out.push(candidate);
        }
    }

    // ---- internals shared with `trace_ops` -------------------------------

    pub(crate) fn take_scratch_items(&mut self) -> Vec<ItemId> {
        std::mem::take(&mut self.scratch.items)
    }

    pub(crate) fn put_scratch_items(&mut self, mut buffer: Vec<ItemId>) {
        buffer.clear();
        self.scratch.items = buffer;
    }

    pub(crate) fn take_scratch_ids(&mut self) -> Vec<ItemId> {
        std::mem::take(&mut self.scratch.ids)
    }

    pub(crate) fn put_scratch_ids(&mut self, mut buffer: Vec<ItemId>) {
        buffer.clear();
        self.scratch.ids = buffer;
    }

    pub(crate) fn normalize_suppressed_mut(&mut self) -> &mut BTreeSet<i32> {
        &mut self.normalize_suppressed
    }

    /// Panics unless the store, every search tree and the layer stack agree.
    ///
    /// O(board), allocating; for tests and `debug_assert`-style use.
    pub fn assert_consistent(&self) {
        self.store.assert_consistent();
        self.trees.assert_consistent(&self.store);
    }
}

/// How many tile shapes of `old` survive unchanged at each end when its
/// polyline becomes `new` — upstream's `index_of_first_different_line` /
/// `index_of_last_different_line` arithmetic (`PolylineTrace.java:966-987`).
///
/// `None` means the two line arrays are identical, upstream's "no change
/// necessary".
///
/// The `- 2` on each side is the rule: tile shape `s` is offset from lines `s`,
/// `s + 1` and `s + 2`, so `k` equal leading lines pin down `k - 2` leading
/// shapes and `m` equal trailing lines pin down `m - 2` trailing ones.
///
/// It is an **upper bound**, not the answer. Nothing here clamps the two
/// against each other, and nothing here knows about the dog-ear scan that makes
/// a tile shape depend on more than its own three lines; both are
/// [`SearchTree::change_trace_entries`](crate::search_tree::SearchTree::change_trace_entries)'s
/// job, because both need the shape counts and the tree's offset width.
fn kept_shape_counts(
    old: &[fr_geometry::planar::Line],
    new: &[fr_geometry::planar::Line],
) -> Option<(usize, usize)> {
    let limit = old.len().min(new.len());
    let mut prefix = limit;
    for i in 0..limit {
        if old[i] != new[i] {
            prefix = i;
            break;
        }
    }
    let mut suffix = limit;
    for i in 1..=limit {
        if old[old.len() - i] != new[new.len() - i] {
            suffix = i - 1;
            break;
        }
    }
    if old.len() == new.len() && prefix == limit {
        return None;
    }
    Some((prefix.saturating_sub(2), suffix.saturating_sub(2)))
}

#[cfg(test)]
mod tests {
    use fr_geometry::planar::IntBox;

    use super::*;
    use crate::item::LayerRange;
    use crate::shape::{Padstack, TileShape as Shape};

    pub(crate) fn board() -> Board {
        let mut board = Board::new(
            LayerStructure::signal_layers(3),
            ClearanceTable::uniform(3, 3, 20),
        );
        board.padstacks_mut().push(Padstack::uniform(
            0,
            2,
            Shape::Box(IntBox::new(-40, -40, 40, 40)),
        ));
        board
    }

    fn straight(board: &mut Board, net: i32, layer: i32, from: IntPoint, to: IntPoint) -> ItemId {
        board
            .insert_trace_without_cleaning(
                Polyline::from_int_points(&[from, to]),
                layer,
                50,
                NetNos::of(net),
                1,
                FixedState::Unfixed,
            )
            .expect("a two-corner trace inserts")
    }

    #[test]
    fn insert_and_remove_keep_the_trees_in_step() {
        let mut board = board();
        let id = straight(
            &mut board,
            1,
            0,
            IntPoint::new(0, 0),
            IntPoint::new(1000, 0),
        );
        assert_eq!(board.default_tree().leaf_count(), 1);
        board.assert_consistent();

        assert!(board.remove_item(id));
        assert_eq!(board.default_tree().leaf_count(), 0);
        board.assert_consistent();
    }

    #[test]
    fn a_degenerate_trace_is_refused_below_user_fixed_and_accepted_at_it() {
        let mut board = board();
        let point = IntPoint::new(100, 100);
        assert!(board
            .insert_trace_without_cleaning(
                Polyline::from_int_points(&[point, point]),
                0,
                50,
                NetNos::of(1),
                1,
                FixedState::ShoveFixed,
            )
            .is_none());

        // A two-corner polyline whose corners coincide: two coincident
        // verticals with one horizontal between them. Built from lines because
        // the point constructor collapses the parallel there-and-back pair.
        let degenerate = Polyline::new_from_lines(&[
            fr_geometry::planar::Line::new(IntPoint::new(0, -10), IntPoint::new(0, 10)),
            fr_geometry::planar::Line::new(IntPoint::new(-10, 0), IntPoint::new(10, 0)),
            fr_geometry::planar::Line::new(IntPoint::new(0, 10), IntPoint::new(0, -10)),
        ]);
        assert_eq!(degenerate.corner_count(), 2);
        assert_eq!(degenerate.first_corner(), degenerate.last_corner());

        assert!(
            board
                .insert_trace_without_cleaning(
                    degenerate.clone(),
                    0,
                    50,
                    NetNos::of(1),
                    1,
                    FixedState::ShoveFixed,
                )
                .is_none(),
            "below USER_FIXED a degenerate trace is refused"
        );
        assert!(
            board
                .insert_trace_without_cleaning(
                    degenerate,
                    0,
                    50,
                    NetNos::of(1),
                    1,
                    FixedState::UserFixed,
                )
                .is_some(),
            "at USER_FIXED it is inserted — which is why normalize needs the skip"
        );
    }

    #[test]
    fn an_out_of_range_clearance_class_is_forced_to_zero() {
        let mut board = board();
        let id = board.insert_item(Item::new(
            NetNos::of(1),
            99,
            FixedState::Unfixed,
            ItemKind::Trace(PolylineTrace {
                polyline: Polyline::from_int_points(&[IntPoint::new(0, 0), IntPoint::new(1000, 0)]),
                layer: 0,
                half_width: 50,
            }),
        ));
        assert_eq!(board.store().get(id).unwrap().clearance_class, 0);
    }

    #[test]
    fn removal_of_a_user_fixed_item_is_a_silent_no_op() {
        let mut board = board();
        let id = board
            .insert_trace_without_cleaning(
                Polyline::from_int_points(&[IntPoint::new(0, 0), IntPoint::new(1000, 0)]),
                0,
                50,
                NetNos::of(1),
                1,
                FixedState::UserFixed,
            )
            .unwrap();
        assert!(!board.remove_item(id));
        assert!(board.store().contains(id));
        board.assert_consistent();
    }

    #[test]
    fn trace_half_width_extremes_track_inserted_traces() {
        let mut board = board();
        assert_eq!(board.max_trace_half_width(), INITIAL_MAX_TRACE_HALF_WIDTH);
        assert_eq!(board.min_trace_half_width(), INITIAL_MIN_TRACE_HALF_WIDTH);
        board
            .insert_trace_without_cleaning(
                Polyline::from_int_points(&[IntPoint::new(0, 0), IntPoint::new(1000, 0)]),
                0,
                20,
                NetNos::of(1),
                1,
                FixedState::Unfixed,
            )
            .unwrap();
        assert_eq!(board.min_trace_half_width(), 20);
        assert_eq!(board.max_trace_half_width(), INITIAL_MAX_TRACE_HALF_WIDTH);
    }

    #[test]
    fn rollback_restores_the_trees_and_the_connectivity() {
        let mut board = board();
        let kept = straight(
            &mut board,
            1,
            0,
            IntPoint::new(0, 0),
            IntPoint::new(1000, 0),
        );
        assert_eq!(board.net_component_count(1), 1);

        let mark = board.begin();
        let added = straight(
            &mut board,
            1,
            0,
            IntPoint::new(5000, 0),
            IntPoint::new(6000, 0),
        );
        board.remove_item(kept);
        assert_eq!(board.default_tree().leaf_count(), 1);

        board.rollback_to(mark);
        assert!(board.store().contains(kept));
        assert!(!board.store().contains(added));
        assert_eq!(board.default_tree().leaf_count(), 1);
        assert_eq!(board.net_component_count(1), 1);
        board.assert_consistent();
    }

    #[test]
    fn a_via_splits_traces_only_on_the_half_open_layer_range() {
        // §11.3: `insertVia` loops `from .. to`, so the via's LAST layer is not
        // split; `insertEscapeVia` loops `from ..= to`.
        let layers = |closed: bool| {
            let mut board = board();
            for layer in 0..3 {
                straight(
                    &mut board,
                    1,
                    layer,
                    IntPoint::new(-1000, 0),
                    IntPoint::new(1000, 0),
                );
            }
            let via = Via {
                center: IntPoint::new(0, 0),
                padstack_no: 0,
                layers: LayerRange::new(0, 2),
                attach_allowed: true,
                is_escape_via: closed,
                escape_via_smd_layer: -1,
            };
            if closed {
                board.insert_escape_via(via, NetNos::of(1), 1, FixedState::Unfixed);
            } else {
                board.insert_via(via, NetNos::of(1), 1, FixedState::Unfixed);
            }
            let mut per_layer = [0usize; 3];
            for (_, item) in board.store().iter() {
                if let ItemKind::Trace(trace) = &item.kind {
                    per_layer[trace.layer as usize] += 1;
                }
            }
            board.assert_consistent();
            per_layer
        };

        assert_eq!(
            layers(false),
            [2, 2, 1],
            "insertVia leaves the last layer unsplit"
        );
        assert_eq!(layers(true), [2, 2, 2], "insertEscapeVia splits all layers");
    }

    #[test]
    fn a_cloned_board_is_independent_and_self_consistent() {
        // State mechanism 3 in its non-CoW form: a worker's board must not
        // alias the board it came from, and its trees must be its own — a
        // shallow copy would leave leaves pointing at the original's slots.
        let mut board = board();
        for i in 0..4i32 {
            straight(
                &mut board,
                1,
                0,
                IntPoint::new(i * 1000, 0),
                IntPoint::new(i * 1000 + 500, 0),
            );
        }
        let mut worker = board.clone();
        worker.assert_consistent();

        let doomed = worker.store().ids().next().unwrap();
        assert!(worker.remove_item(doomed));
        straight(
            &mut worker,
            2,
            1,
            IntPoint::new(0, 5000),
            IntPoint::new(500, 5000),
        );

        assert!(board.store().contains(doomed));
        assert!(!worker.store().contains(doomed));
        assert_eq!(board.default_tree().leaf_count(), 4);
        assert_eq!(worker.default_tree().leaf_count(), 4);
        board.assert_consistent();
        worker.assert_consistent();
    }

    #[test]
    fn normal_contacts_are_exact_end_corners_only() {
        let mut board = board();
        let a = straight(
            &mut board,
            1,
            0,
            IntPoint::new(0, 0),
            IntPoint::new(1000, 0),
        );
        let b = straight(
            &mut board,
            1,
            0,
            IntPoint::new(1000, 0),
            IntPoint::new(1000, 1000),
        );
        // Crosses `a` in the middle but ends nowhere near a corner of it.
        let _crossing = straight(
            &mut board,
            1,
            0,
            IntPoint::new(500, -500),
            IntPoint::new(500, 500),
        );

        let mut contacts = Vec::new();
        board.normal_contacts_at(a, &Point::Int(IntPoint::new(1000, 0)), &mut contacts);
        assert_eq!(contacts, vec![b]);

        board.normal_contacts_at(a, &Point::Int(IntPoint::new(0, 0)), &mut contacts);
        assert!(contacts.is_empty());
    }

    // ---- adopt_store -----------------------------------------------------

    /// Adopting a snapshot must put the board back exactly where a whole-board
    /// clone would, trees included.
    #[test]
    fn adopting_a_snapshot_restores_the_items_and_the_trees() {
        let mut board = board();
        let kept = straight(
            &mut board,
            1,
            0,
            IntPoint::new(0, 0),
            IntPoint::new(1000, 0),
        );
        let doomed = straight(
            &mut board,
            1,
            1,
            IntPoint::new(0, 500),
            IntPoint::new(1000, 500),
        );
        let snapshot = board.store().clone();
        let reference = board.clone();

        board.remove_item(doomed);
        let added = straight(
            &mut board,
            2,
            2,
            IntPoint::new(0, 900),
            IntPoint::new(900, 900),
        );
        assert_eq!(board.default_tree().leaf_count(), 2);

        let delta = board.adopt_store(snapshot);
        assert_eq!(delta.added, vec![doomed], "the removed trace comes back");
        assert_eq!(delta.removed, vec![added], "the new trace goes away");
        assert!(delta.changed.is_empty());

        board.assert_consistent();
        assert!(board.store().contains(kept));
        assert!(board.store().contains(doomed));
        assert!(!board.store().contains(added));
        assert_eq!(
            board.default_tree().leaf_count(),
            reference.default_tree().leaf_count()
        );
        // The counters never rewind: the next id is past the one the discarded
        // trace consumed.
        assert!(board.store().next_id() > added);
    }

    /// The one case a revision-only diff misses: an item whose content did not
    /// change but whose *slot* did, which a slot-indexed search tree still has
    /// to move. A remove inside a transaction, rolled back, re-attaches through
    /// the free list and can land elsewhere.
    #[test]
    fn adopting_a_snapshot_relocates_an_item_that_changed_slots() {
        let mut board = board();
        let a = straight(
            &mut board,
            1,
            0,
            IntPoint::new(0, 0),
            IntPoint::new(1000, 0),
        );
        let b = straight(
            &mut board,
            1,
            1,
            IntPoint::new(0, 500),
            IntPoint::new(1000, 500),
        );
        let snapshot = board.store().clone();
        let slot_before = board.store().slot_of(b).expect("b is live");

        // Free `a`'s slot, then roll the removals back so `b` is re-attached
        // into whichever slot the free list hands out.
        let mark = board.begin();
        board.remove_item(a);
        board.remove_item(b);
        board.rollback_to(mark);
        board.assert_consistent();

        if board.store().slot_of(b) != Some(slot_before) {
            assert!(
                board.store().revision_of(b) == snapshot.revision_of(b),
                "the rollback restored the content, so only the slot moved"
            );
            let delta = board.adopt_store(snapshot);
            assert!(
                delta.changed.contains(&b),
                "a relocation must be reported so the trees follow it: {delta:?}"
            );
            board.assert_consistent();
        } else {
            // The free list handed the slot straight back; nothing to prove
            // beyond the trees still agreeing.
            board.adopt_store(snapshot);
            board.assert_consistent();
        }
        assert!(board.store().contains(a) && board.store().contains(b));
    }

    /// The pooled form: the displaced items come back out through the same
    /// buffer, which is what makes a thirty-deep history thirty stores and not
    /// thirty boards.
    #[test]
    fn swapping_a_store_hands_the_old_one_back() {
        let mut board = board();
        let first = straight(
            &mut board,
            1,
            0,
            IntPoint::new(0, 0),
            IntPoint::new(1000, 0),
        );
        let mut buffer = board.store().clone();

        let second = straight(
            &mut board,
            1,
            1,
            IntPoint::new(0, 500),
            IntPoint::new(1000, 500),
        );
        let mut delta = StoreDelta::new();
        board.swap_store(&mut buffer, &mut delta);

        assert_eq!(delta.removed, vec![second]);
        assert!(
            buffer.contains(second),
            "the buffer holds the displaced board"
        );
        assert!(!board.store().contains(second));
        assert!(board.store().contains(first));
        board.assert_consistent();
    }

    #[test]
    #[should_panic(expected = "while a transaction is open")]
    fn adopting_inside_a_transaction_panics() {
        let mut board = board();
        let snapshot = board.store().clone();
        let _mark = board.begin();
        board.adopt_store(snapshot);
    }
}
