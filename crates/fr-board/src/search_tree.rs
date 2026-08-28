//! The spatial index: one [`SearchTree`] per compensated clearance class, kept
//! incrementally in step with the item store.
//!
//! Ported from `board/searchtree/{ShapeSearchTree, SearchTreeManager}` per
//! `search-tree-obstacles.md` §2 (compensation), §3 (tile shapes per item
//! type), §5 (item ↔ tree wiring) and §10 (the Rust design implications, which
//! this module follows rather than the Java structure).
//!
//! # The one idea
//!
//! A tree compensated for clearance class `T` stores every obstacle shape
//! **pre-inflated** by `clearance(item_cl, T, layer) − (self_clearance(T)+1)/2`,
//! so a router probing it with radius `half_width + (self_clearance(T)+1)/2`
//! turns "keep a clearance" into "do not touch". [`crate::clearance`] owns that
//! arithmetic; this module owns the geometry it is applied to and the
//! bookkeeping that keeps it fresh.
//!
//! # Structural departures from upstream (all endorsed by §10)
//!
//! * **One bounding flavor.** Upstream picks `IntBox` bounds for the 90° tree
//!   and `IntOctagon` for everything else; `fr-datastructures`' arena tree
//!   stores `OctBounds` for every node and a box is the degenerate octagon, so
//!   only the *exact* shape flavor is still per-board.
//! * **Side tables, not per-item lists.** Upstream hangs a
//!   `LinkedList<SearchTreeInfo>` off every `Item` and linearly scans it by tree
//!   identity on every shape lookup. Here each tree owns a `Vec<SlotEntry>`
//!   indexed by the store's arena slot ([`ItemStore::slot_of`]), so the lookup
//!   is one bounds-checked index and a snapshot of the item store does not drag
//!   megabytes of recomputable tile shapes along.
//! * **Deterministic result order is explicit.** Upstream gets it from
//!   `TreeSet<Leaf>` ordered by `Item.compareTo` (descending id) then shape
//!   index; a tree walk here reports leaves in traversal order, so
//!   [`SearchTree::overlapping_tree_entries`] sorts by that key itself.
//!
//! # What is simplified, and where the full rule lives
//!
//! The tile-shape rules of §3 are implemented for the flavors this crate's
//! geometry supports today. Everything below is a *conservative* stand-in —
//! never a shape smaller than upstream's, so no obstacle is missed:
//!
//! | § | upstream | here |
//! |---|---|---|
//! | 3.1 | trace = `Polyline.offset_shape` per segment, dog-ear cuts, octagon clip | identical (`fr_geometry::planar::Polyline::offset_shapes`) |
//! | 3.4 | pad per layer, flavor per angle restriction, then `enlarge(comp)` | identical, from the board's [`PadstackTable`] |
//! | 9.1 | missing pad ⇒ `Circle(center, ceil(drill_radius))` + `drillHoleClearanceDelta` | the circle's **bounding octagon**; the extra hole-clearance delta is not modelled |
//! | 3.5 | `area.split_to_convex()` per piece, `enlarge`, `divide_into_sections(50000)` | identical — [`AreaShape::Polygon`] is split by [`fr_geometry::planar::split_to_convex`] on demand; only a corner list that is not a simple polygon (where upstream fails too) falls back to a bounding octagon |
//! | 3.6 | outline ring: one `offset_shape(BoardOutline.HALF_WIDTH + comp, 0)` per border line per layer | identical, driven off the stored border polylines; the `keepout_outside_outline_generated()` branch is **not** modelled (the generated keepout area has no field on [`crate::item::BoardOutline`] yet) |
//! | 6 | `change_entries` / `merge_entries_*` / `reuse_entries_after_cutout` reuse leaves in place | plain remove + insert; the fast paths are an optimization of the same result |
//! | 2.2 | `overlapping_tree_entries_with_clearance` for uncompensated trees | not here — it belongs to the checker, which is not this crate |

use fr_datastructures::arena_shape_tree::{ArenaLeafId, ArenaShapeTree, OctBounds, QueryStack};
use fr_geometry::planar::{IntOctagon, IntPoint, Point};

use crate::clearance::ClearanceSource;
use crate::item::{AreaShape, Item, ItemId, ItemKind, PlacedArea};
use crate::shape::{self, AngleRestriction, PadstackTable, TileShape};
use crate::store::{ItemStore, SlotIndex};

/// Upstream `ShapeSearchTree.max_tree_shape_width` (`:909-916`): the cap on a
/// single indexed shape, so that one huge copper pour cannot flatten tree
/// selectivity. Upstream tightens it to `min(500 × resolution(MIL), 50000)`
/// when a host CAD is known; that input does not exist in this crate yet.
pub const MAX_TREE_SHAPE_WIDTH: f64 = 50_000.0;

/// Upstream `BoardOutline.HALF_WIDTH` (`BoardOutline.java:28`) — the constant
/// half width every outline border strip is offset by, *before* the clearance
/// compensation is added (`ShapeSearchTree.java:1000,1013`:
/// `offset_shape(half_width + cmp_value, 0)`).
///
/// The outline has no real copper width; the constant is what gives the board
/// edge a solid obstacle instead of a zero-area line, so leaving it out lets
/// the router come 100 board units closer to the edge than upstream allows.
pub const BOARD_OUTLINE_HALF_WIDTH: i32 = 100;

/// A query hit: which item, and which of its tile shapes.
///
/// Upstream's `ShapeTree.TreeEntry`, minus the object reference — ids are the
/// identity here, and the shape is one index away through
/// [`SearchTree::tile_shape`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TreeEntry {
    /// The item the shape belongs to.
    pub item: ItemId,
    /// Index of the shape inside that item's shape array.
    pub shape_index: u16,
}

