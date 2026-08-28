//! The seam: what the referee needs from a board, and nothing else.
//!
//! The checker must be able to judge *any* board — one our importer built, one
//! reconstructed from a foreign SES file, one a test wrote by hand — so it never
//! names a concrete board type. It sees a [`BoardView`]: an item list, the
//! per-item convex tile shapes with their layers, and a clearance lookup.
//!
//! Two seams are deliberate:
//!
//! * **[`ClearanceSource`]** rather than `&ClearanceMatrix` directly. The rules
//!   model is being reworked; the referee reads exactly two numbers out of it
//!   (`getValue` without the safety margin, and the per-class row maximum that
//!   sizes the candidate query), so that is the whole trait.
//!   [`fr_rules::ClearanceMatrix`] implements it, and
//!   [`UniformClearance`] covers the "one number for everything" case that most
//!   tests and every early bring-up board want.
//! * **[`ShapeStore`]** rather than deriving tile shapes here. Deriving them is
//!   the search-tree manager's job (`drc-connectivity.md` §1.4 — DRC compares
//!   the *default tree's* shapes, which for drill items are inflated by the hole
//!   clearance delta, and for the board outline are per-layer strips). Until
//!   that manager exists, whoever builds a board hands the referee the shapes it
//!   already computed. When it does exist, it implements [`BoardView`] directly
//!   and [`ShapeStore`] stays as the test scaffold.
//!
//! # Adapting `fr_board::Board`
//!
//! [`StoreView`] is the adapter that exists today, over the storage layer
//! ([`ItemStore`]) alone. The board proper — with its `SearchTreeManager`, its
//! own `TileShape` and its own clearance trait — is landing in a parallel wave;
//! wiring the referee to it is a `BoardView` impl of five one-liners and no
//! change anywhere else in this crate:
//!
//! ```text
//! item_ids()      -> board.store().ids().collect()   (cached once, descending)
//! item(id)        -> board.store().get(id)
//! shapes(id)      -> board.default_tree().item_shapes(board.store().slot_of(id), id)
//!                    mapped into ItemShape { shape, layer }
//! clearance(..)   -> board.clearances().value(a, b, layer)
//! max_clearance() -> board.clearances() row maximum
//! ```
//!
//! The two `TileShape` enums and the two `ClearanceSource` traits are
//! deliberately *not* shared. The referee's copies exist so that a change to how
//! the router models shapes or reads rules cannot silently change what the
//! referee reports — the thing every quality claim in this project is measured
//! against. The conversion is a three-arm `match`, and it is the right place for
//! an assertion that the two agree.

use fr_board::item::{Item, ItemId};
use fr_board::ItemStore;
use fr_rules::ClearanceMatrix;

use crate::shape::TileShape;

/// One convex piece of an item's copper, on one layer.
///
/// Upstream splits this across `Item.getTileShape(i)` and `Item.shapeLayer(i)`;
/// they are always read together, so they live together.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemShape {
    /// The convex piece.
    pub shape: TileShape,
    /// The layer it sits on. For a drill item this is
    /// `firstLayer() + min(index, lastLayer - firstLayer)`; for the board
    /// outline it is the striped `index * layerCount / shapeCount`.
    pub layer: i32,
}

impl ItemShape {
    /// Convenience constructor.
    pub fn new(shape: impl Into<TileShape>, layer: i32) -> Self {
        ItemShape {
            shape: shape.into(),
            layer,
        }
    }
}

/// The clearance numbers the referee reads.
///
/// Both methods take **raw upstream indices**: a clearance class is an `i32`
/// because `Item::clearance_class` is, and out-of-range indices must answer `0`
/// ("no clearance required"), reproducing `ClearanceMatrix.getValue`'s silent
/// out-of-range behaviour (`drc-connectivity.md` §1.2, §7.2).
///
/// Neither method adds the `clearance_safety_margin = 16`. That margin belongs
/// to the *candidate query* and to the router's insertion-time predicates, never
/// to the referee's verdict (`drc-connectivity.md` §10.1).
pub trait ClearanceSource {
    /// `matrix[class_a][class_b][layer]`, no safety margin.
    fn value(&self, class_a: i32, class_b: i32, layer: i32) -> i32;

    /// `max_j matrix[class_a][j][layer]` — the bound that sizes the candidate
    /// query. Any value `>=` the true row maximum keeps the query a superset;
    /// returning something smaller silently drops violations.
    fn max_value_for_class(&self, class_a: i32, layer: i32) -> i32;
}

impl ClearanceSource for ClearanceMatrix {
    fn value(&self, class_a: i32, class_b: i32, layer: i32) -> i32 {
        if class_a < 0 || class_b < 0 || layer < 0 {
            return 0;
        }
        self.get_value(class_a as usize, class_b as usize, layer as usize, false)
    }

