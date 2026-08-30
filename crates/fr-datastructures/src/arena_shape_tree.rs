//! Arena-backed shape tree — the spatial index that replaces [`MinAreaTree`].
//!
//! Ported from `app.freerouting.datastructures.ShapeTree` / `MinAreaTree`
//! following the Rust design in `docs/upstream-study/search-tree-obstacles.md`
//! §1 (upstream structure) and §10 (restructuring notes).
//!
//! Differences from the pointer-linked Java original:
//!
//! * **Slab arena.** All nodes live in one `Vec<Node>`; children/parent are
//!   `u32` slot indices, freed slots are chained on a free list. The 8-int
//!   octagon bounds are stored inline, so a descent touches one cache line per
//!   node instead of chasing an `IntOctagon` object per node.
//! * **One bounds type.** Bounds are always the 8-parameter [`OctBounds`]
//!   octagon; an axis-parallel box is the degenerate octagon whose diagonal
//!   intercepts are tight against its corners ([`OctBounds::from_box`]), so
//!   there is a single code path for the 45° and the 90° trees.
//! * **Zero-allocation queries.** The DFS stack is a per-query
//!   [`QueryStack`] local (or a caller-owned reusable one), never a member
//!   field, so every query takes `&self`: the tree is `Sync` and queries are
//!   re-entrant and parallel (upstream reuses a member `ArrayStack`, which
//!   makes one in-flight query per tree the hard limit — spec §8).
//! * **Stable, ABA-safe handles.** [`ArenaLeafId`] carries a generation
//!   counter that is bumped whenever a slot is freed, so a handle to a removed
//!   leaf can never resolve to the unrelated leaf that later reuses the slot.
//! * **Integer areas.** The minimum-area-increase heuristic is evaluated in
//!   exact `i64` (checked promotion to `i128` for coordinates outside the
//!   `Limits::CRIT_INT` range), never `f64`, and without materializing the
//!   union shapes.
//! * **Bulk load.** [`ArenaShapeTree::bulk_load`] packs a full item list into a
//!   balanced tree (sort-tile-recursive, specialized to fan-out 2) instead of
//!   replaying the greedy insertion heuristic in board order — which is the
//!   worst case for that heuristic. This is behavior-safe: the tree shape only
//!   affects the *order* in which hits are reported, and callers must sort
//!   results by `(id, shape_index)` for determinism anyway (spec §10).
//!
//! What is kept verbatim from upstream: the minimum-area-increase descent with
//! bounds grown on the way down, the splice-a-new-inner-node insertion, the
//! sibling-promotion removal, and the lazy bound tightening that stops walking
//! up as soon as the recomputed bounds no longer shrink.
//!
//! [`MinAreaTree`]: crate::min_area_tree::MinAreaTree

/// Sentinel for "no node" in the `u32` slot links.
const NONE: u32 = u32::MAX;

// Indices into [`OctBounds::params`], in the upstream `IntOctagon` constructor
// order `(lx, ly, rx, uy, ulx, lrx, llx, urx)`.
const LX: usize = 0;
const LY: usize = 1;
const RX: usize = 2;
const UY: usize = 3;
const ULX: usize = 4;
const LRX: usize = 5;
const LLX: usize = 6;
const URX: usize = 7;

/// Mirror of `fr_geometry::planar::Limits::CRIT_INT` (2^25).
///
/// `fr-geometry` depends on this crate, so the constant is duplicated here
/// rather than imported. It is only used for [`OctBounds::EMPTY`] and to
/// document the range in which the `i64` area fast path applies; nothing here
/// clamps coordinates to it.
const CRIT_INT: i32 = 33_554_432;

/// Largest half-extent for which the `i64` area fast path cannot overflow.
///
/// With `w, h <= 2^30` every product is below `2^60` and the five accumulated
/// terms stay below `2^63`. Real board coordinates are bounded by `CRIT_INT`
/// (2^25), so the `i128` fallback is unreachable in practice.
const AREA_FAST_PATH_LIMIT: i64 = 1 << 30;

/// The 8-parameter bounding octagon used by every node of the tree.
///
/// The parameters are exactly the upstream `IntOctagon` ones (spec §1.2), in
/// the same order, so converting from `fr_geometry::planar::IntOctagon` is a
/// field-by-field call to [`OctBounds::new`]:
///
/// ```text
/// lx  <= x         <= rx        left / right vertical borders
/// ly  <= y         <= uy        bottom / top horizontal borders
/// ulx <= x - y     <= lrx       upper-left / lower-right 45° borders
/// llx <= x + y     <= urx       lower-left / upper-right 135° borders
/// ```
///
/// `IntOctagon` itself cannot be reused here: `fr-geometry` depends on this
/// crate (for `Signum` and `BigIntAux`), so the dependency cannot point the
/// other way. The parameter order is kept identical instead, which makes the
/// conversion in the board layer a single call:
///
/// ```text
/// OctBounds::new(
///     o.left_x, o.bottom_y, o.right_x, o.top_y,
///     o.upper_left_diagonal_x, o.lower_right_diagonal_x,
///     o.lower_left_diagonal_x, o.upper_right_diagonal_x,
/// )
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OctBounds {
    params: [i32; 8],
}

impl OctBounds {
    /// The empty (inverted) octagon, matching upstream `IntOctagon.EMPTY`.
    pub const EMPTY: OctBounds = OctBounds {
        params: [
            CRIT_INT, CRIT_INT, -CRIT_INT, -CRIT_INT, CRIT_INT, -CRIT_INT, CRIT_INT, -CRIT_INT,
        ],
    };