/// Leaf payload. `Copy` and 8 bytes, so the arena tree stores it inline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LeafPayload {
    item: ItemId,
    shape_index: u16,
}

/// One tile shape of one item, with the layer it lives on.
///
/// The shape is optional because a drill item's index space is its **layer**
/// span: upstream's `tile_shape_count()` is `to_layer - from_layer + 1` and
/// `shape_layer(i)` is `from_layer + i`, with a `null` entry for a padstack
/// layer that carries no pad (`search-tree-obstacles.md` §3, §8). Keeping the
/// hole in the index space rather than compacting it is what makes
/// `shape_layer` a subtraction instead of a lookup.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemTileShape {
    /// The compensated tile shape, or `None` for an unoccupied index.
    pub shape: Option<TileShape>,
    /// The layer this shape lies on.
    pub layer: i32,
}

/// Everything shape computation needs besides the item itself.
///
/// Bundled into one borrow so that a [`Board`](crate::board::Board) can hand
/// the tree manager read access to its rules while holding the manager
/// mutably — the fields are disjoint, but the borrow checker needs to see that
/// in one place.
#[derive(Clone, Copy)]
pub struct ShapeContext<'a> {
    /// The clearance matrix the compensation is computed from.
    pub clearances: &'a dyn ClearanceSource,
    /// Pad geometry for drill items.
    pub padstacks: &'a PadstackTable,
    /// Number of board layers, for the per-layer outline shapes.
    pub layer_count: usize,
    /// The board-wide trace angle restriction (§3.4 reads it from the *rules*,
    /// not from the tree).
    pub angle_restriction: AngleRestriction,
    /// Cap on one indexed shape; see [`MAX_TREE_SHAPE_WIDTH`].
    pub max_tree_shape_width: f64,
}

/// Per-slot bookkeeping: which item is in the slot, its compensated shapes, and
/// the leaves they were inserted as.
///
/// `id` is the guard that makes a slot-indexed table safe: slots are recycled,
/// so a stale entry would otherwise be read as belonging to whatever item moved
/// in. Every read checks it.
#[derive(Debug, Clone, Default)]
struct SlotEntry {
    id: ItemId,
    shapes: Vec<ItemTileShape>,
    leaves: Vec<ArenaLeafId>,
}

impl SlotEntry {
    fn clear(&mut self) {
        self.id = ItemId::UNASSIGNED;
        self.shapes.clear();
        self.leaves.clear();
    }
}

/// One spatial index over the board, compensated for a single clearance class.
///
/// `compensated_class == 0` is upstream's uncompensated default tree: shapes
/// are stored raw, and the caller is responsible for the clearance itself.
#[derive(Debug, Clone)]
pub struct SearchTree {
    compensated_class: i32,
    tree: ArenaShapeTree<LeafPayload>,
    slots: Vec<SlotEntry>,
}

impl SearchTree {
    /// An empty tree compensated for `compensated_class`.
    pub fn new(compensated_class: i32) -> Self {
        SearchTree {
            compensated_class,
            tree: ArenaShapeTree::new(),
            slots: Vec::new(),
        }
    }

    /// The clearance class this tree's shapes are inflated for; `0` = raw.
    #[inline]
    pub fn compensated_class(&self) -> i32 {
        self.compensated_class
    }

    /// Number of leaves currently indexed.
    #[inline]
    pub fn leaf_count(&self) -> usize {
        self.tree.len()
    }

    /// The tile shapes computed for the item in `slot`, or an empty slice if
    /// the slot holds a different item (or none).
    pub fn item_shapes(&self, slot: SlotIndex, id: ItemId) -> &[ItemTileShape] {
        match self.slots.get(slot.index()) {
            Some(entry) if entry.id == id => &entry.shapes,
            _ => &[],
        }
    }

    /// One tile shape by (slot, index); `None` for an unoccupied index.
    pub fn tile_shape(&self, slot: SlotIndex, id: ItemId, index: u16) -> Option<&TileShape> {
        self.item_shapes(slot, id)
            .get(index as usize)
            .and_then(|s| s.shape.as_ref())
    }

    /// The layer of one tile shape.
    pub fn shape_layer(&self, slot: SlotIndex, id: ItemId, index: u16) -> Option<i32> {
        self.item_shapes(slot, id).get(index as usize).map(|s| s.layer)
    }

    // ---- maintenance -----------------------------------------------------

    fn ensure_slot(&mut self, slot: usize) {
        if slot >= self.slots.len() {
            self.slots.resize_with(slot + 1, SlotEntry::default);
        }
    }

    /// Computes `item`'s tile shapes and inserts a leaf for each of them.
    ///
    /// Upstream `SearchTreeManager.insert` → `ShapeTree.insert`: shapes are
    /// computed once per (item, tree) and shared by the leaves, which is why
    /// they are stored before the leaves are made.
    pub fn insert(&mut self, slot: SlotIndex, item: &Item, ctx: ShapeContext<'_>) {
        let slot_index = slot.index();
        self.ensure_slot(slot_index);
        debug_assert!(
            self.slots[slot_index].leaves.is_empty(),
            "fr-board search tree: slot {slot_index} still holds leaves of item {}",
            self.slots[slot_index].id
        );

        let mut entry = std::mem::take(&mut self.slots[slot_index]);
        entry.clear();
        entry.id = item.id;
        calculate_tree_shapes(item, self.compensated_class, ctx, &mut entry.shapes);

        for (index, tile) in entry.shapes.iter().enumerate() {
            let Some(shape) = tile.shape.as_ref() else {
                continue;
            };
            let leaf = self.tree.insert(
                shape.bounds(),
                LeafPayload {
                    item: item.id,
                    shape_index: index as u16,
                },
            );
            entry.leaves.push(leaf);
        }
        self.slots[slot_index] = entry;
    }