    fn max_value_for_class(&self, class_a: i32, layer: i32) -> i32 {
        if class_a < 0 || layer < 0 {
            return 0;
        }
        // `ClearanceMatrix::max_value_for_class` clamps out-of-range indices to
        // the last class / layer instead of answering 0, which is the safe
        // direction for a candidate bound but would under-report for a class
        // index past the end, so fall back to the board-wide maximum there.
        if class_a as usize >= self.class_count() || layer as usize >= self.layer_count() {
            return self.max_value_on_layer(layer.max(0) as usize);
        }
        self.max_value_for_class(class_a as usize, layer as usize)
    }
}

/// One clearance for every pair of classes on every layer.
///
/// The degenerate matrix. Useful for tests and for boards whose rules have not
/// been imported yet; `UniformClearance(0)` disables clearance checking without
/// disabling the rest of the referee.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UniformClearance(pub i32);

impl ClearanceSource for UniformClearance {
    fn value(&self, _class_a: i32, _class_b: i32, _layer: i32) -> i32 {
        self.0
    }

    fn max_value_for_class(&self, _class_a: i32, _layer: i32) -> i32 {
        self.0
    }
}

/// What the referee needs from a board.
///
/// Implementors must be *pure*: two calls with the same arguments answer the
/// same thing, and [`item_ids`](BoardView::item_ids) is a stable, deterministic
/// order. The referee's output ordering is canonicalised regardless, but its
/// tie-breaking (which of two equivalent evaluations of an asymmetric predicate
/// is recorded) follows this order.
pub trait BoardView {
    /// Every item on the board, in a deterministic order.
    ///
    /// Upstream's whole-board DRC walks `board.getItems()`, which includes the
    /// board outline and every keepout — so must this.
    fn item_ids(&self) -> &[ItemId];

    /// The item with the given id.
    ///
    /// # Panics
    /// May panic for an id not in [`item_ids`](BoardView::item_ids).
    fn item(&self, id: ItemId) -> &Item;

    /// The item's convex tile shapes, in index order.
    ///
    /// An empty slice is upstream's `tileShapeCount() == 0`: the item exists but
    /// is invisible to both the search tree and the referee, which is exactly
    /// what a `ComponentOutline` and a degenerate area are.
    fn shapes(&self, id: ItemId) -> &[ItemShape];

    /// Number of copper layers. Only used to bound layer loops.
    fn layer_count(&self) -> usize;

    /// The required clearance between two classes on a layer, no safety margin.
    fn clearance(&self, class_a: i32, class_b: i32, layer: i32) -> i32;

    /// The candidate-query bound; see
    /// [`ClearanceSource::max_value_for_class`].
    fn max_clearance(&self, class_a: i32, layer: i32) -> i32;
}

/// Per-item tile shapes in one flat arena.
///
/// Built once by whoever knows how to derive tile shapes, then borrowed by the
/// referee. The arena keeps every item's shapes contiguous so
/// [`BoardView::shapes`] is a slice and the DRC sweep streams.
#[derive(Debug, Clone, Default)]
pub struct ShapeStore {
    shapes: Vec<ItemShape>,
    /// `(id, start, len)`, kept sorted ascending by id so lookup is a binary
    /// search. Ids are handed out monotonically, so the usual insert is a push.
    ranges: Vec<(ItemId, u32, u32)>,
}

impl ShapeStore {
    /// An empty store.
    pub fn new() -> Self {
        ShapeStore::default()
    }

    /// Records every tile shape of one item. Call at most once per item.
    ///
    /// # Panics
    /// Panics if `id` already has shapes recorded.
    pub fn set(&mut self, id: ItemId, shapes: impl IntoIterator<Item = ItemShape>) {
        let at = match self.ranges.binary_search_by_key(&id, |(id, _, _)| *id) {
            Ok(_) => panic!("ShapeStore::set called twice for {id}"),
            Err(at) => at,
        };
        let start = self.shapes.len() as u32;
        self.shapes.extend(shapes);
        let len = self.shapes.len() as u32 - start;
        self.ranges.insert(at, (id, start, len));
    }

    /// The tile shapes of `id`, or an empty slice if it has none.
    pub fn get(&self, id: ItemId) -> &[ItemShape] {
        match self.ranges.binary_search_by_key(&id, |(id, _, _)| *id) {
            Ok(i) => {
                let (_, start, len) = self.ranges[i];
                &self.shapes[start as usize..(start + len) as usize]
            }
            Err(_) => &[],
        }
    }

    /// Total number of tile shapes on the board.
    pub fn len(&self) -> usize {
        self.shapes.len()
    }

    /// True when no shape has been recorded.
    pub fn is_empty(&self) -> bool {
        self.shapes.is_empty()
    }
}

