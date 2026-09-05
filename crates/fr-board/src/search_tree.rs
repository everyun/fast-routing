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
//!   [`SearchTree::overlapping_tree_entries`] sorts by that key itself — and
//!   [`SearchTree::overlapping_tree_entries_unsorted`] does not, for the two
//!   upstream call sites that immediately impose an order of their own.
//! * **The leaf carries what the query rejects on.** The payload holds the
//!   shape's layer and the store slot its item is indexed under, so the layer
//!   filter is one integer compare against a leaf the walk has already loaded
//!   and the slot table is a direct index. See `LeafPayload` and
//!   `SearchTree::visit` (both private; `cargo doc --document-private-items`);
//!   the numbers are below.
//!
//! # The leaf-payload fast reject (M3 wave 5)
//!
//! A query used to pay a full resolution — `store.slot_of` (a **binary search**
//! over the ordered index), the slot table, the shape vector, the hull vector,
//! then `store.get` (a **second** binary search) — before it was allowed to ask
//! the two cheapest questions there are: *is this shape even on my layer*, and
//! *which slot is it in*. Both answers fit in one word each and were known at
//! insert time, so they now ride in the leaf.
//!
//! What that is worth, counted rather than timed. `Issue230-CNH_Functional_Tester_1`,
//! `-mp 2 --deterministic`, from an instrumented build of this file:
//!
//! | | |
//! |---|---|
//! | tree queries | 1 822 309 |
//! | leaf resolutions (leaves the walk reported) | 3 041 357 825 |
//! | of those, surviving the layer test | 683 365 117 (**22.5%**) |
//!
//! So **77.5%** of three billion leaf resolutions now end at one compare against
//! a cache line the walk already owns, and the 22.5% that survive reach their
//! slot entry and their item by two direct indices instead of two binary
//! searches.
//!
//! And on a clock — isolated builds from one `git archive`, arms interleaved,
//! serial, `nice -n 10`, best of the reps shown:
//!
//! | board | before | after | | reps |
//! |---|---|---|---|---|
//! | Issue230, `-mp 2 --deterministic` | 194.98 s | **132.52 s** | 1.47x | 3 |
//! | Issue034, `-mp 20 --deterministic` | 16.33 s | **14.43 s** | 1.13x | 2 |
//! | Issue026, `-mp 20 --deterministic` | 0.58 s | 0.56 s | 1.04x | 3 |
//!
//! Every run in that table wrote a **byte-identical** session file to its own
//! arm's counterpart, and every counter in the `FRMETRIC` report except
//! `tree_nodes_visited` came back identical — the change reorders work, it does
//! not change what is indexed or what is found. (`tree_nodes_visited` is the
//! exception on purpose: it was permanently zero and is now charged.) Seven
//! more boards were run once per arm at `-mp 3 --deterministic` purely to check
//! that: Issue110-RelayModule, Issue155, Issue187, Issue159, Issue143,
//! Issue163 and Issue269 all came back byte-identical too.
//!
//! Issue034 (`16.33 s`) and Issue026 (`0.58 s`) are the honest small print: the
//! win scales with how much of a run is spent in the tree, and on a two-layer
//! board with a couple of hundred items — where the layer test rejects at most
//! half and the store's ordered index fits in L1 — there is very little to
//! reject and very little to save. Issue230 is a four-layer board with power
//! planes; it is the case the change is for.
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
//! | 6 | `change_entries` / `merge_entries_*` reuse leaves in place | [`SearchTree::change_trace_entries`], which `combine` reaches through [`Board::change_trace_polyline`](crate::board::Board::change_trace_polyline) as well; the absorbed half of `merge_entries_*` and all of `reuse_entries_after_cutout` are still plain remove + insert |
//! | 2.2 | `overlapping_tree_entries_with_clearance` for uncompensated trees | the broad phase is this crate's plain overlap query; the symmetric half-clearance enlargement lives at the callers — `fr_check`'s sweep and `fr_router::pull_tight`'s acceptance oracle (the headless branch, `fanout034`) — plus `shove::geom::overlapping_items_with_clearance` |
//!
//! # Is the leaf reuse result-neutral? (yes, and it is checkable)
//!
//! The reuse recycles arena leaves; it must not change *what* is indexed. That
//! is a claim about a whole routing run, so it is settled by running one:
//! [`CHANGE_ENTRIES_ENV`] (`FR_CHANGE_ENTRIES=rebuild`) forces every tree onto
//! the remove + insert fallback, and the session file has to come back
//! byte-identical.
//!
//! Measured, M3 wave 3, `-mp 20`, reuse vs `rebuild`, same binary:
//!
//! | board | result |
//! |---|---|
//! | Issue026-J2_reference, Issue187-processor.Z80, Issue155-CH376_MCP795_Module | byte-identical |
//! | Issue163-pic_programmer, Issue153-wavefolder, Issue093-interf_u | byte-identical |
//! | Issue110-testPCBSpecctraFile | byte-identical |
//!
//! The knob demonstrably bites rather than silently doing nothing, and the
//! evidence for that is a **count, not a clock**. An instrumented build of
//! [`SearchTreeManager::change_trace_entries`] (M3 wave-3 gate) counts the
//! fallback taken 1 516 times on Issue026-J2_reference and 88 410 times on
//! Issue110-testPCBSpecctraFile under `rebuild`, against **0 times** in either
//! board's default run: the in-place path succeeds on every call it is given,
//! so `rebuild` is the only thing that ever exercises the fallback — and under
//! it, every call takes it.
//!
//! It is *not* measurable on a clock, which is worth writing down rather than
//! leaving to be rediscovered. Three runs each, serial, `nice -n 10`,
//! best-of-three `rebuild`/reuse ratio: Issue163-pic_programmer **0.96x**,
//! Issue026-J2_reference **1.01x**, Issue110-testPCBSpecctraFile **1.00x**
//! (47.42 s → 47.63 s) *with those 88 410 forced rebuilds*. On this suite the
//! leaf reuse is therefore worth approximately nothing in wall clock; it is kept
//! because it is what upstream does and because `combine_trace`'s relabel needs
//! the same machinery, not because it is a measured win. (An earlier draft of
//! this comment claimed 3.86 s → 6.33 s on Issue163; that does not reproduce,
//! and the numbers above replace it.)
//!
//! The knob isolates the *tree* half only — the journal's once-per-transaction
//! pre-image stays on in both arms, because it is a question about
//! `store.mutate`, not about leaves.
//!
//! So when a board's routed result *does* move across the in-place change —
//! Issue110-RelayModule did — the reuse is not the cause. What moved is the
//! item id: pull-tight used to remove the trace and re-insert it under a fresh,
//! maximal id, and this crate scans and sorts by **descending id**. Upstream's
//! `PolylineTrace.change` mutates the trace object, so the id survives; keeping
//! it is the parity fix, and the traversal order it restores is board-visible on
//! purpose. [`Board::change_trace_polyline`](crate::board::Board::change_trace_polyline)
//! has the long form.
//!
//! Issue110-RelayModule, `-mp 20 --deterministic`, isolated builds, worked
//! through:
//!
//! | build | stop | wall | result |
//! |---|---|---|---|
//! | wave 1 | global stagnation | 197s | 1u / 4v, score 979.53 |
//! | wave 2, id churn re-armed | global stagnation | 197s | **byte-identical to wave 1** |
//! | wave 2 | still working at the 600s cap | 600s | 1u / 4v, score 979.53 |
//!
//! Re-arming *only* pull-tight's remove + re-insert on top of the whole of
//! wave 2 — leaf reuse, journal, the `divide_large_room` fix, all still in —
//! reproduces wave 1's session file byte for byte. One variable, isolated: the
//! traversal order, and the traversal order is now upstream's.
//!
//! Two things that reading the gate row alone gets wrong. The gate reported
//! wave 2 losing a connection (`1u -> 2u`); it did not. Its budget expired at
//! 285s and it scored the board wave 2 was holding *mid-flight*. Given a budget
//! wave 1 never needed, wave 2 reaches the same 1u / 4v and leaves the same
//! connection open — `Net-(K1-Pad2)`, a two-pin net this port has never routed
//! on this board and the reference does. The regression that is real is
//! **convergence**: wave 1 reaches global stagnation and stops, wave 2 never
//! does, and each of its passes costs roughly three times as much. The board it
//! carries is more fragmented for the same routing — 165 traces and 47 vias in
//! the finished board against 154 and 45 — which is the shape of "the same nets,
//! cut into more pieces", and more pieces is more obstacles is a slower pass.
//! That is the open item — a cost regression on one board from a correct
//! ordering change — not a lost net and not a stale index.

use fr_datastructures::arena_shape_tree::{ArenaLeafId, ArenaShapeTree, OctBounds, QueryStack};
use fr_geometry::planar::{IntBox, IntOctagon, IntPoint, Point, Polyline};

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

/// The environment switch that turns the 45° tiling chain on.
///
/// See [`Tiling45Mode`] for the values. The flip changes the room tiling on
/// **every** default board, so it ships off and the gate decides.
pub const TILING_45_ENV: &str = "FR_ROUTER_TILING_45";