    /// Removes every leaf of the item in `slot`.
    ///
    /// A no-op when the slot holds a different item, which is what makes
    /// double-removal and rollback-driven resynchronization safe.
    pub fn remove(&mut self, slot: SlotIndex, id: ItemId) -> bool {
        let Some(entry) = self.slots.get_mut(slot.index()) else {
            return false;
        };
        if entry.id != id {
            return false;
        }
        for leaf in entry.leaves.drain(..) {
            self.tree.remove(leaf);
        }
        entry.clear();
        true
    }

    /// Drops everything, keeping the allocations.
    pub fn clear(&mut self) {
        self.tree.clear();
        for entry in &mut self.slots {
            entry.clear();
        }
    }

    // ---- queries ---------------------------------------------------------

    /// Every indexed shape overlapping `query`, upstream's
    /// `overlapping_tree_entries` (`ShapeSearchTree.java:369-417`).
    ///
    /// * `layer`: `None` is upstream's `-1`, "every layer".
    /// * `ignore_nets`: an item is skipped when it is an obstacle to **none**
    ///   of the listed nets — i.e. own-net items are filtered out. An empty
    ///   list filters nothing.
    ///
    /// `out` is cleared first and comes back sorted by **descending item id,
    /// then ascending shape index**, which is upstream's `TreeSet<Leaf>` order
    /// (`Item.compareTo` is `other.id - this.id`). Routing results depend on
    /// it, so it is produced explicitly rather than inherited from a container.
    pub fn overlapping_tree_entries(
        &self,
        store: &ItemStore,
        query: &TileShape,
        layer: Option<i32>,
        ignore_nets: &[i32],
        stack: &mut QueryStack,
        out: &mut Vec<TreeEntry>,
    ) {
        out.clear();
        let bounds = query.bounds();
        self.visit(store, &bounds, stack, |item, payload, tile| {
            if !layer_matches(layer, tile.layer) || is_ignored(item, ignore_nets) {
                return;
            }
            let Some(shape) = tile.shape.as_ref() else {
                return;
            };
            if !shape.intersects(query) {
                return;
            }
            out.push(TreeEntry {
                item: payload.item,
                shape_index: payload.shape_index,
            });
        });
        sort_entries(out);
    }

    /// [`overlapping_tree_entries`](SearchTree::overlapping_tree_entries)
    /// deduplicated to items, upstream's `overlapping_objects`.
    ///
    /// Descending id order.
    pub fn overlapping_items(
        &self,
        store: &ItemStore,
        query: &TileShape,
        layer: Option<i32>,
        ignore_nets: &[i32],
        stack: &mut QueryStack,
        out: &mut Vec<ItemId>,
    ) {
        out.clear();
        let bounds = query.bounds();
        self.visit(store, &bounds, stack, |item, payload, tile| {
            if !layer_matches(layer, tile.layer) || is_ignored(item, ignore_nets) {
                return;
            }
            let Some(shape) = tile.shape.as_ref() else {
                return;
            };
            if shape.intersects(query) {
                out.push(payload.item);
            }
        });
        // Descending id, then deduplicate — upstream's `TreeSet<SearchTreeObject>`.
        out.sort_by(|a, b| b.cmp(a));
        out.dedup();
    }

    /// The shared broad-phase walk: resolve every hit leaf to its item and tile
    /// shape, and hand both to `visit`.
    ///
    /// The DFS stack is caller-owned (so the tree itself stays `&self` and the
    /// query allocates nothing once warm); the closure holds a second immutable
    /// borrow of `self` for the slot table, which is why the two are separate
    /// parameters.
    fn visit(
        &self,
        store: &ItemStore,
        bounds: &OctBounds,
        stack: &mut QueryStack,
        mut visit: impl FnMut(&Item, LeafPayload, &ItemTileShape),
    ) {
        self.tree.visit_overlaps(bounds, stack, |_leaf, payload| {
            let Some(slot) = store.slot_of(payload.item) else {
                return;
            };
            let Some(entry) = self.slots.get(slot.index()) else {
                return;
            };
            if entry.id != payload.item {
                return;
            }
            let Some(tile) = entry.shapes.get(payload.shape_index as usize) else {
                return;
            };
            let Some(item) = store.get(payload.item) else {
                return;
            };
            visit(item, payload, tile);
        });
    }

    /// Panics unless every leaf, slot entry and store item agree.
    ///
    /// O(board); for tests and `debug_assert`-style use from the board layer.
    pub fn assert_consistent(&self, store: &ItemStore) {
        let mut leaves = 0usize;
        for (slot_index, entry) in self.slots.iter().enumerate() {
            if entry.id == ItemId::UNASSIGNED {
                assert!(
                    entry.leaves.is_empty() && entry.shapes.is_empty(),
                    "empty slot {slot_index} still carries tree state"
                );
                continue;
            }
            let slot = store
                .slot_of(entry.id)
                .unwrap_or_else(|| panic!("tree holds {} which is not on the board", entry.id));
            assert_eq!(
                slot.index(),
                slot_index,
                "item {} is indexed under the wrong slot",
                entry.id
            );
            let occupied = entry.shapes.iter().filter(|s| s.shape.is_some()).count();
            assert_eq!(
                occupied,
                entry.leaves.len(),
                "item {} has {occupied} shapes but {} leaves",
                entry.id,
                entry.leaves.len()
            );
            leaves += entry.leaves.len();
            for leaf in &entry.leaves {
                let payload = self
                    .tree
                    .get(*leaf)
                    .unwrap_or_else(|| panic!("item {} holds a stale leaf handle", entry.id));
                assert_eq!(payload.item, entry.id, "leaf points at a different item");
            }
        }
        assert_eq!(
            leaves,
            self.tree.len(),
            "the slot table accounts for {leaves} leaves but the tree holds {}",
            self.tree.len()
        );
    }
}