/// The [`BoardView`] over the new [`ItemStore`] model.
///
/// This is the adapter the rest of the port uses: an item store, the tile shapes
/// somebody derived for it, and a clearance source. When the search-tree manager
/// lands it takes over the [`ShapeStore`]'s role and this type either wraps it or
/// is replaced by a `BoardView` impl on the board itself — either way no caller
/// of [`check_clearance`](crate::check_clearance) changes.
pub struct StoreView<'a, C: ClearanceSource> {
    store: &'a ItemStore,
    shapes: &'a ShapeStore,
    clearance: &'a C,
    layer_count: usize,
    /// `item_ids()` needs to hand out a slice, and [`ItemStore`] exposes its
    /// order as an iterator, so the order is materialised once here. Descending
    /// by id, matching upstream's `Item.compareTo`.
    ids: Vec<ItemId>,
}

impl<'a, C: ClearanceSource> StoreView<'a, C> {
    /// Wraps a store, its tile shapes and its clearance rules.
    pub fn new(
        store: &'a ItemStore,
        shapes: &'a ShapeStore,
        clearance: &'a C,
        layer_count: usize,
    ) -> Self {
        StoreView {
            ids: store.ids().collect(),
            store,
            shapes,
            clearance,
            layer_count,
        }
    }

    /// The wrapped store.
    pub fn store(&self) -> &'a ItemStore {
        self.store
    }
}

impl<C: ClearanceSource> BoardView for StoreView<'_, C> {
    fn item_ids(&self) -> &[ItemId] {
        &self.ids
    }

    fn item(&self, id: ItemId) -> &Item {
        self.store
            .get(id)
            .unwrap_or_else(|| panic!("StoreView: no item {id}"))
    }

    fn shapes(&self, id: ItemId) -> &[ItemShape] {
        self.shapes.get(id)
    }

    fn layer_count(&self) -> usize {
        self.layer_count
    }

    fn clearance(&self, class_a: i32, class_b: i32, layer: i32) -> i32 {
        self.clearance.value(class_a, class_b, layer)
    }

    fn max_clearance(&self, class_a: i32, layer: i32) -> i32 {
        self.clearance.max_value_for_class(class_a, layer)
    }
}

/// A dense `ItemId -> index` map over a [`BoardView`]'s item list.
///
/// The referee keeps several per-item side arrays (contact counts, tail flags,
/// union-find parents); indexing them by raw [`ItemId`] would need a sparse map,
/// so ids are densified once against `view.item_ids()`. The dense index of an
/// item is its position in `view.item_ids()`.
#[derive(Debug, Clone)]
pub struct IdIndex {
    /// `(id, dense index)` sorted ascending by id.
    sorted: Vec<(ItemId, u32)>,
}

impl IdIndex {
    /// Densifies a view's item list.
    pub fn build(ids: &[ItemId]) -> Self {
        let mut sorted: Vec<(ItemId, u32)> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| (*id, i as u32))
            .collect();
        sorted.sort_unstable_by_key(|(id, _)| *id);
        IdIndex { sorted }
    }

    /// The dense index of `id`, i.e. its position in `view.item_ids()`.
    #[inline]
    pub fn dense(&self, id: ItemId) -> Option<usize> {
        self.sorted
            .binary_search_by_key(&id, |(id, _)| *id)
            .ok()
            .map(|i| self.sorted[i].1 as usize)
    }

    /// Number of items indexed.
    pub fn len(&self) -> usize {
        self.sorted.len()
    }

    /// True when no item is indexed.
    pub fn is_empty(&self) -> bool {
        self.sorted.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fr_geometry::planar::IntBox;

    #[test]
    fn shape_store_returns_an_empty_slice_for_an_unknown_item() {
        let mut s = ShapeStore::new();
        s.set(
            ItemId::from_raw(4),
            [ItemShape::new(IntBox::new(0, 0, 1, 1), 0)],
        );
        assert_eq!(s.get(ItemId::from_raw(4)).len(), 1);
        assert!(s.get(ItemId::from_raw(5)).is_empty());
    }

    #[test]
    fn uniform_clearance_answers_the_same_everywhere() {
        let c = UniformClearance(40);
        assert_eq!(c.value(1, 2, 0), 40);
        assert_eq!(c.value(9, 9, 7), 40);
        assert_eq!(c.max_value_for_class(3, 1), 40);
    }

    #[test]
    fn out_of_range_clearance_classes_read_as_zero() {
        let m = ClearanceMatrix::default_instance(
            fr_rules::LayerStructure::new(vec![fr_rules::Layer::new("signal", true)]),
            30,
        );
        assert_eq!(ClearanceSource::value(&m, 1, 1, 0), 30);
        assert_eq!(ClearanceSource::value(&m, -1, 1, 0), 0);
        assert_eq!(ClearanceSource::value(&m, 1, 99, 0), 0);
        assert_eq!(ClearanceSource::value(&m, 1, 1, 5), 0);
    }
}