/// How much of the 45° chain to run — the A/B switch, three-valued because the
/// two halves of the chain measure very differently.
///
/// Upstream selects both halves with one tree subclass, but they are
/// independent:
///
/// * `Sorted45DegreeRoomNeighbours` — the octagon **door calculation**, which
///   relaxes all eight untouched offsets in one step instead of dropping one
///   simplex border line per restart, and builds octagon gap rooms;
/// * `ShapeSearchTree45Degree.complete_shape` — the octagon **room completion**.
///
/// Measured at `-mp 1` on the easy-10 gate set by the **M2b.2 gate**, i.e. on
/// the merged tree that also carries the link-door `Adjustment` fix. Unrouted
/// connections per mode (**zero** added clearance violations in every cell;
/// Java 2.3.0 at `-mp 1` in the last column for scale):
///
/// | board | `Off` | `Doors` | `Full` | java |
/// |---|---|---|---|---|
/// | Issue026 | 5/33 | **2/33** | 4/33 | 7/33 |
/// | Issue034 | 26/147 | **16/147** | 18/147 | 40/147 |
/// | Issue102 | 43/106 | 45/106 | **36/106** | 52/106 |
/// | Issue155 | 5/53 | **4/53** | 5/53 | 18/53 |
/// | Issue159 | **2/23** | 3/23 | 3/23 | 1/23 |
/// | Issue110 ×2 | **0/4** | 1/4 | 1/4 | 0/4 |
/// | Issue143 ×2, Issue187 | 0 | 0 | 0 | 0 |
///
/// and expansions, the cost axis this chain was meant to move:
///
/// | board | `Off` | `Doors` | `Full` | lab jar |
/// |---|---|---|---|---|
/// | Issue034 | 220,048 | **118,695** | 268,333 | 41,680 |
/// | Issue159 | **2,417** | 5,068 | 4,842 | 1,847 |
/// | Issue155 | 56,674 | 60,672 | **33,025** | 12,962 |
/// | Issue026 | 13,547 | **6,615** | 8,299 | 7,691 |
/// | Issue102 | **136,251** | 166,971 | 156,380 | 86,538 |
///
/// So the two halves genuinely disagree, and *which* one pays depends on the
/// board — no mode dominates on either axis. `Doors` is the strongest on
/// expansions and on the two big boards' unrouted count; `Full` wins Issue102
/// and Issue155's expansions outright, contradicting the pre-link-door-fix
/// reading that the octagon completion always tiles more finely. What decides
/// the shipped default is the gate score, not the totals: the two Issue110
/// boards and Issue159 **complete under `Off` and regress under both 45 modes**,
/// so `Off` scores 9/10 against 7/10 for either. Hence [`Off`](Self::Off)
/// stays the default and the switch stays three-valued for the boards where a
/// 45 mode is measurably better.
///
/// **Re-measured 2026-09-05 (wave A, `84bd710`), retiring the 7/10 verdict.**
/// After the maze-const collect work, the neckdown branch and the SAT/fork
/// gates, easy-10 under `Doors` is u same-or-**better** on all 9 routable
/// fixtures (026 3→2, both Issue110-testProjects 1→0, 143 1→0, 159 3→0), v=0
/// everywhere; the 24-board `-mp 20 --fanout` regression under `Doors` drops
/// Issue034's fanout +1v (zero added violations on all 24) and improves
/// unrouted on 102/054/153/157/269 — but Issue107 goes 0→1 (Java: 0) and
/// Issue163 7→10 (Java: 9), so the default still stays `Off`: the gate is
/// per-board non-regression. easyeda-pcb1's fanout stage runs **127×**
/// faster under `Doors` (4.07 s vs 515.9 s wall, at parity with the jar's
/// 4.01 s; 13u vs 31u, zero added violations) — the any-angle room machinery
/// costs ~90× per expansion on that board's tiny neckdown rooms. Details:
/// `~/fr-work/reports/REPORT-tiling45.md`.
///
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Tiling45Mode {
    /// Everything any-angle.
    Off,
    /// `Sorted45DegreeRoomNeighbours` only; room completion stays any-angle.
    /// The default for 45°-restricted boards (aligned with upstream).
    #[default]
    Doors,
    /// The whole chain: the octagon tree flavor, `complete_shape` and the doors.
    Full,
}

impl Tiling45Mode {
    /// True when any part of the chain runs.
    #[inline]
    pub fn is_on(self) -> bool {
        self != Tiling45Mode::Off
    }
}

/// What [`TILING_45_ENV`] asks for. Read once per process.
///
/// `full` / `1` / `true` / `on` / `yes` selects [`Tiling45Mode::Full`],
/// `doors` selects [`Tiling45Mode::Doors`], `off` / `0` / `false` selects
/// [`Tiling45Mode::Off`]. Defaults to [`Tiling45Mode::Doors`].
pub fn tiling_45_mode() -> Tiling45Mode {
    use std::sync::OnceLock;
    static MODE: OnceLock<Tiling45Mode> = OnceLock::new();
    *MODE.get_or_init(|| match std::env::var(TILING_45_ENV).ok().as_deref() {
        Some("full") | Some("1") | Some("true") | Some("on") | Some("yes") => Tiling45Mode::Full,
        Some("doors") => Tiling45Mode::Doors,
        Some("off") | Some("0") | Some("false") => Tiling45Mode::Off,
        _ => Tiling45Mode::Doors,
    })
}

/// Parity knob: set to `rebuild` to make
/// [`SearchTreeManager::change_trace_entries`] take its remove + insert
/// fallback for *every* trace instead of reusing leaves.
///
/// This exists to settle one question, and it is the question the in-place
/// change always raises: **is the leaf reuse result-neutral?** The two paths
/// differ only in which leaves are recycled — the shapes indexed, the items
/// they belong to and the sorted order every query reports are the same either
/// way — so a run under `rebuild` must produce a byte-identical session file.
/// If it does not, the reuse has a bookkeeping bug and the board can see it.
///
/// It deliberately does **not** restore the pre-`change_entries` behaviour of
/// removing the *item* and re-inserting it under a fresh [`ItemId`]. That is a
/// different variable, it is not upstream's (`PolylineTrace.change` mutates the
/// trace object in place, so the id survives), and conflating the two is what
/// made "the in-place change moved the result" look like one finding instead of
/// two. See [`Board::change_trace_polyline`](crate::board::Board::change_trace_polyline).
pub const CHANGE_ENTRIES_ENV: &str = "FR_CHANGE_ENTRIES";

/// True when [`CHANGE_ENTRIES_ENV`] asks for the rebuild path. Read once per
/// process.
fn change_entries_rebuild() -> bool {
    use std::sync::OnceLock;
    static REBUILD: OnceLock<bool> = OnceLock::new();
    *REBUILD.get_or_init(|| {
        matches!(
            std::env::var(CHANGE_ENTRIES_ENV).ok().as_deref(),
            Some("rebuild")
        )
    })
}

/// The **angle family** of a search tree — upstream's tree *subclass*.
///
/// `SearchTreeManager` grows this axis alongside the clearance-class axis:
/// `SearchTreeManager.java:166` picks the concrete class from
/// `rules.get_trace_angle_restriction()`, so a board can hold a
/// `ShapeSearchTree` and a `ShapeSearchTree45Degree` for the *same* compensated
/// clearance class and they store different geometry for the same items.
///
/// This is **not** [`AngleRestriction`], even though the two agree on real
/// boards. `AngleRestriction` is a *board rule* and decides the flavor of drill
/// and area shapes in **every** tree (`search-tree-obstacles.md` §3.4 — "the
/// conversion mode follows `board.rules`, not the tree subclass"). The family
/// is a property of the *tree* and decides whether the remaining shapes —
/// traces and the outline ring — are coarsened to octagons as well.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TreeAngleFamily {
    /// `ShapeSearchTree` — trace shapes stay exact simplices (§3.1).
    #[default]
    AnyAngle,
    /// `ShapeSearchTree45Degree` — every stored shape is an octagon (§3.3).
    ///
    /// Upstream leaves the *stored* trace shape a simplex and octagonizes it at
    /// every read (`ShapeSearchTree45Degree.java:147-149`,
    /// `Sorted45DegreeRoomNeighbours.java:113-114`, both
    /// `get_tree_shape(...).bounding_octagon()`); doing it once at insert is the
    /// same geometry for a 45°-restricted trace, where the octagon-clipped
    /// offset simplex *is* its own bounding octagon. The only divergence is a
    /// trace that is not 45°-restricted on a 45° board: its stored shape becomes
    /// the (larger) bounding octagon, so `overlapping_tree_entries` against this
    /// tree can report a hit upstream would have rejected. That is conservative
    /// — never a missed obstacle — and it is exactly what upstream's own room
    /// completion sees anyway.
    FortyFive,
    /// `ShapeSearchTree90Degree` — every stored shape is a box.
    Ninety,
}

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
    /// Store slot the tree walk already resolved. Lets a caller index
    /// [`SearchTree::item_shapes`] without [`ItemStore::slot_of`].
    pub slot: SlotIndex,
}