/// The registry of active trees, upstream's `SearchTreeManager`.
///
/// Every board mutation fans out to **every** active tree (§5.2), which is why
/// creating an autoroute tree roughly doubles the cost of each subsequent trace
/// insert or rip.
#[derive(Debug, Clone)]
pub struct SearchTreeManager {
    default_tree: SearchTree,
    compensated: Vec<SearchTree>,
    clearance_compensation_used: bool,
}

impl Default for SearchTreeManager {
    fn default() -> Self {
        SearchTreeManager::new()
    }
}

impl SearchTreeManager {
    /// A manager with the uncompensated default tree only, upstream's
    /// constructor state (`SearchTreeManager.java:19-25`).
    pub fn new() -> Self {
        SearchTreeManager {
            default_tree: SearchTree::new(0),
            compensated: Vec::new(),
            clearance_compensation_used: false,
        }
    }

    /// The default (interactive) tree.
    #[inline]
    pub fn default_tree(&self) -> &SearchTree {
        &self.default_tree
    }

    /// True when the default tree stores compensated shapes.
    #[inline]
    pub fn clearance_compensation_used(&self) -> bool {
        self.clearance_compensation_used
    }

    /// The compensated tree for `clearance_class`, if one exists.
    pub fn compensated_tree(&self, clearance_class: i32) -> Option<&SearchTree> {
        if self.default_tree.compensated_class == clearance_class {
            return Some(&self.default_tree);
        }
        self.compensated
            .iter()
            .find(|tree| tree.compensated_class == clearance_class)
    }

    /// Number of active trees, default included.
    pub fn tree_count(&self) -> usize {
        1 + self.compensated.len()
    }

    fn trees_mut(&mut self) -> impl Iterator<Item = &mut SearchTree> {
        std::iter::once(&mut self.default_tree).chain(self.compensated.iter_mut())
    }

    /// Inserts `item` into every active tree.
    pub fn insert(&mut self, slot: SlotIndex, item: &Item, ctx: ShapeContext<'_>) {
        for tree in self.trees_mut() {
            tree.insert(slot, item, ctx);
        }
    }

    /// Removes the item in `slot` from every active tree.
    pub fn remove(&mut self, slot: SlotIndex, id: ItemId) {
        for tree in self.trees_mut() {
            tree.remove(slot, id);
        }
    }

    /// Creates — and bulk-loads — the tree compensated for `clearance_class`,
    /// upstream's `getAutorouteTree` (`SearchTreeManager.java:129-159`).
    ///
    /// Returns immediately when a tree for that class already exists. Class 0
    /// never gets its own tree: it is the default tree by definition.
    pub fn ensure_compensated_tree(
        &mut self,
        clearance_class: i32,
        store: &ItemStore,
        ctx: ShapeContext<'_>,
    ) {
        if clearance_class <= 0 || self.compensated_tree(clearance_class).is_some() {
            return;
        }
        let mut tree = SearchTree::new(clearance_class);
        // Ascending id: tree *shape* depends on insertion order but query
        // results never do (they are sorted), and ascending is the order the
        // ordered index streams in.
        for (id, item) in store.iter_ascending() {
            if let Some(slot) = store.slot_of(id) {
                tree.insert(slot, item, ctx);
            }
        }
        self.compensated.push(tree);
    }

    /// Drops every tree but the default one — upstream's
    /// `reset_compensated_trees()`, called when autorouting ends.
    pub fn reset_compensated_trees(&mut self) {
        self.compensated.clear();
    }

    /// Rebuilds every tree from `store`, upstream's `reinsert_tree_items()`.
    ///
    /// The blunt instrument behind a clearance-rule change; ordinary mutations
    /// never take this path.
    pub fn rebuild(&mut self, store: &ItemStore, ctx: ShapeContext<'_>) {
        for tree in self.trees_mut() {
            tree.clear();
        }
        for (id, item) in store.iter_ascending() {
            if let Some(slot) = store.slot_of(id) {
                for tree in self.trees_mut() {
                    tree.insert(slot, item, ctx);
                }
            }
        }
    }

    /// Switches the default tree between raw and class-1-compensated shapes,
    /// upstream's `set_clearance_compensation_used` (`:78-97`): remove all, new
    /// default tree, reinsert all.
    ///
    /// Upstream turns this on when the board uses only the `{null, default}`
    /// classes, so that even interactive routing gets the touching-is-a-
    /// violation property (§2.1).
    pub fn set_clearance_compensation_used(
        &mut self,
        used: bool,
        store: &ItemStore,
        ctx: ShapeContext<'_>,
    ) {
        if used == self.clearance_compensation_used {
            return;
        }
        self.clearance_compensation_used = used;
        self.default_tree = SearchTree::new(if used { 1 } else { 0 });
        self.compensated.clear();
        self.rebuild(store, ctx);
    }

    /// Panics unless every tree agrees with the store.
    pub fn assert_consistent(&self, store: &ItemStore) {
        self.default_tree.assert_consistent(store);
        for tree in &self.compensated {
            tree.assert_consistent(store);
        }
    }
}