    /// Creates bounds from the 8 octagon parameters.
    ///
    /// The argument order matches upstream `IntOctagon(lx, ly, rx, uy, ulx,
    /// lrx, llx, urx)`.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        lx: i32,
        ly: i32,
        rx: i32,
        uy: i32,
        ulx: i32,
        lrx: i32,
        llx: i32,
        urx: i32,
    ) -> Self {
        OctBounds {
            params: [lx, ly, rx, uy, ulx, lrx, llx, urx],
        }
    }

    /// Creates bounds from the raw parameter array
    /// `[lx, ly, rx, uy, ulx, lrx, llx, urx]`.
    #[inline]
    pub const fn from_params(params: [i32; 8]) -> Self {
        OctBounds { params }
    }

    /// Creates the degenerate octagon that equals the axis-parallel box
    /// `(lx, ly) .. (rx, uy)`, with the diagonal intercepts tight against the
    /// box corners (upstream `IntBox.to_IntOctagon`).
    #[inline]
    pub const fn from_box(lx: i32, ly: i32, rx: i32, uy: i32) -> Self {
        OctBounds::new(lx, ly, rx, uy, lx - uy, rx - ly, lx + ly, rx + uy)
    }

    /// Returns the raw parameter array `[lx, ly, rx, uy, ulx, lrx, llx, urx]`.
    #[inline]
    pub const fn params(&self) -> [i32; 8] {
        self.params
    }

    /// x of the left vertical border.
    #[inline]
    pub const fn lx(&self) -> i32 {
        self.params[LX]
    }

    /// y of the bottom horizontal border.
    #[inline]
    pub const fn ly(&self) -> i32 {
        self.params[LY]
    }

    /// x of the right vertical border.
    #[inline]
    pub const fn rx(&self) -> i32 {
        self.params[RX]
    }

    /// y of the top horizontal border.
    #[inline]
    pub const fn uy(&self) -> i32 {
        self.params[UY]
    }

    /// Lower bound of `x - y` (upper-left 45° border).
    #[inline]
    pub const fn ulx(&self) -> i32 {
        self.params[ULX]
    }

    /// Upper bound of `x - y` (lower-right 45° border).
    #[inline]
    pub const fn lrx(&self) -> i32 {
        self.params[LRX]
    }

    /// Lower bound of `x + y` (lower-left 135° border).
    #[inline]
    pub const fn llx(&self) -> i32 {
        self.params[LLX]
    }

    /// Upper bound of `x + y` (upper-right 135° border).
    #[inline]
    pub const fn urx(&self) -> i32 {
        self.params[URX]
    }

    /// Returns `true` if any of the four bound pairs crosses.
    ///
    /// Like upstream `IntOctagon.is_empty` this is a *parameter* test, not a
    /// geometric one: an octagon whose parameters do not cross can still
    /// enclose no point.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.params[LX] > self.params[RX]
            || self.params[LY] > self.params[UY]
            || self.params[ULX] > self.params[LRX]
            || self.params[LLX] > self.params[URX]
    }

    /// Returns `true` if the two octagons overlap.
    ///
    /// This is upstream's 8-compare, short-circuiting test (`intersection`
    /// componentwise, then `is_empty`): conservative in the same way upstream
    /// is — it can report an overlap for two octagons whose intersection is
    /// geometrically empty but whose parameter intervals all overlap. Broad
    /// phase only; exact shape tests happen in the board layer.
    #[inline]
    pub fn intersects(&self, other: &OctBounds) -> bool {
        let a = &self.params;
        let b = &other.params;
        a[LX] <= b[RX]
            && b[LX] <= a[RX]
            && a[LY] <= b[UY]
            && b[LY] <= a[UY]
            && a[ULX] <= b[LRX]
            && b[ULX] <= a[LRX]
            && a[LLX] <= b[URX]
            && b[LLX] <= a[URX]
    }

    /// Componentwise union — the smallest octagon of this family containing
    /// both operands.
    #[inline]
    pub fn union(&self, other: &OctBounds) -> OctBounds {
        let a = &self.params;
        let b = &other.params;
        OctBounds {
            params: [
                a[LX].min(b[LX]),
                a[LY].min(b[LY]),
                a[RX].max(b[RX]),
                a[UY].max(b[UY]),
                a[ULX].min(b[ULX]),
                a[LRX].max(b[LRX]),
                a[LLX].min(b[LLX]),
                a[URX].max(b[URX]),
            ],
        }
    }

    /// Returns `true` if `self` contains `other` (componentwise).
    #[inline]
    pub fn contains(&self, other: &OctBounds) -> bool {
        let a = &self.params;
        let b = &other.params;
        a[LX] <= b[LX]
            && a[LY] <= b[LY]
            && a[RX] >= b[RX]
            && a[UY] >= b[UY]
            && a[ULX] <= b[ULX]
            && a[LRX] >= b[LRX]
            && a[LLX] <= b[LLX]
            && a[URX] >= b[URX]
    }

    /// Twice the area of the octagon, as an exact integer.
    ///
    /// Twice the area is used so the result is integral (the four clipped
    /// corners are right triangles with half-integer areas). Only *ordering*
    /// of areas matters for the insertion heuristic, and doubling preserves it
    /// while keeping the whole heuristic in integer arithmetic — `f64` never
    /// participates in tree construction.
    ///
    /// The area is computed as `box - the four clipped corners` instead of by
    /// the shoelace formula: the legs stay bounded by the box extent, which
    /// keeps the `i64` fast path valid.
    #[inline]
    pub fn area2(&self) -> i64 {
        area2_of(&self.params)
    }

    /// Twice the area of `self ∪ other`, without materializing the union.
    #[inline]
    pub fn union_area2(&self, other: &OctBounds) -> i64 {
        let a = &self.params;
        let b = &other.params;
        area2_of(&[
            a[LX].min(b[LX]),
            a[LY].min(b[LY]),
            a[RX].max(b[RX]),
            a[UY].max(b[UY]),
            a[ULX].min(b[ULX]),
            a[LRX].max(b[LRX]),
            a[LLX].min(b[LLX]),
            a[URX].max(b[URX]),
        ])
    }

    /// Twice the area increase of growing `self` to contain `other` — the
    /// upstream `position_locate` descent key.
    #[inline]
    pub fn area_increase2(&self, other: &OctBounds) -> i64 {
        self.union_area2(other).saturating_sub(self.area2())
    }

    /// Sum of the left and right border x, i.e. twice the x of the bounding
    /// box center. Used as an integer sort key by the bulk loader.
    #[inline]
    fn center_x2(&self) -> i64 {
        self.params[LX] as i64 + self.params[RX] as i64
    }

    /// Sum of the bottom and top border y, i.e. twice the y of the bounding
    /// box center. Used as an integer sort key by the bulk loader.
    #[inline]
    fn center_y2(&self) -> i64 {
        self.params[LY] as i64 + self.params[UY] as i64
    }
}