/// Leaf payload. `Copy` and 16 bytes, so the arena tree stores it inline.
///
/// # Why the layer and the slot travel with the leaf
///
/// A tree walk reports every leaf whose *bounds* meet the query; the query then
/// has to decide whether it wants that leaf. Two of those decisions used to cost
/// a full pointer chase before they could even be asked:
///
/// * the **layer** test — [`ItemStore::slot_of`] (a binary search over the
///   ordered index), the tree's slot table, the shape vector, and only then
///   `tile.layer != layer`;
/// * the **slot** the item is indexed under — [`ItemStore::slot_of`] again,
///   even though the tree already knew it at insert time.
///
/// Both are one word. Carrying them in the payload turns the layer test into a
/// single integer compare against a field of the leaf node the walk has just
/// read anyway (so it is free — same cache line), and turns the slot lookup
/// into a direct index. On `Issue230` the M3 wave-4 profile measured
/// 585 537 908 leaf resolutions over 498 897 queries with ~57% of them rejected
/// by the layer test *after* the whole chase; those 57% now cost one compare and
/// touch no memory outside the tree.
///
/// The **hull** [`IntBox`] deliberately stays in `SlotEntry::hulls` rather than
/// joining it, which is a departure from the wave-4 prescription and is
/// argued rather than measured. It is 16 further bytes on every node of the
/// arena — 60 bytes per node against 76, and the *descent* reads `bounds` out of
/// those same nodes, so widening them is paid by every query, hit or miss, and
/// by every inner node as well as every leaf. What it buys is narrow: the leaf
/// that survives the layer test has already forced `SlotEntry` into cache for
/// the `id` guard, so the hull it saves is one index into a vector on the line
/// next door, and only on the leaves a box query swallows whole. Two words for
/// a fast reject that fires on 57% of all leaves is a different trade from four
/// more for one warm load on a fraction of the rest.
///
/// `slot` is not an identity — slots are recycled — which is why every read
/// still checks `SlotEntry::id` against `item` before trusting the entry, the
/// same guard the `slot_of` version used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LeafPayload {
    item: ItemId,
    /// The store slot `item` occupied when the leaf was inserted, i.e. the index
    /// of its [`SlotEntry`] in [`SearchTree::slots`].
    slot: SlotIndex,
    /// `layer` of the [`ItemTileShape`] this leaf stands for — the fast reject.
    layer: i32,
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
    /// [`hull_box`] of each entry in `shapes`, parallel to it.
    ///
    /// A query's exact shape test is the single most expensive thing the router
    /// does, and most of the answers it buys are foregone conclusions: a stored
    /// shape wholly inside the query box meets it, no half-plane arithmetic
    /// needed. The test is one `IntBox` containment given the shape's hull — so
    /// the hull is computed once, at insert, rather than per query. It is kept
    /// here rather than on [`ItemTileShape`] so that the derived value cannot be
    /// desynchronized through that type's public fields.
    hulls: Vec<Option<IntBox>>,
    leaves: Vec<ArenaLeafId>,
}

impl SlotEntry {
    fn clear(&mut self) {
        self.id = ItemId::UNASSIGNED;
        self.shapes.clear();
        self.hulls.clear();
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
    angle_family: TreeAngleFamily,
    tree: ArenaShapeTree<LeafPayload>,
    slots: Vec<SlotEntry>,
}

impl SearchTree {
    /// An empty any-angle tree compensated for `compensated_class`.
    pub fn new(compensated_class: i32) -> Self {
        SearchTree::with_family(compensated_class, TreeAngleFamily::AnyAngle)
    }

    /// An empty tree compensated for `compensated_class` in `angle_family` —
    /// the two-axis constructor `SearchTreeManager.java:160-172` dispatches on.
    pub fn with_family(compensated_class: i32, angle_family: TreeAngleFamily) -> Self {
        SearchTree {
            compensated_class,
            angle_family,
            tree: ArenaShapeTree::new(),
            slots: Vec::new(),
        }
    }

    /// The clearance class this tree's shapes are inflated for; `0` = raw.
    #[inline]
    pub fn compensated_class(&self) -> i32 {
        self.compensated_class
    }

    /// Which tree subclass this tree stands for.
    #[inline]
    pub fn angle_family(&self) -> TreeAngleFamily {
        self.angle_family
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
        self.item_shapes(slot, id)
            .get(index as usize)
            .map(|s| s.layer)
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
        calculate_tree_shapes_for_family(
            item,
            self.compensated_class,
            self.angle_family,
            ctx,
            &mut entry.shapes,
        );

        entry.hulls.clear();
        entry
            .hulls
            .extend(entry.shapes.iter().map(|tile| match tile.shape.as_ref() {
                Some(shape) => hull_box(shape),
                None => None,
            }));

        for (index, tile) in entry.shapes.iter().enumerate() {
            let Some(shape) = tile.shape.as_ref() else {
                continue;
            };
            let leaf = self.tree.insert(
                shape.bounds(),
                LeafPayload {
                    item: item.id,
                    slot,
                    layer: tile.layer,
                    shape_index: index as u16,
                },
            );
            entry.leaves.push(leaf);
        }
        self.slots[slot_index] = entry;
    }

    /// Re-indexes a trace whose polyline changed, **reusing** the leaves and
    /// the tile shapes of the segments that did not move — upstream's
    /// `ShapeSearchTree.change_entries` (`:110-153`).
    ///
    /// `item` must already carry the *new* polyline; the slot still carries the
    /// shapes computed for the old one, and those are what the kept ranges are
    /// taken from. `keep_at_start` / `keep_at_end` are the **upper bounds**
    /// [`Board::change_trace_polyline`](crate::board::Board::change_trace_polyline)
    /// derives from the two line arrays: `k` equal leading lines bound the
    /// reusable leading shapes at `k - 2`, and likewise at the end. They are
    /// narrowed here — see `dogear_safe_keep_at_start`, which is where the
    /// fact that a tile shape reads more than its own three lines is paid for.
    ///
    /// Returns `false` — having touched nothing — when the fast path does not
    /// apply (the slot holds another item, the item is not a trace, or the slot
    /// state is not the one-leaf-per-shape shape a trace always has). The caller
    /// then falls back to [`remove`](Self::remove) + [`insert`](Self::insert),
    /// which produces the same index.
    ///
    /// # The trailing leaves are relabelled, not re-inserted
    ///
    /// A leaf's payload here is the pair `(item, shape index)` stored *inside*
    /// the arena tree, where upstream's `Leaf` is a heap object it mutates in
    /// place (`leaf.shape_index_in_object = new_index`, `:140`). When the
    /// segment count changes, the trailing leaves keep their shape — and
    /// therefore their bounds, and therefore their place in the tree — and only
    /// their index shifts, so they are relabelled through
    /// [`ArenaShapeTree::set_payload`] and never leave the tree. Neither their
    /// geometry nor their position is recomputed; when the count is unchanged
    /// (the whole `reposition_line` family) the suffix is not touched at all.
    pub fn change_trace_entries(
        &mut self,
        slot: SlotIndex,
        item: &Item,
        keep_at_start: usize,
        keep_at_end: usize,
        ctx: ShapeContext<'_>,
    ) -> bool {
        let ItemKind::Trace(trace) = &item.kind else {
            return false;
        };
        let slot_index = slot.index();
        match self.slots.get(slot_index) {
            // One leaf per shape is what a trace always has (no `None` tile),
            // and it is what makes `leaves[i]` the leaf of shape `i`.
            Some(entry)
                if entry.id == item.id
                    && entry.leaves.len() == entry.shapes.len()
                    && entry.hulls.len() == entry.shapes.len() => {}
            _ => return false,
        }

        let line_count = trace.polyline.arr.len();
        let new_shape_count = line_count.saturating_sub(2);
        let mut entry = std::mem::take(&mut self.slots[slot_index]);
        let old_shape_count = entry.shapes.len();

        // The offset width this tree stores the trace at — `p_obj.get_half_width()
        // + clearance_compensation_value(...)` (`change_entries`, `:117-119`).
        // It is also the radius the dog-ear scan below is bounded by, which is
        // why the kept ranges have to be narrowed per tree rather than once.
        let offset_width = trace.half_width
            + ctx.clearances.tree_compensation(
                item.clearance_class,
                self.compensated_class,
                trace.layer,
            );

        // Clamp before use: the two kept ranges must not overlap in either the
        // old or the new shape array, and neither may reach past the dog-ear
        // horizon.
        let keep_start = dogear_safe_keep_at_start(
            &trace.polyline,
            offset_width,
            keep_at_start.min(old_shape_count).min(new_shape_count),
        );
        let keep_end = dogear_safe_keep_at_end(
            &trace.polyline,
            offset_width,
            keep_at_end
                .min(old_shape_count - keep_start)
                .min(new_shape_count - keep_start),
            new_shape_count,
        );
        let old_mid_end = old_shape_count - keep_end;
        let new_mid_end = new_shape_count - keep_end;

        // 1. The changed middle's old leaves go, exactly as `change_entries`
        //    drops `old_entries[keep_at_start .. old_shape_count - keep_at_end]`.
        for leaf in entry.leaves[keep_start..old_mid_end].iter().copied() {
            self.tree.remove(leaf);
        }
        // The trailing leaves always survive: their shapes did not move, so
        // neither did their place in the tree. When the segment count changed
        // their *index* moved, and that is a relabel, not a re-insert — see
        // step 4.
        let suffix_shifts = new_shape_count != old_shape_count;

        // 2. Only the changed segments' geometry is recomputed —
        //    `offset_shapes(p_new_polyline, chw, keep_at_start,
        //    arr.length - 1 - keep_at_end)`.
        let middle = trace.polyline.offset_shapes_range(
            offset_width,
            keep_start,
            line_count.saturating_sub(1) - keep_end,
        );
        debug_assert_eq!(
            middle.len(),
            new_mid_end - keep_start,
            "offset_shapes_range disagrees with the kept-range arithmetic"
        );
        // 3. Shape, hull and leaf of each changed segment, in one pass — the
        //    leaf carries its **final** index, so nothing has to be relabelled.
        let mut mid_shapes: Vec<ItemTileShape> = Vec::with_capacity(middle.len());
        let mut mid_hulls: Vec<Option<IntBox>> = Vec::with_capacity(middle.len());
        let mut mid_leaves: Vec<ArenaLeafId> = Vec::with_capacity(middle.len());
        for (offset, simplex) in middle.into_iter().enumerate() {
            let mut shape = TileShape::Simplex(simplex);
            if self.angle_family == TreeAngleFamily::FortyFive {
                shape = shape.to_flavor(AngleRestriction::FortyFive);
            }
            mid_hulls.push(hull_box(&shape));
            mid_leaves.push(self.tree.insert(
                shape.bounds(),
                LeafPayload {
                    item: item.id,
                    slot,
                    layer: trace.layer,
                    shape_index: (keep_start + offset) as u16,
                },
            ));
            mid_shapes.push(ItemTileShape {
                shape: Some(shape),
                layer: trace.layer,
            });
        }
        entry.shapes.splice(keep_start..old_mid_end, mid_shapes);
        entry.hulls.splice(keep_start..old_mid_end, mid_hulls);
        entry.leaves.splice(keep_start..old_mid_end, mid_leaves);

        // 4. The trailing shapes are kept verbatim; only their *index* moved,
        //    so the leaves are relabelled where they sit —
        //    `new_leaf_arr[new_index].shape_index_in_object = new_index`
        //    (`ShapeSearchTree.java:140`). The bounds are unchanged by
        //    construction (same shape), which is exactly the precondition
        //    [`ArenaShapeTree::set_payload`] carries.
        if suffix_shifts {
            for index in new_mid_end..new_shape_count {
                let relabelled = self.tree.set_payload(
                    entry.leaves[index],
                    LeafPayload {
                        item: item.id,
                        slot,
                        // A trace has one layer, and a kept trailing shape is
                        // the same shape it always was — only its index moved.
                        layer: trace.layer,
                        shape_index: index as u16,
                    },
                );
                debug_assert!(
                    relabelled.is_some(),
                    "a kept trailing leaf must still be live"
                );
            }
        }
        debug_assert_eq!(entry.shapes.len(), new_shape_count);
        debug_assert_eq!(entry.hulls.len(), new_shape_count);
        debug_assert_eq!(entry.leaves.len(), new_shape_count);

        self.slots[slot_index] = entry;
        true
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
        self.collect_tree_entries::<true>(store, query, layer, ignore_nets, stack, out);
        sort_entries(out);
    }