// ---------------------------------------------------------------- geometry

fn layer_matches(query_layer: Option<i32>, shape_layer: i32) -> bool {
    match query_layer {
        None => true,
        Some(layer) => layer == shape_layer,
    }
}

/// Upstream's net filter: skip the object when the ignore list is non-empty and
/// the object is an obstacle to none of the listed nets
/// (`ShapeSearchTree.java:369-417`; `SearchTreeObject.is_obstacle(net)` is
/// `!contains_net(net)` for every `Item`).
fn is_ignored(item: &Item, ignore_nets: &[i32]) -> bool {
    if ignore_nets.is_empty() {
        return false;
    }
    !ignore_nets.iter().any(|net| !item.contains_net(*net))
}

fn sort_entries(out: &mut [TreeEntry]) {
    out.sort_by(|a, b| {
        b.item
            .cmp(&a.item)
            .then_with(|| a.shape_index.cmp(&b.shape_index))
    });
}

/// Computes the tile shapes of `item` for a tree compensated for
/// `tree_class`, appending them to `out` (which is cleared first).
///
/// This is upstream's double dispatch `Item.calculate_tree_shapes(tree)` →
/// `tree.calculate_tree_shapes(item)` collapsed into one match, which is the
/// only way to read §3 as the table it is.
pub fn calculate_tree_shapes(
    item: &Item,
    tree_class: i32,
    ctx: ShapeContext<'_>,
    out: &mut Vec<ItemTileShape>,
) {
    out.clear();
    let item_class = item.clearance_class;
    let compensation =
        |layer: i32| ctx.clearances.tree_compensation(item_class, tree_class, layer);

    match &item.kind {
        // §3.1 — one shape per polyline segment, with the joint border lines
        // shared, the dog-ear cuts and the octagon clip all inside
        // `Polyline::offset_shapes`.
        ItemKind::Trace(trace) => {
            let offset_width = trace.half_width + compensation(trace.layer);
            for shape in trace.polyline.offset_shapes(offset_width) {
                out.push(ItemTileShape {
                    shape: Some(TileShape::Simplex(shape)),
                    layer: trace.layer,
                });
            }
        }
        // §3.4 / §9.1 — one shape per spanned layer, holes in the index space
        // preserved.
        ItemKind::Pin(pin) => drill_shapes(
            pin.center,
            pin.padstack_no,
            pin.layers.first,
            pin.layers.last,
            &compensation,
            ctx,
            out,
        ),
        ItemKind::Via(via) => drill_shapes(
            via.center,
            via.padstack_no,
            via.layers.first,
            via.layers.last,
            &compensation,
            ctx,
            out,
        ),
        // §3.5 — convex pieces, enlarged, then sectioned so a plane cannot
        // become one giant leaf.
        ItemKind::ObstacleArea(area) => {
            area_shapes(&area.area, area.layer, &compensation, ctx, out)
        }
        ItemKind::ConductionArea(area) => {
            area_shapes(&area.area, area.layer, &compensation, ctx, out)
        }
        // §3.6 — a ring of thin segment shapes, replicated per layer. v2 takes
        // the compensation at the shape's own layer; v19 always used layer 0.
        ItemKind::BoardOutline(outline) => {
            for layer in 0..ctx.layer_count as i32 {
                // `half_width + cmp_value`, not `cmp_value`
                // (`ShapeSearchTree.java:1000,1013`).
                let offset_width = BOARD_OUTLINE_HALF_WIDTH + compensation(layer);
                for border in &outline.borders {
                    let segment_count = border.arr.len().saturating_sub(2);
                    for index in 0..segment_count {
                        if let Some(shape) = border.offset_shape(offset_width, index) {
                            out.push(ItemTileShape {
                                shape: Some(TileShape::Simplex(shape)),
                                layer,
                            });
                        }
                    }
                }
            }
        }
        // §3, table: `ComponentOutline.tile_shape_count()` is 0.
        ItemKind::ComponentOutline(_) => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn drill_shapes(
    center: IntPoint,
    padstack_no: u32,
    from_layer: i32,
    to_layer: i32,
    compensation: &impl Fn(i32) -> i32,
    ctx: ShapeContext<'_>,
    out: &mut Vec<ItemTileShape>,
) {
    let padstack = ctx.padstacks.get(padstack_no);
    let offset = shape::translation(center.difference_by_int(&IntPoint::ZERO));
    for layer in from_layer..=to_layer {
        let pad = padstack.and_then(|p| p.shape_on_layer(layer)).cloned();
        let shape = match pad {
            Some(pad) => Some(pad),
            // v2's synthetic drill-hole obstacle: an unused inner layer of a
            // drilled padstack still blocks, because the hole is there.
            None => padstack
                .filter(|p| p.drill_radius > 0)
                .map(|p| TileShape::Oct(shape::circle_octagon(p.drill_radius))),
        };
        let shape = shape.map(|shape| {
            shape
                .translate_by(&offset)
                .to_flavor(ctx.angle_restriction)
                .enlarge(compensation(layer))
        });
        out.push(ItemTileShape { shape, layer });
    }
}

fn area_shapes(
    area: &PlacedArea,
    layer: i32,
    compensation: &impl Fn(i32) -> i32,
    ctx: ShapeContext<'_>,
    out: &mut Vec<ItemTileShape>,
) {
    let grow = compensation(layer);
    for piece in placed_area_pieces(area) {
        for section in piece.enlarge(grow).divide_into_sections(ctx.max_tree_shape_width) {
            let section = section.to_flavor(ctx.angle_restriction);
            if section.is_empty() {
                continue;
            }
            out.push(ItemTileShape {
                shape: Some(section),
                layer,
            });
        }
    }
}

/// The convex decomposition of a placed area, in board coordinates.
///
/// Upstream materializes `relativeArea` through `translation` / `rotation` /
/// `sideChanged` and calls `Area.splitToConvex()`. The decomposition is derived
/// here, on demand, for the same reason it is derived there: `ObstacleArea`
/// stores the *original* area and nothing else, so anything that has to write
/// the area back out (`DsnWriter`, the golden dump) still has its corners.
///
/// * [`AreaShape::Polygon`] is split by
///   [`Polygon::split_to_convex`](fr_geometry::planar::Polygon::split_to_convex),
///   which is `PolygonShape.split_to_convex` itself. Only a corner list that is
///   not a simple polygon — where upstream's own division-point search also
///   fails — falls back to the **bounding octagon**, a conservative
///   over-approximation that is never a missed obstacle;
/// * [`AreaShape::Convex`] is a decomposition already in hand and is used as is;
/// * rotation is applied for multiples of 90° only; any other angle is placement
///   input this crate cannot realize exactly, and using the unrotated shape
///   would be wrong in the *unsafe* direction, so the bounding octagon of the
///   unrotated piece is used instead. The importer keeps that path unreachable
///   for the boards it loads by baking an oblique rotation into the stored
///   corners and handing the board `rotation_deg == 0` (`specctra::build`'s
///   `placed_area`, and Issue054); it stays here because [`PlacedArea`] is a
///   public type any caller can build.
pub fn placed_area_pieces(area: &PlacedArea) -> Vec<TileShape> {
    let base: Vec<TileShape> = match &area.relative_area {
        AreaShape::Box(b) => vec![TileShape::Box(*b)],
        AreaShape::Octagon(o) => vec![TileShape::Oct(*o)],
        AreaShape::Simplex(s) => vec![TileShape::Simplex(s.clone())],
        AreaShape::Convex(pieces) => pieces
            .iter()
            .cloned()
            .map(TileShape::Simplex)
            .collect(),
        AreaShape::Polygon(polygon) => {
            let corners = polygon.corner_array();
            if corners.len() < 3 {
                Vec::new()
            } else {
                match polygon.split_to_convex() {
                    Some(pieces) => pieces.into_iter().map(TileShape::Simplex).collect(),
                    None => vec![octagon_hull(corners)],
                }
            }
        }
    };

    let rotation = area.rotation_deg.rem_euclid(360.0);
    let quarter_turns = (rotation / 90.0).round();
    let is_right_angle = (quarter_turns * 90.0 - rotation).abs() < 1e-9;
    let pole = IntPoint::ZERO;
    let offset = shape::translation(area.translation);

    base.into_iter()
        .map(|piece| {
            let piece = if is_right_angle {
                piece.turn_90_degree(quarter_turns as i32, &pole)
            } else {
                // Not realizable exactly; widen to the bounding octagon of the
                // shape's circumscribed circle so the result covers every
                // rotation of it.
                widen_to_rotation_hull(&piece)
            };
            let piece = if area.side_changed {
                piece.mirror_vertical(&pole)
            } else {
                piece
            };
            piece.translate_by(&offset)
        })
        .collect()
}

/// The bounding octagon of the disc that every rotation of `shape` about the
/// origin stays inside — the conservative stand-in for a non-right-angle
/// placement rotation.
fn widen_to_rotation_hull(shape: &TileShape) -> TileShape {
    let bounding = shape.bounding_box();
    if bounding.is_empty() {
        return shape.clone();
    }
    let radius = [
        bounding.left_x().unsigned_abs(),
        bounding.right_x().unsigned_abs(),
        bounding.bottom_y().unsigned_abs(),
        bounding.top_y().unsigned_abs(),
    ]
    .into_iter()
    .max()
    .unwrap_or(0);
    let radius = ((radius as f64) * std::f64::consts::SQRT_2).ceil() as i32;
    TileShape::Oct(shape::circle_octagon(radius))
}

/// The smallest octagon containing `corners` — the conservative stand-in for a
/// polygon [`split_to_convex`](fr_geometry::planar::split_to_convex) could not
/// decompose, i.e. one whose border crosses itself.
fn octagon_hull(corners: &[Point]) -> TileShape {
    let mut lx = i32::MAX;
    let mut ly = i32::MAX;
    let mut rx = i32::MIN;
    let mut uy = i32::MIN;
    let mut ulx = i32::MAX;
    let mut lrx = i32::MIN;
    let mut llx = i32::MAX;
    let mut urx = i32::MIN;
    for corner in corners {
        let float = corner.to_float();
        let (x, y) = (float.x, float.y);
        lx = lx.min(x.floor() as i32);
        ly = ly.min(y.floor() as i32);
        rx = rx.max(x.ceil() as i32);
        uy = uy.max(y.ceil() as i32);
        ulx = ulx.min((x - y).floor() as i32);
        lrx = lrx.max((x - y).ceil() as i32);
        llx = llx.min((x + y).floor() as i32);
        urx = urx.max((x + y).ceil() as i32);
    }
    TileShape::Oct(IntOctagon::new(lx, ly, rx, uy, ulx, lrx, llx, urx))
}

#[cfg(test)]
mod tests {
    use fr_geometry::planar::{IntBox, IntVector, Polyline};

    use super::*;
    use crate::clearance::ClearanceTable;
    use crate::item::{
        FixedState, ItemKind, LayerRange, ObstacleArea, ObstacleKind, PolylineTrace, Via,
    };
    use crate::net_nos::NetNos;
    use crate::shape::Padstack;

    struct Fixture {
        clearances: ClearanceTable,
        padstacks: PadstackTable,
    }

    impl Fixture {
        fn new() -> Self {
            let mut padstacks = PadstackTable::new();
            padstacks.push(Padstack::round(0, 1, 40));
            Fixture {
                clearances: ClearanceTable::uniform(3, 2, 20),
                padstacks,
            }
        }

        fn ctx(&self) -> ShapeContext<'_> {
            ShapeContext {
                clearances: &self.clearances,
                padstacks: &self.padstacks,
                layer_count: 2,
                angle_restriction: AngleRestriction::FortyFive,
                max_tree_shape_width: MAX_TREE_SHAPE_WIDTH,
            }
        }
    }

    fn trace_item(nets: &[i32], points: &[IntPoint], layer: i32, half_width: i32) -> Item {
        Item::new(
            NetNos::from_slice(nets),
            1,
            FixedState::Unfixed,
            ItemKind::Trace(PolylineTrace {
                polyline: Polyline::from_int_points(points),
                layer,
                half_width,
            }),
        )
    }

    fn via_item(nets: &[i32], center: IntPoint) -> Item {
        Item::new(
            NetNos::from_slice(nets),
            1,
            FixedState::Unfixed,
            ItemKind::Via(Via {
                center,
                padstack_no: 0,
                layers: LayerRange::new(0, 1),
                attach_allowed: true,
                is_escape_via: false,
                escape_via_smd_layer: -1,
            }),
        )
    }

    #[test]
    fn a_trace_gets_one_shape_per_segment() {
        let fixture = Fixture::new();
        let item = trace_item(
            &[1],
            &[
                IntPoint::new(0, 0),
                IntPoint::new(1000, 0),
                IntPoint::new(1000, 1000),
            ],
            0,
            50,
        );
        let mut shapes = Vec::new();
        calculate_tree_shapes(&item, 0, fixture.ctx(), &mut shapes);
        assert_eq!(shapes.len(), 2, "two corners' worth of segments");
        assert!(shapes.iter().all(|s| s.layer == 0 && s.shape.is_some()));
    }

    #[test]
    fn compensation_inflates_the_trace_shape_but_class_zero_does_not() {
        let fixture = Fixture::new();
        let item = trace_item(&[1], &[IntPoint::new(0, 0), IntPoint::new(1000, 0)], 0, 50);

        let mut raw = Vec::new();
        calculate_tree_shapes(&item, 0, fixture.ctx(), &mut raw);
        let mut compensated = Vec::new();
        calculate_tree_shapes(&item, 1, fixture.ctx(), &mut compensated);

        let raw_box = raw[0].shape.as_ref().unwrap().bounding_box();
        let compensated_box = compensated[0].shape.as_ref().unwrap().bounding_box();
        // clearance(1,1) = 20, self compensation = (20+1)/2 = 10, so the
        // obstacle grows by 20 - 10 = 10.
        assert_eq!(compensated_box.top_y() - raw_box.top_y(), 10);
    }

    #[test]
    fn a_drill_item_gets_one_shape_per_spanned_layer() {
        let fixture = Fixture::new();
        let item = via_item(&[1], IntPoint::new(500, 500));
        let mut shapes = Vec::new();
        calculate_tree_shapes(&item, 0, fixture.ctx(), &mut shapes);
        assert_eq!(shapes.len(), 2);
        assert_eq!(shapes[0].layer, 0);
        assert_eq!(shapes[1].layer, 1);
        let bounding = shapes[0].shape.as_ref().unwrap().bounding_box();
        assert_eq!(bounding.left_x(), 460);
        assert_eq!(bounding.right_x(), 540);
    }

    #[test]
    fn an_unused_padstack_layer_keeps_its_index_and_gets_the_hole_obstacle() {
        let mut padstacks = PadstackTable::new();
        let mut padstack = Padstack::round(0, 2, 40);
        padstack.shapes[1] = None;
        padstacks.push(padstack);
        let clearances = ClearanceTable::uniform(3, 3, 20);
        let ctx = ShapeContext {
            clearances: &clearances,
            padstacks: &padstacks,
            layer_count: 3,
            angle_restriction: AngleRestriction::FortyFive,
            max_tree_shape_width: MAX_TREE_SHAPE_WIDTH,
        };

        let mut item = via_item(&[1], IntPoint::new(0, 0));
        if let ItemKind::Via(via) = &mut item.kind {
            via.layers = LayerRange::new(0, 2);
        }
        let mut shapes = Vec::new();
        calculate_tree_shapes(&item, 0, ctx, &mut shapes);
        assert_eq!(shapes.len(), 3, "the index space spans every drilled layer");
        assert_eq!(shapes[1].layer, 1);
        assert!(
            shapes[1].shape.is_some(),
            "a drilled layer without a pad still blocks (v2 §9.1)"
        );
    }

    #[test]
    fn a_component_outline_is_never_indexed() {
        let fixture = Fixture::new();
        let item = Item::new(
            NetNos::EMPTY,
            1,
            FixedState::SystemFixed,
            ItemKind::ComponentOutline(crate::item::ComponentOutline {
                area: PlacedArea::new(AreaShape::Box(IntBox::new(0, 0, 100, 100))),
                is_front: true,
            }),
        );
        let mut shapes = Vec::new();
        calculate_tree_shapes(&item, 0, fixture.ctx(), &mut shapes);
        assert!(shapes.is_empty());
    }

    #[test]
    fn a_placed_area_is_translated_into_board_coordinates() {
        let mut area = PlacedArea::new(AreaShape::Box(IntBox::new(0, 0, 100, 100)));
        area.translation = IntVector::new(1000, 2000);
        let pieces = placed_area_pieces(&area);
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].bounding_box(), IntBox::new(1000, 2000, 1100, 2100));
    }

    #[test]
    fn queries_are_sorted_descending_by_id_then_ascending_by_shape() {
        let fixture = Fixture::new();
        let mut store = ItemStore::new();
        let mut tree = SearchTree::new(0);

        let mut inserted = Vec::new();
        for _ in 0..4 {
            let id = store.insert(trace_item(
                &[1],
                &[
                    IntPoint::new(0, 0),
                    IntPoint::new(1000, 0),
                    IntPoint::new(1000, 1000),
                ],
                0,
                50,
            ));
            let slot = store.slot_of(id).unwrap();
            let item = store.get(id).unwrap();
            tree.insert(slot, item, fixture.ctx());
            inserted.push(id);
        }

        let mut stack = QueryStack::new();
        let mut out = Vec::new();
        tree.overlapping_tree_entries(
            &store,
            &TileShape::Box(IntBox::new(-10_000, -10_000, 10_000, 10_000)),
            Some(0),
            &[],
            &mut stack,
            &mut out,
        );
        assert_eq!(out.len(), 8);
        assert!(out.windows(2).all(|w| {
            w[0].item > w[1].item || (w[0].item == w[1].item && w[0].shape_index < w[1].shape_index)
        }));
        tree.assert_consistent(&store);
    }

    #[test]
    fn the_net_filter_skips_own_net_items_only() {
        let fixture = Fixture::new();
        let mut store = ItemStore::new();
        let mut tree = SearchTree::new(0);
        for net in [1, 2] {
            let id = store.insert(trace_item(
                &[net],
                &[IntPoint::new(0, 0), IntPoint::new(1000, 0)],
                0,
                50,
            ));
            let slot = store.slot_of(id).unwrap();
            tree.insert(slot, store.get(id).unwrap(), fixture.ctx());
        }

        let query = TileShape::Box(IntBox::new(0, -100, 1000, 100));
        let mut stack = QueryStack::new();
        let mut out = Vec::new();
        tree.overlapping_tree_entries(&store, &query, Some(0), &[], &mut stack, &mut out);
        assert_eq!(out.len(), 2, "no filter, both traces");

        tree.overlapping_tree_entries(&store, &query, Some(1), &[], &mut stack, &mut out);
        assert!(out.is_empty(), "layer filter");

        tree.overlapping_tree_entries(&store, &query, Some(0), &[1], &mut stack, &mut out);
        assert_eq!(out.len(), 1, "net 1 is filtered out");
        assert!(store.get(out[0].item).unwrap().contains_net(2));
    }

    #[test]
    fn removal_frees_every_leaf_and_a_recycled_slot_starts_clean() {
        let fixture = Fixture::new();
        let mut store = ItemStore::new();
        let mut tree = SearchTree::new(0);

        let a = store.insert(trace_item(
            &[1],
            &[
                IntPoint::new(0, 0),
                IntPoint::new(1000, 0),
                IntPoint::new(1000, 1000),
            ],
            0,
            50,
        ));
        let slot = store.slot_of(a).unwrap();
        tree.insert(slot, store.get(a).unwrap(), fixture.ctx());
        assert_eq!(tree.leaf_count(), 2);

        tree.remove(slot, a);
        store.remove(a);
        assert_eq!(tree.leaf_count(), 0);
        tree.assert_consistent(&store);

        let b = store.insert(via_item(&[2], IntPoint::new(0, 0)));
        let slot_b = store.slot_of(b).unwrap();
        assert_eq!(slot_b, slot, "the slot really is recycled");
        tree.insert(slot_b, store.get(b).unwrap(), fixture.ctx());
        tree.assert_consistent(&store);
    }

    #[test]
    fn a_compensated_tree_is_built_from_the_whole_board() {
        let fixture = Fixture::new();
        let mut store = ItemStore::new();
        let mut manager = SearchTreeManager::new();
        for _ in 0..3 {
            let id = store.insert(trace_item(
                &[1],
                &[IntPoint::new(0, 0), IntPoint::new(1000, 0)],
                0,
                50,
            ));
            manager.insert(store.slot_of(id).unwrap(), store.get(id).unwrap(), fixture.ctx());
        }
        assert_eq!(manager.tree_count(), 1);
        manager.ensure_compensated_tree(1, &store, fixture.ctx());
        assert_eq!(manager.tree_count(), 2);
        assert_eq!(manager.compensated_tree(1).unwrap().leaf_count(), 3);
        manager.assert_consistent(&store);

        manager.reset_compensated_trees();
        assert_eq!(manager.tree_count(), 1);
    }

    #[test]
    fn an_area_larger_than_the_cap_is_sectioned() {
        let fixture = Fixture::new();
        let item = Item::new(
            NetNos::EMPTY,
            1,
            FixedState::SystemFixed,
            ItemKind::ObstacleArea(ObstacleArea {
                area: PlacedArea::new(AreaShape::Box(IntBox::new(0, 0, 200_000, 200_000))),
                layer: 0,
                kind: ObstacleKind::Keepout,
            }),
        );
        let mut shapes = Vec::new();
        calculate_tree_shapes(&item, 0, fixture.ctx(), &mut shapes);
        assert!(
            shapes.len() > 1,
            "a 200k-wide keepout must be split at {MAX_TREE_SHAPE_WIDTH}"
        );
    }
}