/// Twice the octagon area, `2*w*h - a² - b² - c² - d²` where `a..d` are the
/// legs of the four clipped corner triangles.
#[inline]
fn area2_of(p: &[i32; 8]) -> i64 {
    let w = p[RX] as i64 - p[LX] as i64;
    let h = p[UY] as i64 - p[LY] as i64;
    if w <= 0 || h <= 0 {
        return 0;
    }
    let max_leg = w.min(h);
    // Corner cut legs, clamped so that non-normalized parameters (redundant or
    // overshooting diagonal borders) cannot produce a negative or wrapping
    // area.
    let ll = clamp_leg(p[LLX] as i64 - (p[LX] as i64 + p[LY] as i64), max_leg);
    let lr = clamp_leg((p[RX] as i64 - p[LY] as i64) - p[LRX] as i64, max_leg);
    let ur = clamp_leg((p[RX] as i64 + p[UY] as i64) - p[URX] as i64, max_leg);
    let ul = clamp_leg(p[ULX] as i64 - (p[LX] as i64 - p[UY] as i64), max_leg);

    if w <= AREA_FAST_PATH_LIMIT && h <= AREA_FAST_PATH_LIMIT {
        // i64 fast path: every term is below 2^60.
        (2 * w * h - ll * ll - lr * lr - ur * ur - ul * ul).max(0)
    } else {
        // Checked promotion for coordinates far outside the board range.
        let (w, h) = (w as i128, h as i128);
        let (ll, lr, ur, ul) = (ll as i128, lr as i128, ur as i128, ul as i128);
        let area = 2 * w * h - ll * ll - lr * lr - ur * ur - ul * ul;
        area.clamp(0, i64::MAX as i128) as i64
    }
}

#[inline]
fn clamp_leg(leg: i64, max_leg: i64) -> i64 {
    leg.clamp(0, max_leg)
}

/// Stable handle to a leaf of an [`ArenaShapeTree`].
///
/// The generation counter makes slot reuse safe: after the leaf is removed the
/// slot's generation is bumped, so an old handle never resolves to whatever
/// leaf (or inner node) later occupies the same slot. `MinAreaTree`'s plain
/// `LeafId(usize)` has exactly that ABA hazard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArenaLeafId {
    generation: u32,
    index: u32,
}

impl ArenaLeafId {
    /// A handle that never resolves to a live leaf.
    pub const INVALID: ArenaLeafId = ArenaLeafId {
        generation: 0,
        index: NONE,
    };

    /// Slot index of the leaf inside the arena.
    ///
    /// Not a semantic identity: slots are reused. Do not use it as a sort key.
    #[inline]
    pub const fn slot(&self) -> u32 {
        self.index
    }

    /// Generation stamp of the slot at the time the handle was issued.
    #[inline]
    pub const fn generation(&self) -> u32 {
        self.generation
    }
}

/// What a slab slot currently holds.
#[derive(Debug, Clone, Copy)]
enum NodeKind<P: Copy> {
    Inner { left: u32, right: u32 },
    Leaf(P),
    Free { next: u32 },
}

/// One slab slot: bounds first, so a descent that rejects the node touches
/// only the leading 32 bytes.
#[derive(Debug, Clone, Copy)]
struct Node<P: Copy> {
    bounds: OctBounds,
    parent: u32,
    generation: u32,
    kind: NodeKind<P>,
}

/// Reusable depth-first search stack for [`ArenaShapeTree`] queries.
///
/// Holds `INLINE_CAPACITY` slot indices inline and spills to the heap only for
/// trees deeper than that (the DFS stack of a binary tree never exceeds
/// `height + 1` entries). Queries take it by `&mut`, so the tree itself stays
/// `&self` and shareable across threads.
#[derive(Debug, Clone)]
pub struct QueryStack {
    inline: [u32; QueryStack::INLINE_CAPACITY],
    len: usize,
    spill: Vec<u32>,
}

impl Default for QueryStack {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryStack {
    /// Number of entries held without touching the heap.
    pub const INLINE_CAPACITY: usize = 64;

    /// Creates an empty stack.
    #[inline]
    pub fn new() -> Self {
        QueryStack {
            inline: [0; Self::INLINE_CAPACITY],
            len: 0,
            spill: Vec::new(),
        }
    }

    /// Empties the stack, keeping any heap capacity already acquired.
    #[inline]
    pub fn clear(&mut self) {
        self.len = 0;
        self.spill.clear();
    }

    #[inline]
    fn push(&mut self, value: u32) {
        if self.len < Self::INLINE_CAPACITY {
            self.inline[self.len] = value;
            self.len += 1;
        } else {
            self.spill.push(value);
        }
    }

    #[inline]
    fn pop(&mut self) -> Option<u32> {
        if let Some(value) = self.spill.pop() {
            return Some(value);
        }
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        Some(self.inline[self.len])
    }
}

/// Depth statistics of a tree, the integer form of upstream
/// `ShapeTree.statistics()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DepthStats {
    /// Number of leaves reached.
    pub leaves: usize,
    /// Depth of the deepest leaf (root = depth 0).
    pub max_depth: usize,
    /// Sum of all leaf depths; divide by `leaves` for the average.
    pub sum_depth: u64,
}

/// Binary bounding-octagon tree with data only in the leaves.
///
/// Generic over a `Copy` leaf payload; the board stores `(ItemId, shape_index)`
/// there. See the module docs for how it differs from the upstream
/// pointer-linked tree.
#[derive(Debug, Clone)]
pub struct ArenaShapeTree<P: Copy> {
    nodes: Vec<Node<P>>,
    free_head: u32,
    root: u32,
    leaf_count: usize,
    epoch: u64,
}