    /// [`overlapping_tree_entries`](SearchTree::overlapping_tree_entries) in
    /// **tree traversal order** — the same entries, not sorted.
    ///
    /// # When the sort is upstream's and when it is not
    ///
    /// `ShapeSearchTree.overlapping_tree_entries` (`:369-417`) collects into a
    /// `TreeSet<Leaf>`, so its result *is* sorted and a caller that consumes it
    /// in order needs [`overlapping_tree_entries`](Self::overlapping_tree_entries).
    /// That covers every caller whose first decision depends on which entry
    /// comes first.
    ///
    /// It does not cover a caller that re-sorts. `complete_shape` and
    /// `SortedRoomNeighbours.calculate_neighbours` each walk the tree and then
    /// sort the result into *their own* order — the first by `Leaf.compareTo`,
    /// the second by a single ascending `get_id_no()` key that interleaves
    /// items with expansion rooms (`fr_router::engine`'s
    /// `sort_obstacles_for_completion` / `sort_obstacles_for_neighbours`). For
    /// those, sorting here is a comparison sort over a list nothing reads: on
    /// `Issue230` the entry list averages 508 elements over 205 k completion
    /// queries and 1 174 over the neighbour queries, all of it thrown away one
    /// call later.
    ///
    /// The rule, then, is the mirror of the one on
    /// [`overlapping_tree_entries_bounds_only`](Self::overlapping_tree_entries_bounds_only):
    /// use this only where the caller imposes its own total order on the result
    /// before anything reads it. Traversal order depends on the insertion
    /// history, so a caller that leaks it into a routing decision makes the
    /// board depend on the tree's shape — which is exactly what
    /// [`sort_entries`] exists to prevent.
    pub fn overlapping_tree_entries_unsorted(
        &self,
        store: &ItemStore,
        query: &TileShape,
        layer: Option<i32>,
        ignore_nets: &[i32],
        stack: &mut QueryStack,
        out: &mut Vec<TreeEntry>,
    ) {
        self.collect_tree_entries::<true>(store, query, layer, ignore_nets, stack, out);
    }

    /// [`overlapping_tree_entries`](SearchTree::overlapping_tree_entries) with
    /// the **exact shape test dropped**: the leaves whose bounding shape meets
    /// `query`'s, filtered by layer and net only, in the same order.
    ///
    /// # When this is the faithful port and the exact one is not
    ///
    /// `ShapeSearchTree.complete_shape` (`ShapeSearchTree.java:578-595`) does
    /// not call `overlapping_tree_entries` at all. It walks the tree itself:
    ///
    /// ```text
    /// if (curr_node.bounding_shape.intersects(bounding_shape)) { … }
    /// ```
    ///
    /// — a *regular* bounding-shape test and nothing else, then
    /// `Collections.sort(overlapping_leaves)` (`Leaf.compareTo`, which is
    /// [`sort_entries`]'s key). The exact work happens in the caller, where the
    /// `intersection.dimension() == 2` gate (`:617-620`) decides whether the
    /// obstacle carves the room; a leaf that only meets the bound is a no-op
    /// there, so the exact test in the query is pure duplicated cost. It was
    /// measured at 34% of an Issue034 run.
    ///
    /// Every other upstream query — `overlapping_objects`,
    /// `overlapping_tree_entries`, `overlapping_items_with_clearance` — *does*
    /// filter exactly, and its port must keep
    /// [`overlapping_tree_entries`](SearchTree::overlapping_tree_entries).
    /// The rule: use this one only where the caller re-does the exact geometry
    /// itself and would discard the extra entries anyway.
    ///
    /// The result is a **superset** of the exact query's: a bounding-shape
    /// overlap is implied by any shape overlap, never the other way round.
    pub fn overlapping_tree_entries_bounds_only(
        &self,
        store: &ItemStore,
        query: &TileShape,
        layer: Option<i32>,
        ignore_nets: &[i32],
        stack: &mut QueryStack,
        out: &mut Vec<TreeEntry>,
    ) {
        self.collect_tree_entries::<false>(store, query, layer, ignore_nets, stack, out);
        sort_entries(out);
    }

    /// The body every entry point shares; `EXACT` is a const parameter so the
    /// shape test is compiled away rather than branched on per leaf.
    ///
    /// Leaves `out` in traversal order — the sort, where there is one, belongs
    /// to the caller.
    fn collect_tree_entries<const EXACT: bool>(
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
        let contains = QueryContains::of(query);
        self.visit(
            store,
            &bounds,
            layer,
            ignore_nets,
            stack,
            |payload, tile, hull| {
                let Some(shape) = tile.shape.as_ref() else {
                    return;
                };
                if EXACT && !contains.swallows(hull) && !shape.intersects(query) {
                    return;
                }
                out.push(TreeEntry {
                    item: payload.item,
                    shape_index: payload.shape_index,
                    slot: payload.slot,
                });
            },
        );
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
        let contains = QueryContains::of(query);
        self.visit(
            store,
            &bounds,
            layer,
            ignore_nets,
            stack,
            |payload, tile, hull| {
                let Some(shape) = tile.shape.as_ref() else {
                    return;
                };
                if contains.swallows(hull) || shape.intersects(query) {
                    out.push(payload.item);
                }
            },
        );
        // Descending id, then deduplicate — upstream's `TreeSet<SearchTreeObject>`.
        // Unstable: the elements are plain ids, so equal ones are
        // indistinguishable and `dedup` collapses them anyway. It also keeps the
        // query allocation-free — the stable sort borrows a scratch buffer from
        // the allocator once the run is longer than its in-place threshold.
        out.sort_unstable_by(|a, b| b.cmp(a));
        out.dedup();
    }