impl<P: Copy> Default for ArenaShapeTree<P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P: Copy> ArenaShapeTree<P> {
    /// Creates an empty tree.
    pub fn new() -> Self {
        ArenaShapeTree {
            nodes: Vec::new(),
            free_head: NONE,
            root: NONE,
            leaf_count: 0,
            epoch: 0,
        }
    }

    /// Creates an empty tree with slab capacity for `leaf_capacity` leaves
    /// (a tree with `n` leaves has exactly `2n - 1` nodes).
    pub fn with_leaf_capacity(leaf_capacity: usize) -> Self {
        let mut tree = Self::new();
        tree.nodes.reserve(2 * leaf_capacity.max(1) - 1);
        tree
    }

    /// Number of leaves stored.
    #[inline]
    pub fn len(&self) -> usize {
        self.leaf_count
    }

    /// Returns `true` if the tree holds no leaves.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.leaf_count == 0
    }

    /// Mutation counter, incremented by every insert, remove and clear.
    ///
    /// The concurrency scheme on top of it lands later; the contract fixed
    /// here is that reads are `&self` only and that any mutation is visible as
    /// an epoch change (spec §10, "epoch-scoped immutability").
    #[inline]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Number of slab slots currently in use (leaves plus inner nodes).
    #[inline]
    pub fn node_count(&self) -> usize {
        if self.leaf_count == 0 {
            0
        } else {
            2 * self.leaf_count - 1
        }
    }

    /// Number of slab slots allocated, including free ones.
    #[inline]
    pub fn slab_len(&self) -> usize {
        self.nodes.len()
    }

    /// Removes every leaf, keeping the slab capacity.
    ///
    /// All outstanding [`ArenaLeafId`]s are invalidated: every slot's
    /// generation is bumped, so no stale handle survives the clear.
    pub fn clear(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.free_head = NONE;
        self.root = NONE;
        self.leaf_count = 0;
        for idx in (0..self.nodes.len()).rev() {
            let next = self.free_head;
            let node = &mut self.nodes[idx];
            node.generation = node.generation.wrapping_add(1);
            node.parent = NONE;
            node.bounds = OctBounds::EMPTY;
            node.kind = NodeKind::Free { next };
            self.free_head = idx as u32;
        }
    }

    // ---------------------------------------------------------------- slab

    fn alloc(&mut self, bounds: OctBounds, parent: u32, kind: NodeKind<P>) -> u32 {
        if self.free_head != NONE {
            let idx = self.free_head as usize;
            let next = match self.nodes[idx].kind {
                NodeKind::Free { next } => next,
                _ => unreachable!("free list slot is not free"),
            };
            self.free_head = next;
            let generation = self.nodes[idx].generation;
            self.nodes[idx] = Node {
                bounds,
                parent,
                generation,
                kind,
            };
            idx as u32
        } else {
            let idx = self.nodes.len();
            assert!(
                idx < NONE as usize,
                "arena shape tree exceeded the u32 slot space"
            );
            self.nodes.push(Node {
                bounds,
                parent,
                generation: 0,
                kind,
            });
            idx as u32
        }
    }

    /// Returns the slot to the free list, bumping its generation so every
    /// handle into it becomes stale.
    fn free(&mut self, idx: u32) {
        let next = self.free_head;
        let node = &mut self.nodes[idx as usize];
        node.generation = node.generation.wrapping_add(1);
        node.parent = NONE;
        node.bounds = OctBounds::EMPTY;
        node.kind = NodeKind::Free { next };
        self.free_head = idx;
    }

    #[inline]
    fn leaf_id(&self, idx: u32) -> ArenaLeafId {
        ArenaLeafId {
            index: idx,
            generation: self.nodes[idx as usize].generation,
        }
    }

    #[inline]
    fn resolve(&self, id: ArenaLeafId) -> Option<&Node<P>> {
        let node = self.nodes.get(id.index as usize)?;
        if node.generation != id.generation {
            return None;
        }
        match node.kind {
            NodeKind::Leaf(_) => Some(node),
            _ => None,
        }
    }

    /// Returns the payload of a live leaf, or `None` if the handle is stale.
    #[inline]
    pub fn get(&self, id: ArenaLeafId) -> Option<P> {
        self.resolve(id).map(|node| match node.kind {
            NodeKind::Leaf(payload) => payload,
            _ => unreachable!(),
        })
    }

    /// Replaces the payload of a live leaf **without moving it**, returning the
    /// old payload; `None` — and no change — if the handle is stale.
    ///
    /// This is upstream's `leaf.object = p_to_trace` /
    /// `leaf.shape_index_in_object = new_index` (`ShapeSearchTree.java:155-290`,
    /// the `merge_entries_*` family), which relabels a leaf that already sits at
    /// the right place in the tree rather than removing and re-inserting it. The
    /// bounds are deliberately not touched: relabelling is only ever correct
    /// when the shape the leaf stands for is *the same shape*, so its bounds are
    /// the same too. A payload change that also moves the shape must go through
    /// [`remove`](Self::remove) + [`insert`](Self::insert).
    ///
    /// The mutation is visible in [`epoch`](Self::epoch), as every mutation is.
    #[inline]
    pub fn set_payload(&mut self, id: ArenaLeafId, payload: P) -> Option<P> {
        let node = self.nodes.get_mut(id.index as usize)?;
        if node.generation != id.generation {
            return None;
        }
        let NodeKind::Leaf(old) = node.kind else {
            return None;
        };
        node.kind = NodeKind::Leaf(payload);
        self.epoch = self.epoch.wrapping_add(1);
        Some(old)
    }

    /// Returns the bounds a live leaf was inserted with.
    #[inline]
    pub fn leaf_bounds(&self, id: ArenaLeafId) -> Option<OctBounds> {
        self.resolve(id).map(|node| node.bounds)
    }

    /// Returns `true` if the handle still refers to a live leaf.
    #[inline]
    pub fn contains_leaf(&self, id: ArenaLeafId) -> bool {
        self.resolve(id).is_some()
    }

    // ------------------------------------------------------------- mutation

    /// Inserts a leaf, descending into the child whose bounds grow least
    /// (upstream `MinAreaTree.insert_leaf` / `position_locate`).
    ///
    /// Ties go to the first child, matching upstream's `<=` comparison.
    pub fn insert(&mut self, bounds: OctBounds, payload: P) -> ArenaLeafId {
        self.epoch = self.epoch.wrapping_add(1);
        self.leaf_count += 1;

        if self.root == NONE {
            let idx = self.alloc(bounds, NONE, NodeKind::Leaf(payload));
            self.root = idx;
            return self.leaf_id(idx);
        }

        // Descend to the victim leaf, growing the bounds on the way down.
        let mut curr = self.root;
        loop {
            let (left, right) = match self.nodes[curr as usize].kind {
                NodeKind::Inner { left, right } => (left, right),
                NodeKind::Leaf(_) => break,
                NodeKind::Free { .. } => unreachable!("free slot reached during descent"),
            };
            let increase_left = self.nodes[left as usize].bounds.area_increase2(&bounds);
            let increase_right = self.nodes[right as usize].bounds.area_increase2(&bounds);

            let node = &mut self.nodes[curr as usize];
            node.bounds = node.bounds.union(&bounds);

            curr = if increase_left <= increase_right {
                left
            } else {
                right
            };
        }

        // Splice a new inner node in place of the victim leaf.
        let victim = curr;
        let victim_parent = self.nodes[victim as usize].parent;
        let merged = self.nodes[victim as usize].bounds.union(&bounds);

        let leaf = self.alloc(bounds, NONE, NodeKind::Leaf(payload));
        let inner = self.alloc(
            merged,
            victim_parent,
            NodeKind::Inner {
                left: victim,
                right: leaf,
            },
        );
        self.nodes[victim as usize].parent = inner;
        self.nodes[leaf as usize].parent = inner;

        if victim_parent == NONE {
            self.root = inner;
        } else {
            self.replace_child(victim_parent, victim, inner);
        }

        self.leaf_id(leaf)
    }

    fn replace_child(&mut self, parent: u32, old_child: u32, new_child: u32) {
        match self.nodes[parent as usize].kind {
            NodeKind::Inner {
                ref mut left,
                ref mut right,
            } => {
                if *left == old_child {
                    *left = new_child;
                } else {
                    debug_assert_eq!(*right, old_child);
                    *right = new_child;
                }
            }
            _ => unreachable!("parent slot is not an inner node"),
        }
    }

    /// Removes a leaf, returning its payload; `None` for a stale handle.
    ///
    /// The sibling is promoted into the parent's place and the ancestors are
    /// re-tightened bottom-up, stopping as soon as the recomputed bounds stop
    /// shrinking (upstream `MinAreaTree.remove_leaf`, spec §1.4).
    pub fn remove(&mut self, id: ArenaLeafId) -> Option<P> {
        let payload = self.get(id)?;
        self.epoch = self.epoch.wrapping_add(1);
        self.leaf_count -= 1;

        let idx = id.index;
        let parent = self.nodes[idx as usize].parent;
        self.free(idx);

        if parent == NONE {
            self.root = NONE;
            return Some(payload);
        }

        let (left, right) = match self.nodes[parent as usize].kind {
            NodeKind::Inner { left, right } => (left, right),
            _ => unreachable!("leaf parent is not an inner node"),
        };
        let sibling = if left == idx { right } else { left };
        let grand_parent = self.nodes[parent as usize].parent;

        self.nodes[sibling as usize].parent = grand_parent;
        if grand_parent == NONE {
            self.root = sibling;
        } else {
            self.replace_child(grand_parent, parent, sibling);
        }
        self.free(parent);

        // Lazy tightening: walk up while the bounds actually shrink.
        let mut curr = grand_parent;
        while curr != NONE {
            let (left, right) = match self.nodes[curr as usize].kind {
                NodeKind::Inner { left, right } => (left, right),
                _ => break,
            };
            let tightened = self.nodes[left as usize]
                .bounds
                .union(&self.nodes[right as usize].bounds);
            if tightened.contains(&self.nodes[curr as usize].bounds) {
                break;
            }
            self.nodes[curr as usize].bounds = tightened;
            curr = self.nodes[curr as usize].parent;
        }

        Some(payload)
    }

    // --------------------------------------------------------------- queries

    /// Calls `visit` for every leaf whose bounds overlap `query`.
    ///
    /// The zero-allocation query primitive: `stack` is caller-owned and reused,
    /// `visit` decides what to keep. Reports leaves in traversal order, which
    /// depends on the insertion history — callers that need determinism must
    /// sort by their own `(id, shape_index)` key (spec §10).
    pub fn visit_overlaps<F>(&self, query: &OctBounds, stack: &mut QueryStack, mut visit: F)
    where
        F: FnMut(ArenaLeafId, P),
    {
        stack.clear();
        if self.root == NONE {
            return;
        }
        let mut idx = self.root;
        if !self.nodes[idx as usize].bounds.intersects(query) {
            return;
        }
        // Descend into one child and defer the other, instead of pushing both
        // and popping one back: the common node stays in a register and only
        // nodes that actually intersect ever reach the stack.
        loop {
            let node = &self.nodes[idx as usize];
            match node.kind {
                NodeKind::Inner { left, right } => {
                    let hit_left = self.nodes[left as usize].bounds.intersects(query);
                    let hit_right = self.nodes[right as usize].bounds.intersects(query);
                    if hit_left {
                        if hit_right {
                            stack.push(right);
                        }
                        idx = left;
                        continue;
                    }
                    if hit_right {
                        idx = right;
                        continue;
                    }
                }
                NodeKind::Leaf(payload) => visit(
                    ArenaLeafId {
                        index: idx,
                        generation: node.generation,
                    },
                    payload,
                ),
                NodeKind::Free { .. } => unreachable!("free slot reached during query"),
            }
            match stack.pop() {
                Some(next) => idx = next,
                None => break,
            }
        }
    }

    /// Collects every leaf overlapping `query` into `out`, which is cleared
    /// first and reused, so a warmed-up caller allocates nothing per query.
    pub fn overlaps(&self, query: &OctBounds, out: &mut Vec<(ArenaLeafId, P)>) {
        let mut stack = QueryStack::new();
        self.overlaps_with_stack(query, &mut stack, out);
    }

    /// [`ArenaShapeTree::overlaps`] with a caller-owned DFS stack — fully
    /// allocation-free once both `stack` and `out` are warm.
    pub fn overlaps_with_stack(
        &self,
        query: &OctBounds,
        stack: &mut QueryStack,
        out: &mut Vec<(ArenaLeafId, P)>,
    ) {
        out.clear();
        self.visit_overlaps(query, stack, |id, payload| out.push((id, payload)));
    }

    /// Iterates every live leaf in slab order.
    pub fn iter_leaves(&self) -> impl Iterator<Item = (ArenaLeafId, OctBounds, P)> + '_ {
        self.nodes
            .iter()
            .enumerate()
            .filter_map(|(idx, node)| match node.kind {
                NodeKind::Leaf(payload) => Some((
                    ArenaLeafId {
                        index: idx as u32,
                        generation: node.generation,
                    },
                    node.bounds,
                    payload,
                )),
                _ => None,
            })
    }

    /// Bounds of the root, i.e. of the whole indexed set; `None` when empty.
    pub fn root_bounds(&self) -> Option<OctBounds> {
        if self.root == NONE {
            None
        } else {
            Some(self.nodes[self.root as usize].bounds)
        }
    }

    // ------------------------------------------------------------ bulk load

    /// Builds a balanced tree from a full item list.
    ///
    /// Sort-tile-recursive packing specialized to fan-out 2: at every node the
    /// group is split at the median center along its longer axis. Ties are
    /// broken by input index, so the result is deterministic.
    ///
    /// Preferred over replaying [`ArenaShapeTree::insert`] whenever a tree is
    /// built from scratch (`SearchTreeManager.get_autoroute_tree`): board order
    /// is the worst case for the greedy heuristic (spec §10).
    pub fn bulk_load(entries: &[(OctBounds, P)]) -> Self {
        let mut ids = Vec::new();
        Self::bulk_load_with_ids(entries, &mut ids)
    }

    /// [`ArenaShapeTree::bulk_load`], additionally reporting the leaf handles.
    ///
    /// `out_ids` is cleared and filled so that `out_ids[i]` is the handle of
    /// `entries[i]`.
    pub fn bulk_load_with_ids(entries: &[(OctBounds, P)], out_ids: &mut Vec<ArenaLeafId>) -> Self {
        out_ids.clear();
        out_ids.resize(entries.len(), ArenaLeafId::INVALID);

        let mut tree = ArenaShapeTree::new();
        if entries.is_empty() {
            return tree;
        }
        tree.nodes.reserve(2 * entries.len() - 1);
        tree.leaf_count = entries.len();

        let mut perm: Vec<u32> = (0..entries.len() as u32).collect();
        tree.root = tree.pack(entries, &mut perm, out_ids);
        tree
    }

    fn pack(
        &mut self,
        entries: &[(OctBounds, P)],
        perm: &mut [u32],
        out_ids: &mut [ArenaLeafId],
    ) -> u32 {
        debug_assert!(!perm.is_empty());
        if perm.len() == 1 {
            let entry = perm[0] as usize;
            let (bounds, payload) = entries[entry];
            let idx = self.alloc(bounds, NONE, NodeKind::Leaf(payload));
            out_ids[entry] = self.leaf_id(idx);
            return idx;
        }

        let mut group = entries[perm[0] as usize].0;
        for &i in &perm[1..] {
            group = group.union(&entries[i as usize].0);
        }
        let width = group.rx() as i64 - group.lx() as i64;
        let height = group.uy() as i64 - group.ly() as i64;
        if width >= height {
            perm.sort_unstable_by_key(|&i| (entries[i as usize].0.center_x2(), i));
        } else {
            perm.sort_unstable_by_key(|&i| (entries[i as usize].0.center_y2(), i));
        }

        let (head, tail) = perm.split_at_mut(perm.len() / 2);
        let left = self.pack(entries, head, out_ids);
        let right = self.pack(entries, tail, out_ids);

        let bounds = self.nodes[left as usize]
            .bounds
            .union(&self.nodes[right as usize].bounds);
        let idx = self.alloc(bounds, NONE, NodeKind::Inner { left, right });
        self.nodes[left as usize].parent = idx;
        self.nodes[right as usize].parent = idx;
        idx
    }

    // ------------------------------------------------------------ diagnostics

    /// Leaf depth statistics, the integer form of upstream
    /// `ShapeTree.statistics()`.
    pub fn depth_stats(&self) -> DepthStats {
        let mut stats = DepthStats {
            leaves: 0,
            max_depth: 0,
            sum_depth: 0,
        };
        if self.root == NONE {
            return stats;
        }
        let mut stack = vec![(self.root, 0usize)];
        while let Some((idx, depth)) = stack.pop() {
            match self.nodes[idx as usize].kind {
                NodeKind::Inner { left, right } => {
                    stack.push((left, depth + 1));
                    stack.push((right, depth + 1));
                }
                NodeKind::Leaf(_) => {
                    stats.leaves += 1;
                    stats.max_depth = stats.max_depth.max(depth);
                    stats.sum_depth += depth as u64;
                }
                NodeKind::Free { .. } => unreachable!("free slot reached during traversal"),
            }
        }
        stats
    }

    /// Checks the structural invariants; `Err` describes the first violation.
    ///
    /// `O(n)` debug helper used by the differential tests: every inner node has
    /// two children whose parent link points back, every inner node's bounds
    /// contain both children's bounds, the leaf count matches, and the free
    /// list is disjoint from the live tree.
    pub fn check_invariants(&self) -> Result<(), String> {
        let mut seen = vec![false; self.nodes.len()];
        let mut leaves = 0usize;
        let mut inner = 0usize;

        if self.root == NONE {
            if self.leaf_count != 0 {
                return Err(format!("empty root but leaf_count = {}", self.leaf_count));
            }
        } else {
            if self.nodes[self.root as usize].parent != NONE {
                return Err("root has a parent".to_string());
            }
            let mut stack = vec![self.root];
            while let Some(idx) = stack.pop() {
                let slot = idx as usize;
                if seen[slot] {
                    return Err(format!("slot {slot} reachable twice"));
                }
                seen[slot] = true;
                match self.nodes[slot].kind {
                    NodeKind::Inner { left, right } => {
                        inner += 1;
                        if left == right {
                            return Err(format!("inner slot {slot} has identical children"));
                        }
                        for child in [left, right] {
                            if child as usize >= self.nodes.len() {
                                return Err(format!("inner slot {slot} has out-of-range child"));
                            }
                            if self.nodes[child as usize].parent != idx {
                                return Err(format!(
                                    "child {child} of slot {slot} has parent {}",
                                    self.nodes[child as usize].parent
                                ));
                            }
                            if !self.nodes[slot]
                                .bounds
                                .contains(&self.nodes[child as usize].bounds)
                            {
                                return Err(format!(
                                    "bounds of slot {slot} do not contain child {child}"
                                ));
                            }
                            stack.push(child);
                        }
                    }
                    NodeKind::Leaf(_) => leaves += 1,
                    NodeKind::Free { .. } => {
                        return Err(format!("free slot {slot} is reachable from the root"))
                    }
                }
            }
        }

        if leaves != self.leaf_count {
            return Err(format!(
                "reachable leaves {leaves} but leaf_count {}",
                self.leaf_count
            ));
        }
        if leaves > 0 && inner + 1 != leaves {
            return Err(format!("{leaves} leaves but {inner} inner nodes"));
        }

        let mut free = self.free_head;
        let mut free_seen = 0usize;
        while free != NONE {
            let slot = free as usize;
            if slot >= self.nodes.len() {
                return Err("free list runs out of the slab".to_string());
            }
            if seen[slot] {
                return Err(format!("slot {slot} is both live and free"));
            }
            seen[slot] = true;
            free_seen += 1;
            if free_seen > self.nodes.len() {
                return Err("free list is cyclic".to_string());
            }
            free = match self.nodes[slot].kind {
                NodeKind::Free { next } => next,
                _ => return Err(format!("free list slot {slot} is not free")),
            };
        }
        if leaves + inner + free_seen != self.nodes.len() {
            return Err(format!(
                "{} slab slots but {leaves} leaves + {inner} inner + {free_seen} free",
                self.nodes.len()
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(lx: i32, ly: i32, rx: i32, uy: i32) -> OctBounds {
        OctBounds::from_box(lx, ly, rx, uy)
    }

    #[test]
    fn box_area_matches_the_octagon_formula() {
        // A degenerate octagon (a box) has no corner cuts.
        assert_eq!(square(0, 0, 10, 20).area2(), 2 * 10 * 20);
        // The canonical octagon of the fr-geometry tests: a 100x100 box with
        // four corners cut at leg 20, area 9200.
        let oct = OctBounds::new(0, 0, 100, 100, -80, 80, 20, 180);
        assert_eq!(oct.area2(), 2 * 9200);
        // Degenerate shapes have zero area, never a negative one.
        assert_eq!(OctBounds::EMPTY.area2(), 0);
        assert_eq!(square(5, 5, 5, 5).area2(), 0);
    }

    #[test]
    fn area_ignores_redundant_diagonal_borders() {
        // Diagonal bounds looser than the box must not shrink the area, and
        // wildly unbounded ones (as produced by simplex conversions) must not
        // overflow.
        let loose = OctBounds::new(0, 0, 10, 10, -1000, 1000, -1000, 1000);
        assert_eq!(loose.area2(), 2 * 100);
        let unbounded = OctBounds::new(0, 0, 10, 10, i32::MIN, i32::MAX, i32::MIN, i32::MAX);
        assert_eq!(unbounded.area2(), 2 * 100);
    }

    #[test]
    fn area_promotes_to_i128_outside_the_fast_path() {
        // Coordinates far beyond Limits::CRIT_INT: the i64 product would
        // overflow, the i128 path saturates instead of wrapping.
        let huge = OctBounds::new(
            i32::MIN,
            i32::MIN,
            i32::MAX,
            i32::MAX,
            i32::MIN,
            i32::MAX,
            i32::MIN,
            i32::MAX,
        );
        assert_eq!(huge.area2(), i64::MAX);
    }

    #[test]
    fn union_area_matches_the_materialized_union() {
        let a = OctBounds::new(0, 0, 100, 100, -80, 80, 20, 180);
        let b = square(50, 50, 250, 90);
        assert_eq!(a.union_area2(&b), a.union(&b).area2());
        assert_eq!(
            a.area_increase2(&b),
            a.union(&b).area2().saturating_sub(a.area2())
        );
        // Growing to contain a shape already inside costs nothing.
        assert_eq!(a.area_increase2(&square(10, 40, 20, 50)), 0);
    }

    #[test]
    fn intersects_is_symmetric_and_touching_counts() {
        let a = square(0, 0, 10, 10);
        let b = square(10, 10, 20, 20);
        let c = square(11, 11, 20, 20);
        assert!(a.intersects(&b) && b.intersects(&a));
        assert!(!a.intersects(&c) && !c.intersects(&a));
        assert!(!a.intersects(&OctBounds::EMPTY));
        // The diagonal borders participate: the octagon's clipped corner
        // separates it from a small box in that corner.
        let oct = OctBounds::new(0, 0, 100, 100, -80, 80, 20, 180);
        assert!(!oct.intersects(&square(0, 0, 5, 5)));
        assert!(oct.intersects(&square(0, 0, 40, 40)));
    }

    #[test]
    fn insert_query_remove_roundtrip() {
        let mut tree: ArenaShapeTree<&'static str> = ArenaShapeTree::new();
        assert!(tree.is_empty());
        assert_eq!(tree.epoch(), 0);

        let a = tree.insert(square(0, 0, 10, 10), "a");
        let b = tree.insert(square(20, 20, 30, 30), "b");
        let c = tree.insert(square(5, 5, 15, 15), "c");
        assert_eq!(tree.len(), 3);
        assert_eq!(tree.node_count(), 5);
        assert_eq!(tree.epoch(), 3);
        tree.check_invariants().unwrap();

        let mut out = Vec::new();
        tree.overlaps(&square(0, 0, 8, 8), &mut out);
        let mut hits: Vec<&str> = out.iter().map(|&(_, payload)| payload).collect();
        hits.sort_unstable();
        assert_eq!(hits, vec!["a", "c"]);

        tree.overlaps(&square(25, 25, 26, 26), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1, "b");

        assert_eq!(tree.remove(a), Some("a"));
        assert_eq!(tree.remove(a), None, "double remove must be rejected");
        tree.check_invariants().unwrap();
        tree.overlaps(&square(0, 0, 8, 8), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1, "c");

        assert_eq!(tree.remove(b), Some("b"));
        assert_eq!(tree.remove(c), Some("c"));
        assert!(tree.is_empty());
        assert_eq!(tree.root_bounds(), None);
        tree.check_invariants().unwrap();
    }

    #[test]
    fn set_payload_relabels_without_moving_the_leaf() {
        let mut tree: ArenaShapeTree<&'static str> = ArenaShapeTree::new();
        let a = tree.insert(square(0, 0, 10, 10), "a");
        let b = tree.insert(square(20, 20, 30, 30), "b");
        let shape = tree.node_count();
        let root = tree.root_bounds().unwrap();
        let epoch = tree.epoch();

        assert_eq!(tree.set_payload(a, "a2"), Some("a"));
        assert_eq!(tree.get(a), Some("a2"));
        assert_eq!(tree.get(b), Some("b"), "the sibling is untouched");
        assert_eq!(tree.epoch(), epoch + 1, "a mutation must bump the epoch");
        assert_eq!(tree.node_count(), shape, "no node is allocated or freed");
        assert_eq!(tree.root_bounds(), Some(root), "no bounds are disturbed");
        tree.check_invariants().unwrap();

        // The relabelled leaf answers queries at the same place, under its new
        // name — which is the whole point of a relabel over a re-insert.
        let mut out = Vec::new();
        tree.overlaps(&square(0, 0, 8, 8), &mut out);
        assert_eq!(out, vec![(a, "a2")]);

        // A stale handle is refused rather than corrupting a recycled slot.
        assert_eq!(tree.remove(a), Some("a2"));
        assert_eq!(tree.set_payload(a, "ghost"), None);
        assert_eq!(tree.get(b), Some("b"));
        tree.check_invariants().unwrap();
    }

    #[test]
    fn removal_tightens_ancestor_bounds() {
        let mut tree: ArenaShapeTree<u32> = ArenaShapeTree::new();
        tree.insert(square(0, 0, 10, 10), 0);
        tree.insert(square(20, 20, 30, 30), 1);
        let far = tree.insert(square(1000, 1000, 1010, 1010), 2);

        let grown = tree.root_bounds().unwrap();
        assert_eq!((grown.rx(), grown.uy()), (1010, 1010));

        tree.remove(far).unwrap();
        let tightened = tree.root_bounds().unwrap();
        assert_eq!((tightened.rx(), tightened.uy()), (30, 30));
        tree.check_invariants().unwrap();
    }

    #[test]
    fn clear_invalidates_every_handle_and_keeps_capacity() {
        let mut tree: ArenaShapeTree<u32> = ArenaShapeTree::new();
        let ids: Vec<_> = (0..8)
            .map(|i| tree.insert(square(i, i, i + 5, i + 5), i as u32))
            .collect();
        let slab = tree.slab_len();

        tree.clear();
        assert!(tree.is_empty());
        assert_eq!(tree.slab_len(), slab, "clear must keep the slab capacity");
        for id in &ids {
            assert!(!tree.contains_leaf(*id));
            assert_eq!(tree.get(*id), None);
            assert_eq!(tree.remove(*id), None);
        }
        tree.check_invariants().unwrap();

        // The slab is reusable afterwards.
        let fresh = tree.insert(square(0, 0, 1, 1), 42);
        assert_eq!(tree.get(fresh), Some(42));
        tree.check_invariants().unwrap();
    }

    #[test]
    fn query_stack_spills_past_the_inline_capacity() {
        let mut stack = QueryStack::new();
        let n = QueryStack::INLINE_CAPACITY + 17;
        for i in 0..n as u32 {
            stack.push(i);
        }
        for i in (0..n as u32).rev() {
            assert_eq!(stack.pop(), Some(i), "LIFO order across the spill boundary");
        }
        assert_eq!(stack.pop(), None);
    }

    #[test]
    fn deep_degenerate_tree_is_queried_correctly() {
        // Strictly nested boxes make the greedy heuristic build a maximally
        // unbalanced tree, deeper than QueryStack::INLINE_CAPACITY.
        let mut tree: ArenaShapeTree<u32> = ArenaShapeTree::new();
        let n = 300u32;
        for i in 0..n {
            let d = (n - i) as i32;
            tree.insert(square(-d, -d, d, d), i);
        }
        tree.check_invariants().unwrap();
        assert!(tree.depth_stats().max_depth > QueryStack::INLINE_CAPACITY);

        let mut out = Vec::new();
        tree.overlaps(&square(0, 0, 0, 0), &mut out);
        assert_eq!(out.len(), n as usize);
    }

    #[test]
    fn tree_is_send_and_sync_and_queried_in_parallel() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ArenaShapeTree<(u32, u16)>>();
        assert_send_sync::<OctBounds>();

        let mut tree: ArenaShapeTree<u32> = ArenaShapeTree::new();
        for i in 0..500u32 {
            tree.insert(square(i as i32, 0, i as i32 + 4, 4), i);
        }
        let tree = &tree;
        std::thread::scope(|scope| {
            for t in 0..4u32 {
                scope.spawn(move || {
                    let mut stack = QueryStack::new();
                    let mut out = Vec::new();
                    for i in 0..100u32 {
                        let x = (t * 100 + i) as i32;
                        tree.overlaps_with_stack(&square(x, 0, x, 4), &mut stack, &mut out);
                        assert!(!out.is_empty());
                        assert!(out.iter().all(|&(_, payload)| {
                            let p = payload as i32;
                            p <= x && x <= p + 4
                        }));
                    }
                });
            }
        });
    }
}