    /// The shared broad-phase walk: reject every hit leaf the cheapest way
    /// available, and hand the survivors' tile shape and hull to `visit`.
    ///
    /// The DFS stack is caller-owned (so the tree itself stays `&self` and the
    /// query allocates nothing once warm); the closure holds a second immutable
    /// borrow of `self` for the slot table, which is why the two are separate
    /// parameters.
    ///
    /// # The order of the tests is the optimization
    ///
    /// The layer and net filters are applied *here*, before the tile shape is
    /// resolved, because each one can be answered more cheaply than the
    /// resolution it used to follow (see [`LeafPayload`]):
    ///
    /// 1. **layer** — one integer compare against a field of the leaf node the
    ///    walk has already loaded. Nothing else is touched. This is where ~57%
    ///    of `Issue230`'s 585 M leaf resolutions stop.
    /// 2. **slot entry** — a direct index into [`SearchTree::slots`], guarded by
    ///    `entry.id == payload.item`, where it used to be
    ///    [`ItemStore::slot_of`]'s binary search over the ordered index.
    /// 3. **the item** — [`ItemStore::item_at_slot`], another direct index,
    ///    where it used to be a second binary search through
    ///    [`ItemStore::get`]. The id re-check is what makes it equivalent: a
    ///    slot the store has since recycled holds a different id, and a freed
    ///    one holds nothing, so "the leaf's item is still on the board, in this
    ///    slot" is decided by one comparison.
    /// 4. **net** — [`is_ignored`], which needs the item and nothing else.
    ///
    /// Only then are `shapes` and `hulls` indexed. That is upstream's own order
    /// of business, not a reordering:
    /// `ShapeSearchTree.overlapping_tree_entries` (`:383-395`) tests
    /// `curr_object.shape_layer(shape_index) != p_layer` first, the ignore-net
    /// loop second, and `curr_shape.intersects(p_shape)` only for what survives
    /// both. What moved is where the layer comes *from*, not when it is asked.
    ///
    /// # `tree_nodes_visited`
    ///
    /// Every query in this crate funnels through here, so this is where
    /// [`fr_metrics::counters::Counter::TreeNodesVisited`] is charged — once per
    /// query, with the total [`ArenaShapeTree::visit_overlaps`] accumulated in a
    /// register, never once per node. Half a billion thread-local
    /// read-modify-writes would be a cost of measuring rather than a
    /// measurement, which is why the counter sat at zero until now: there was no
    /// affordable place to bump it from.
    ///
    /// The dependency runs `fr-board → fr-metrics`, which is acyclic —
    /// `fr-metrics` has no dependencies at all, by design — and it is the same
    /// direction `fr-router` already takes.
    fn visit(
        &self,
        store: &ItemStore,
        bounds: &OctBounds,
        layer: Option<i32>,
        ignore_nets: &[i32],
        stack: &mut QueryStack,
        mut visit: impl FnMut(LeafPayload, &ItemTileShape, Option<&IntBox>),
    ) {
        let nodes = self.tree.visit_overlaps(bounds, stack, |_leaf, payload| {
            if !layer_matches(layer, payload.layer) {
                return;
            }
            let Some(entry) = self.slots.get(payload.slot.index()) else {
                return;
            };
            if entry.id != payload.item {
                return;
            }
            // The store still has the last word on whether the item is on the
            // board — but by slot, not by id.
            let Some(item) = store.item_at_slot(payload.slot) else {
                return;
            };
            if item.id != payload.item || is_ignored(item, ignore_nets) {
                return;
            }
            let Some(tile) = entry.shapes.get(payload.shape_index as usize) else {
                return;
            };
            debug_assert_eq!(
                tile.layer, payload.layer,
                "leaf payload layer is stale for item {} shape {}",
                payload.item, payload.shape_index
            );
            let hull = entry
                .hulls
                .get(payload.shape_index as usize)
                .and_then(Option::as_ref);
            visit(payload, tile, hull);
        });
        fr_metrics::counters::bump::tree_nodes_visited(nodes);
    }

    /// Panics unless every leaf, slot entry and store item agree.
    ///
    /// O(board); for tests and `debug_assert`-style use from the board layer.
    pub fn assert_consistent(&self, store: &ItemStore) {
        let mut leaves = 0usize;
        for (slot_index, entry) in self.slots.iter().enumerate() {
            if entry.id == ItemId::UNASSIGNED {
                assert!(
                    entry.leaves.is_empty() && entry.shapes.is_empty() && entry.hulls.is_empty(),
                    "empty slot {slot_index} still carries tree state"
                );
                continue;
            }
            // The hull cache is derived state, so the only thing that can go
            // wrong with it is going stale — check it against the shape it was
            // computed from, not just its length.
            assert_eq!(
                entry.hulls.len(),
                entry.shapes.len(),
                "item {} has {} shapes but {} cached hulls",
                entry.id,
                entry.shapes.len(),
                entry.hulls.len()
            );
            for (index, tile) in entry.shapes.iter().enumerate() {
                let expected = tile.shape.as_ref().and_then(hull_box);
                assert_eq!(
                    entry.hulls[index], expected,
                    "item {} shape {index} has a stale cached hull",
                    entry.id
                );
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
                // The slot and the layer in the payload are what every query
                // decides on before it touches anything else, so a stale one is
                // a silently wrong answer rather than a crash. They are derived
                // state exactly like `hulls`, and checked the same way.
                assert_eq!(
                    payload.slot.index(),
                    slot_index,
                    "item {} has a leaf pointing at the wrong slot",
                    entry.id
                );
                let tile = entry
                    .shapes
                    .get(payload.shape_index as usize)
                    .unwrap_or_else(|| {
                        panic!(
                            "item {} has a leaf for shape {} of {}",
                            entry.id,
                            payload.shape_index,
                            entry.shapes.len()
                        )
                    });
                assert_eq!(
                    payload.layer, tile.layer,
                    "item {} shape {} has a stale layer in its leaf payload",
                    entry.id, payload.shape_index
                );
                assert!(
                    tile.shape.is_some(),
                    "item {} has a leaf for the unoccupied shape index {}",
                    entry.id,
                    payload.shape_index
                );
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

    /// The any-angle compensated tree for `clearance_class`, if one exists.
    pub fn compensated_tree(&self, clearance_class: i32) -> Option<&SearchTree> {
        self.compensated_tree_with_family(clearance_class, TreeAngleFamily::AnyAngle)
    }

    /// The compensated tree at `(clearance_class, family)`, if one exists.
    ///
    /// The registry is two-dimensional: upstream keys its autoroute trees by
    /// clearance class and builds each one through the subclass constructor its
    /// angle restriction selects (`SearchTreeManager.java:160-172`), so the same
    /// class can be present in more than one family at once.
    pub fn compensated_tree_with_family(
        &self,
        clearance_class: i32,
        family: TreeAngleFamily,
    ) -> Option<&SearchTree> {
        if self.default_tree.compensated_class == clearance_class
            && self.default_tree.angle_family == family
        {
            return Some(&self.default_tree);
        }
        self.compensated
            .iter()
            .find(|tree| tree.compensated_class == clearance_class && tree.angle_family == family)
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

    /// Fans [`SearchTree::change_trace_entries`] out to every active tree,
    /// upstream's `SearchTreeManager.change_entries` (`:278-284`).
    ///
    /// The kept-range counts are geometry-independent — a tile shape at index
    /// `s` is a function of lines `s..=s+2` in *any* tree, only its offset
    /// width differs — so the same pair is correct for all of them. A tree that
    /// refuses the fast path (it has never seen the item, say) falls back to
    /// remove + insert, which lands on the same index — as does every tree when
    /// [`CHANGE_ENTRIES_ENV`] asks for it, which is how "same index" is checked
    /// on a real board rather than asserted.
    pub fn change_trace_entries(
        &mut self,
        slot: SlotIndex,
        item: &Item,
        keep_at_start: usize,
        keep_at_end: usize,
        ctx: ShapeContext<'_>,
    ) {
        let rebuild = change_entries_rebuild();
        for tree in self.trees_mut() {
            if rebuild || !tree.change_trace_entries(slot, item, keep_at_start, keep_at_end, ctx) {
                tree.remove(slot, item.id);
                tree.insert(slot, item, ctx);
            }
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
        self.ensure_compensated_tree_with_family(
            clearance_class,
            TreeAngleFamily::AnyAngle,
            store,
            ctx,
        );
    }

    /// [`ensure_compensated_tree`](Self::ensure_compensated_tree) in a chosen
    /// angle family — upstream's `getAutorouteTree` reaching the
    /// `ShapeSearchTree45Degree` arm of `SearchTreeManager.java:166`.
    pub fn ensure_compensated_tree_with_family(
        &mut self,
        clearance_class: i32,
        family: TreeAngleFamily,
        store: &ItemStore,
        ctx: ShapeContext<'_>,
    ) {
        if clearance_class <= 0
            || self
                .compensated_tree_with_family(clearance_class, family)
                .is_some()
        {
            return;
        }
        let mut tree = SearchTree::with_family(clearance_class, family);
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

// ------------------------------------------------- the dog-ear horizon (§6)

/// How far a tile shape's dog-ear scan may look, as a squared distance.
///
/// `Polyline::offset_shapes_range` builds a segment's four border lines from
/// the three lines around it and then walks *outwards* along the polyline,
/// cutting the corner's "dog ear" against each further border line, for as long
/// as consecutive corners stay inside this radius of the segment's own corner
/// (`polyline.rs`, `check_dist_square = 2 * half_width²`). A tile shape is
/// therefore **not** a function of its own three lines alone, which is the one
/// place upstream's `change_entries` contract is too optimistic: it derives the
/// kept ranges from line equality only (`PolylineTrace.java:983-987`).
///
/// On a board whose corners are further apart than the trace is wide the two
/// rules coincide, which is why the discrepancy is invisible almost everywhere.
/// Where it is not — a chain of chamfers shorter than the trace half width,
/// exactly what `smoothen_corners` produces — a kept leaf would hold a shape
/// with a stale dog-ear cut, i.e. a *smaller* obstacle than the trace really
/// is. That is a missed clearance violation, so the horizon is respected here
/// rather than inherited.
#[inline]
fn dogear_horizon_square(offset_width: i32) -> f64 {
    let w = f64::from(offset_width);
    2.0 * w * w
}

/// How many corners the narrowing scans will look at before giving up and
/// keeping nothing.
///
/// Both scans stop at the first corner outside the horizon, so on any board
/// whose corners are further apart than its traces are wide they cost one
/// distance each. The cap bounds the other case — a polyline that folds a long
/// run of corners into one trace-width disc — to `O(1)` instead of `O(run²)`.
/// Giving up means reusing fewer leaves, never more, so the cap can only cost
/// time, not correctness.
const DOGEAR_SCAN_LIMIT: usize = 64;

/// Narrows `keep_at_start` to the shapes whose dog-ear scan provably stays
/// inside the unchanged leading run.
///
/// `keep_at_start` came from `k` equal leading lines as `k - 2`, so lines
/// `0..k` and hence corners `0..=k - 2` are shared by the old and the new
/// polyline. Shape `s` anchors its forward scan at corner `s + 1` and walks
/// corners `s + 2, s + 3, …`, stopping at the first one outside the horizon. If
/// it stops at a *shared* corner it read shared lines only and the stored shape
/// is still exact; if it runs to the end of the shared run it may have read a
/// line that moved.
///
/// So the largest kept count is the largest `K` for which some corner in
/// `K + 1 ..= k - 2` is outside corner `K`'s horizon. Only shared corners are
/// consulted, which is what makes the answer the same for the old polyline (the
/// one the kept shapes were computed from) and the new one.
///
/// The last shape of the run is always dropped: its scan starts at the first
/// line that changed, so nothing shared can prove it stopped in time. One
/// segment per end is a rounding error against the whole polyline.
fn dogear_safe_keep_at_start(
    polyline: &Polyline,
    offset_width: i32,
    keep_at_start: usize,
) -> usize {
    if keep_at_start == 0 {
        return 0;
    }
    let horizon = dogear_horizon_square(offset_width);
    // `keep_at_start = k - 2`, so the last shared corner is `k - 2 =
    // keep_at_start`.
    let last_shared_corner = keep_at_start;
    let floor = keep_at_start.saturating_sub(DOGEAR_SCAN_LIMIT);
    let mut keep = keep_at_start;
    while keep > floor {
        let anchor = polyline.corner_approx(keep);
        if ((keep + 1)..=last_shared_corner)
            .take(DOGEAR_SCAN_LIMIT)
            .any(|corner| polyline.corner_approx(corner).distance_square(&anchor) > horizon)
        {
            return keep;
        }
        keep -= 1;
    }
    // Ran out of budget inside a dense run: keep nothing rather than guess.
    if keep == floor && floor > 0 {
        return 0;
    }
    keep
}

/// [`dogear_safe_keep_at_start`] for the trailing run, whose scan walks
/// *backwards*.
///
/// `keep_at_end = m - 2` for `m` equal trailing lines, so the first shared
/// trailing line is `new_shape_count + 2 - m = new_shape_count - keep_at_end`
/// and every corner from there on is shared. Shape `s` anchors its backward
/// scan at corner `s` and walks corners `s - 1, s - 2, …`.
fn dogear_safe_keep_at_end(
    polyline: &Polyline,
    offset_width: i32,
    keep_at_end: usize,
    new_shape_count: usize,
) -> usize {
    if keep_at_end == 0 {
        return 0;
    }
    let horizon = dogear_horizon_square(offset_width);
    let first_shared_corner = new_shape_count - keep_at_end;
    let floor = keep_at_end.saturating_sub(DOGEAR_SCAN_LIMIT);
    let mut keep = keep_at_end;
    while keep > floor {
        let anchor_corner = new_shape_count - keep;
        let anchor = polyline.corner_approx(anchor_corner);
        if (first_shared_corner..anchor_corner)
            .rev()
            .take(DOGEAR_SCAN_LIMIT)
            .any(|corner| polyline.corner_approx(corner).distance_square(&anchor) > horizon)
        {
            return keep;
        }
        keep -= 1;
    }
    if keep == floor && floor > 0 {
        return 0;
    }
    keep
}

// ---------------------------------------------------------------- geometry

/// The smallest [`IntBox`] that provably **contains** `shape`, or `None` when
/// there is none.
///
/// `None` has two causes and both must stay `None`: an empty shape, whose
/// "bounding box" is the inverted [`IntBox::EMPTY`] and would swallow any
/// containment test; and an unbounded simplex, whose corner approximations are
/// the `i32::MAX` sentinel of parallel border lines — or, for a two-line wedge,
/// the single point where the lines cross while the shape itself runs to
/// infinity. Only for a bounded shape is the corner hull a hull.
fn hull_box(shape: &TileShape) -> Option<IntBox> {
    let hull = match shape {
        TileShape::Box(b) => *b,
        TileShape::Oct(o) => o.bounding_box(),
        TileShape::Simplex(s) if s.is_bounded() => s.bounding_box(),
        TileShape::Simplex(_) => return None,
    };
    (!hull.is_empty()).then_some(hull)
}

/// The "does this query swallow a stored shape whole" test, resolved once per
/// query instead of per leaf.
///
/// A stored shape whose hull lies inside the query **is** an overlap, so the
/// exact half-plane test can be skipped for it. That is not an approximation:
/// `shape ⊆ hull ⊆ query` gives `shape ∩ query = shape`, and a hull only exists
/// for a non-empty shape ([`hull_box`]).
///
/// Only a box query gets the shortcut. An octagon or a simplex query would need
/// its own containment predicate for a gain the profile does not show: the
/// query that dominates a routing run is `complete_shape`'s running bound
/// (`rooms/complete.rs`), and that one is an `IntBox`.
#[derive(Clone, Copy)]
enum QueryContains {
    /// The query is an `IntBox`; a hull inside it is a hit.
    Box(IntBox),
    /// Anything else — every leaf takes the exact test.
    Never,
}

impl QueryContains {
    fn of(query: &TileShape) -> QueryContains {
        match query {
            TileShape::Box(b) if !b.is_empty() => QueryContains::Box(*b),
            _ => QueryContains::Never,
        }
    }

    #[inline]
    fn swallows(&self, hull: Option<&IntBox>) -> bool {
        match (self, hull) {
            (QueryContains::Box(query), Some(hull)) => query.contains_box(hull),
            _ => false,
        }
    }
}

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

/// Upstream's `TreeSet<Leaf>` order: descending item id, then ascending shape
/// index (`Leaf.compareTo` → `Item.compareTo`, which is `other.id - this.id`).
///
/// Unstable, because the key is the leaf's whole identity: a tree never holds
/// two leaves with the same `(item, shape_index)`, so the comparator has no ties
/// and stability cannot change the output. It saves the scratch allocation the
/// stable sort takes for runs longer than its in-place threshold — this is the
/// tail of every spatial query, and there are ~200 000 of them on Issue034.
fn sort_entries(out: &mut [TreeEntry]) {
    out.sort_unstable_by(|a, b| {
        b.item
            .cmp(&a.item)
            .then_with(|| a.shape_index.cmp(&b.shape_index))
    });
}

/// [`calculate_tree_shapes`] in a chosen angle family.
///
/// [`TreeAngleFamily::AnyAngle`] is `ShapeSearchTree` itself.
/// [`TreeAngleFamily::FortyFive`] is `ShapeSearchTree45Degree`, whose three
/// overrides all have the same shape — *"call super, then coarsen every result
/// to its bounding octagon"* (`:442-461` for areas and the outline;
/// `:411-439` for drills, which additionally keeps a box a box "to avoid small
/// corner cutoffs when taking the offset as an octagon"). Both of those are
/// [`TileShape::to_flavor`]`(FortyFive)`, so the family is one post-pass:
///
/// * a `Box` stays a `Box` — the octagon-that-is-a-box rule of §3.3;
/// * an `Oct` stays itself;
/// * a `Simplex` — a trace segment or an outline strip — becomes its bounding
///   octagon, which for 45°-restricted geometry is the same region.
///
/// The drill and area arms already ran `to_flavor(ctx.angle_restriction)` before
/// the compensation was applied, because §3.4 makes that a *board rule*; on a
/// 45° board this pass therefore finds them already octagonal and is a no-op on
/// them. On an any-angle board driving a 45° tree — which upstream never builds
/// — it octagonizes *after* the offset instead of before, a difference of at
/// most the corner chamfer and always in the conservative direction.
pub fn calculate_tree_shapes_for_family(
    item: &Item,
    tree_class: i32,
    family: TreeAngleFamily,
    ctx: ShapeContext<'_>,
    out: &mut Vec<ItemTileShape>,
) {
    calculate_tree_shapes(item, tree_class, ctx, out);
    match family {
        TreeAngleFamily::AnyAngle => {}
        TreeAngleFamily::FortyFive => {
            for tile in out.iter_mut() {
                if let Some(shape) = tile.shape.take() {
                    tile.shape = Some(shape.to_flavor(AngleRestriction::FortyFive));
                }
            }
        }
        TreeAngleFamily::Ninety => {
            for tile in out.iter_mut() {
                if let Some(shape) = tile.shape.take() {
                    tile.shape = Some(shape.to_flavor(AngleRestriction::Ninety));
                }
            }
        }
    }
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
    let compensation = |layer: i32| {
        ctx.clearances
            .tree_compensation(item_class, tree_class, layer)
    };

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
        for section in piece
            .enlarge(grow)
            .divide_into_sections(ctx.max_tree_shape_width)
        {
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
        AreaShape::Convex(pieces) => pieces.iter().cloned().map(TileShape::Simplex).collect(),
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
    use fr_geometry::planar::{IntBox, IntVector, Polyline, Simplex};

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
        assert_eq!(
            pieces[0].bounding_box(),
            IntBox::new(1000, 2000, 1100, 2100)
        );
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

    /// The payload is copied out of every leaf the walk reaches, and the leaf
    /// nodes are what the *descent* reads its bounds from, so its width is a
    /// cost every query pays whether it hits anything or not. Two extra words
    /// bought the layer and slot fast reject; the four the cached hull would add
    /// are argued against in [`LeafPayload`]'s docs, and this is the tripwire
    /// that makes adding them a deliberate act with a measurement attached.
    #[test]
    fn the_leaf_payload_stays_two_words_wider_than_the_pair_it_replaced() {
        assert_eq!(std::mem::size_of::<LeafPayload>(), 16);
    }

    /// The leaf payload's `slot` is a hint, never an identity, and the store is
    /// what decides whether the item behind it is still on the board.
    ///
    /// This is the guard the fast reject rewrote: it used to be
    /// `store.slot_of(payload.item)` returning `None`, and it is now
    /// `store.item_at_slot(payload.slot)` returning a different id (or nothing).
    /// A leaf the tree still holds for an item the store has dropped must stay
    /// invisible either way — including after the slot has been handed to
    /// somebody else, which is the case the id check exists for.
    #[test]
    fn a_leaf_outliving_its_item_in_the_store_is_invisible() {
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

        let query = TileShape::Box(IntBox::new(-10_000, -10_000, 10_000, 10_000));
        let mut stack = QueryStack::new();
        let mut out = Vec::new();
        tree.overlapping_tree_entries(&store, &query, Some(0), &[], &mut stack, &mut out);
        assert_eq!(out.len(), 2, "both segments are indexed and visible");

        // The store drops the item; the tree has not been told yet. The leaves
        // are still there — and must answer nothing.
        store.remove(a);
        tree.overlapping_tree_entries(&store, &query, Some(0), &[], &mut stack, &mut out);
        assert!(out.is_empty(), "a leaf without a live item reports nothing");
        assert_eq!(
            tree.leaf_count(),
            2,
            "the leaves really are still in the tree"
        );

        // Now somebody else takes the slot, still without the tree being told.
        // The slot table says item `a`, the store says item `b`: the id check is
        // the only thing between that mismatch and a wrong answer.
        let b = store.insert(via_item(&[2], IntPoint::new(500, 500)));
        assert_eq!(store.slot_of(b), Some(slot), "the slot really is recycled");
        tree.overlapping_tree_entries(&store, &query, Some(0), &[], &mut stack, &mut out);
        assert!(out.is_empty(), "the newcomer must not inherit a's leaves");

        // Resynchronizing restores the ordinary state, and the board is
        // consistent again.
        tree.remove(slot, a);
        assert_eq!(tree.leaf_count(), 0);
        tree.insert(slot, store.get(b).unwrap(), fixture.ctx());
        tree.overlapping_tree_entries(&store, &query, Some(0), &[], &mut stack, &mut out);
        assert_eq!(
            out,
            vec![TreeEntry {
                item: b,
                shape_index: 0,
                slot,
            }]
        );
        tree.assert_consistent(&store);
    }

    /// The unsorted variant is the same query with the tail cut off: same
    /// entries, no order promise.
    #[test]
    fn the_unsorted_walk_reports_exactly_what_the_sorted_one_does() {
        let fixture = Fixture::new();
        let mut store = ItemStore::new();
        let mut tree = SearchTree::new(0);
        for net in [1, 2, 3] {
            let id = store.insert(trace_item(
                &[net],
                &[
                    IntPoint::new(0, 0),
                    IntPoint::new(1000, 0),
                    IntPoint::new(1000, 1000),
                ],
                0,
                50,
            ));
            tree.insert(
                store.slot_of(id).unwrap(),
                store.get(id).unwrap(),
                fixture.ctx(),
            );
        }
        // A via, so the layer filter has something to reject on layer 1 too.
        let via = store.insert(via_item(&[4], IntPoint::new(500, 500)));
        tree.insert(
            store.slot_of(via).unwrap(),
            store.get(via).unwrap(),
            fixture.ctx(),
        );

        let query = TileShape::Box(IntBox::new(-10_000, -10_000, 10_000, 10_000));
        let mut stack = QueryStack::new();
        let (mut sorted, mut unsorted) = (Vec::new(), Vec::new());
        for (layer, ignore) in [
            (None, &[][..]),
            (Some(0), &[][..]),
            (Some(1), &[][..]),
            (Some(0), &[1][..]),
            (Some(0), &[1, 2][..]),
        ] {
            tree.overlapping_tree_entries(&store, &query, layer, ignore, &mut stack, &mut sorted);
            tree.overlapping_tree_entries_unsorted(
                &store,
                &query,
                layer,
                ignore,
                &mut stack,
                &mut unsorted,
            );
            assert_eq!(
                unsorted.len(),
                sorted.len(),
                "layer {layer:?}, ignore {ignore:?}"
            );
            let mut resorted = unsorted.clone();
            sort_entries(&mut resorted);
            assert_eq!(resorted, sorted, "layer {layer:?}, ignore {ignore:?}");
        }
        assert!(!sorted.is_empty(), "the sweep never hit anything");
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

    /// The bounds-only walk keeps every filter except the shape test, so it is
    /// a superset of the exact query, and it is a *strict* superset exactly when
    /// a leaf's bounding shape meets the query and its shape does not.
    ///
    /// A shallow triangle is the smallest thing with that gap: its bounding
    /// octagon has to square off a hypotenuse of slope −0.3, so the whole wedge
    /// above the hypotenuse is inside the bounds and outside the shape.
    #[test]
    fn the_bounds_only_walk_is_a_superset_in_the_same_order() {
        let clearances = ClearanceTable::uniform(3, 2, 20);
        let padstacks = PadstackTable::new();
        // Any-angle, or the keepout would be coarsened to its bounding octagon
        // on the way in and the gap this test needs would close (§3.4).
        let ctx = ShapeContext {
            clearances: &clearances,
            padstacks: &padstacks,
            layer_count: 2,
            angle_restriction: AngleRestriction::None,
            max_tree_shape_width: MAX_TREE_SHAPE_WIDTH,
        };

        let mut store = ItemStore::new();
        let mut tree = SearchTree::new(0);
        for _ in 0..3 {
            let id = store.insert(Item::new(
                NetNos::from_slice(&[1]),
                1,
                FixedState::SystemFixed,
                ItemKind::ObstacleArea(ObstacleArea {
                    area: PlacedArea::new(AreaShape::Simplex(Simplex::from_convex_polygon(&[
                        IntPoint::new(0, 0),
                        IntPoint::new(10_000, 0),
                        IntPoint::new(0, 3_000),
                    ]))),
                    layer: 0,
                    kind: ObstacleKind::Keepout,
                }),
            ));
            let slot = store.slot_of(id).unwrap();
            tree.insert(slot, store.get(id).unwrap(), ctx);
        }

        let mut stack = QueryStack::new();
        let mut exact = Vec::new();
        let mut loose = Vec::new();

        // Above the hypotenuse (y = 3000 − 0.3x is 1200 at x = 6000), inside the
        // bounding octagon on all eight directions.
        let wedge = TileShape::Box(IntBox::new(6_000, 2_000, 7_000, 2_500));
        tree.overlapping_tree_entries(&store, &wedge, Some(0), &[], &mut stack, &mut exact);
        tree.overlapping_tree_entries_bounds_only(
            &store,
            &wedge,
            Some(0),
            &[],
            &mut stack,
            &mut loose,
        );
        assert!(exact.is_empty(), "the exact test rejects the whole wedge");
        assert_eq!(loose.len(), 3, "the bounding shapes all reach it");
        assert!(loose.windows(2).all(|w| w[0].item > w[1].item));

        // Layer and net filters still apply — they are not shape tests.
        tree.overlapping_tree_entries_bounds_only(
            &store,
            &wedge,
            Some(1),
            &[],
            &mut stack,
            &mut loose,
        );
        assert!(loose.is_empty(), "layer filter survives");
        tree.overlapping_tree_entries_bounds_only(
            &store,
            &wedge,
            Some(0),
            &[1],
            &mut stack,
            &mut loose,
        );
        assert!(loose.is_empty(), "net filter survives");

        // Where the exact test does hit, the two agree entry for entry.
        let inside = TileShape::Box(IntBox::new(100, 100, 400, 400));
        tree.overlapping_tree_entries(&store, &inside, Some(0), &[], &mut stack, &mut exact);
        tree.overlapping_tree_entries_bounds_only(
            &store,
            &inside,
            Some(0),
            &[],
            &mut stack,
            &mut loose,
        );
        assert_eq!(exact.len(), 3);
        assert_eq!(exact, loose);
    }

    /// The containment shortcut is only allowed to skip the exact test where
    /// the exact test would have said yes, so a box query has to give the same
    /// answer as the same box handed over as a simplex — which never takes the
    /// shortcut.
    #[test]
    fn the_containment_shortcut_agrees_with_the_exact_test_on_a_sweep() {
        let clearances = ClearanceTable::uniform(3, 2, 20);
        let padstacks = PadstackTable::new();
        let ctx = ShapeContext {
            clearances: &clearances,
            padstacks: &padstacks,
            layer_count: 2,
            angle_restriction: AngleRestriction::None,
            max_tree_shape_width: MAX_TREE_SHAPE_WIDTH,
        };

        let mut store = ItemStore::new();
        let mut tree = SearchTree::new(0);
        // A trace (simplex tile shapes) and a keepout box (a box tile shape), so
        // both hull-bearing flavors are indexed.
        let trace = store.insert(trace_item(
            &[1],
            &[
                IntPoint::new(0, 0),
                IntPoint::new(4_000, 1_000),
                IntPoint::new(6_000, 5_000),
            ],
            0,
            120,
        ));
        tree.insert(
            store.slot_of(trace).unwrap(),
            store.get(trace).unwrap(),
            ctx,
        );
        let keepout = store.insert(Item::new(
            NetNos::EMPTY,
            1,
            FixedState::SystemFixed,
            ItemKind::ObstacleArea(ObstacleArea {
                area: PlacedArea::new(AreaShape::Box(IntBox::new(2_000, 3_000, 3_000, 4_000))),
                layer: 0,
                kind: ObstacleKind::Keepout,
            }),
        ));
        tree.insert(
            store.slot_of(keepout).unwrap(),
            store.get(keepout).unwrap(),
            ctx,
        );
        tree.assert_consistent(&store);

        let mut stack = QueryStack::new();
        let (mut shortcut, mut exact) = (Vec::new(), Vec::new());
        let (mut shortcut_items, mut exact_items) = (Vec::new(), Vec::new());
        let mut hits = 0usize;
        for llx in (-1_000..7_000).step_by(479) {
            for lly in (-1_000..6_000).step_by(397) {
                for size in [0, 250, 1_500, 20_000] {
                    let box_ = IntBox::new(llx, lly, llx + size, lly + size);
                    let as_box = TileShape::Box(box_);
                    // The same region as a simplex: `QueryContains::Never`, so
                    // every leaf takes the exact test.
                    let as_simplex = TileShape::Simplex(box_.to_simplex());
                    tree.overlapping_tree_entries(
                        &store,
                        &as_box,
                        Some(0),
                        &[],
                        &mut stack,
                        &mut shortcut,
                    );
                    tree.overlapping_tree_entries(
                        &store,
                        &as_simplex,
                        Some(0),
                        &[],
                        &mut stack,
                        &mut exact,
                    );
                    assert_eq!(shortcut, exact, "query {box_:?}");
                    tree.overlapping_items(
                        &store,
                        &as_box,
                        Some(0),
                        &[],
                        &mut stack,
                        &mut shortcut_items,
                    );
                    tree.overlapping_items(
                        &store,
                        &as_simplex,
                        Some(0),
                        &[],
                        &mut stack,
                        &mut exact_items,
                    );
                    assert_eq!(shortcut_items, exact_items, "items for {box_:?}");
                    hits += shortcut.len();
                }
            }
        }
        assert!(hits > 0, "the sweep never hit anything");
    }

    /// An unbounded stored shape has no hull, so it can never take the shortcut:
    /// the wedge below runs to infinity while its two corner approximations sit
    /// on the single point where its border lines cross, which a query box would
    /// otherwise "contain".
    #[test]
    fn only_a_bounded_non_empty_shape_gets_a_hull() {
        let wedge = TileShape::Simplex(Simplex::get_instance(&[
            fr_geometry::planar::Line::new(IntPoint::new(0, 0), IntPoint::new(100, 0)),
            fr_geometry::planar::Line::new(IntPoint::new(0, 0), IntPoint::new(0, -100)),
        ]));
        assert!(hull_box(&wedge).is_none());
        assert!(hull_box(&TileShape::Simplex(Simplex::EMPTY)).is_none());
        assert!(hull_box(&TileShape::Box(IntBox::EMPTY)).is_none());
        assert_eq!(
            hull_box(&TileShape::Box(IntBox::new(1, 2, 3, 4))),
            Some(IntBox::new(1, 2, 3, 4))
        );
        assert!(!QueryContains::of(&TileShape::Box(IntBox::EMPTY))
            .swallows(Some(&IntBox::new(1, 2, 3, 4))));
        assert!(!QueryContains::of(&TileShape::Box(IntBox::new(0, 0, 10, 10))).swallows(None));
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
            manager.insert(
                store.slot_of(id).unwrap(),
                store.get(id).unwrap(),
                fixture.ctx(),
            );
        }
        assert_eq!(manager.tree_count(), 1);
        manager.ensure_compensated_tree(1, &store, fixture.ctx());
        assert_eq!(manager.tree_count(), 2);
        assert_eq!(manager.compensated_tree(1).unwrap().leaf_count(), 3);
        manager.assert_consistent(&store);

        manager.reset_compensated_trees();
        assert_eq!(manager.tree_count(), 1);
    }

    // ---- the 45° angle family ------------------------------------------

    /// The 45° tree stores octagons where the any-angle tree stores simplices,
    /// and the region is the same for a 45°-restricted trace.
    #[test]
    fn the_fortyfive_family_octagonizes_trace_shapes_without_moving_them() {
        let fixture = Fixture::new();
        let item = trace_item(
            &[1],
            &[
                IntPoint::new(0, 0),
                IntPoint::new(1000, 0),
                IntPoint::new(2000, 1000),
            ],
            0,
            50,
        );

        let mut any = Vec::new();
        calculate_tree_shapes_for_family(
            &item,
            1,
            TreeAngleFamily::AnyAngle,
            fixture.ctx(),
            &mut any,
        );
        let mut oct = Vec::new();
        calculate_tree_shapes_for_family(
            &item,
            1,
            TreeAngleFamily::FortyFive,
            fixture.ctx(),
            &mut oct,
        );

        assert_eq!(any.len(), oct.len());
        for (a, o) in any.iter().zip(oct.iter()) {
            let a = a.shape.as_ref().unwrap();
            let o = o.shape.as_ref().unwrap();
            assert!(
                matches!(a, TileShape::Simplex(_)),
                "the any-angle tree keeps the exact offset simplex"
            );
            assert!(
                matches!(o, TileShape::Oct(_) | TileShape::Box(_)),
                "the 45° tree stores an octagon"
            );
            // A 45°-restricted segment's octagon-clipped offset simplex *is*
            // its own bounding octagon, so the box must not have moved.
            assert_eq!(a.bounding_box(), o.bounding_box());
        }
    }

    /// The two families are separate registry slots: asking for one must not
    /// hand back the other.
    #[test]
    fn the_manager_keys_trees_by_class_and_family() {
        let fixture = Fixture::new();
        let mut store = ItemStore::new();
        let mut manager = SearchTreeManager::new();
        let id = store.insert(trace_item(
            &[1],
            &[IntPoint::new(0, 0), IntPoint::new(1000, 0)],
            0,
            50,
        ));
        manager.insert(
            store.slot_of(id).unwrap(),
            store.get(id).unwrap(),
            fixture.ctx(),
        );

        manager.ensure_compensated_tree(1, &store, fixture.ctx());
        assert_eq!(manager.tree_count(), 2);
        assert!(manager
            .compensated_tree_with_family(1, TreeAngleFamily::FortyFive)
            .is_none());

        manager.ensure_compensated_tree_with_family(
            1,
            TreeAngleFamily::FortyFive,
            &store,
            fixture.ctx(),
        );
        assert_eq!(manager.tree_count(), 3, "both families coexist");
        let any = manager.compensated_tree(1).unwrap();
        let oct = manager
            .compensated_tree_with_family(1, TreeAngleFamily::FortyFive)
            .unwrap();
        assert_eq!(any.angle_family(), TreeAngleFamily::AnyAngle);
        assert_eq!(oct.angle_family(), TreeAngleFamily::FortyFive);
        assert_eq!(any.leaf_count(), oct.leaf_count());

        let slot = store.slot_of(id).unwrap();
        assert!(matches!(
            any.tile_shape(slot, id, 0).unwrap(),
            TileShape::Simplex(_)
        ));
        assert!(!matches!(
            oct.tile_shape(slot, id, 0).unwrap(),
            TileShape::Simplex(_)
        ));

        // Asking twice is idempotent, and the whole board stays consistent.
        manager.ensure_compensated_tree_with_family(
            1,
            TreeAngleFamily::FortyFive,
            &store,
            fixture.ctx(),
        );
        assert_eq!(manager.tree_count(), 3);
        manager.assert_consistent(&store);
    }

    /// The 45° family never *shrinks* a shape — the property that makes it a
    /// safe substitute in an obstacle index.
    #[test]
    fn the_fortyfive_family_never_loses_an_obstacle() {
        let fixture = Fixture::new();
        let item = trace_item(
            &[1],
            &[
                IntPoint::new(0, 0),
                // A deliberately non-45° segment: the octagon is a strict
                // over-approximation here, which is the conservative direction.
                IntPoint::new(1000, 300),
            ],
            0,
            50,
        );
        let mut any = Vec::new();
        calculate_tree_shapes_for_family(
            &item,
            0,
            TreeAngleFamily::AnyAngle,
            fixture.ctx(),
            &mut any,
        );
        let mut oct = Vec::new();
        calculate_tree_shapes_for_family(
            &item,
            0,
            TreeAngleFamily::FortyFive,
            fixture.ctx(),
            &mut oct,
        );
        for (a, o) in any.iter().zip(oct.iter()) {
            let a = a.shape.as_ref().unwrap();
            let o = o.shape.as_ref().unwrap();
            let a_box = a.bounding_box();
            let o_box = o.bounding_box();
            assert!(
                o_box.left_x() <= a_box.left_x()
                    && o_box.bottom_y() <= a_box.bottom_y()
                    && o_box.right_x() >= a_box.right_x()
                    && o_box.top_y() >= a_box.top_y(),
                "the octagon must contain the simplex"
            );
        }
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
