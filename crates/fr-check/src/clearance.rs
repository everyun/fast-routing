//! Clearance DRC: which pairs of items are too close, on which layer, by how
//! much.
//!
//! This is a direct port of `Item.clearanceViolations()` plus
//! `DesignRulesChecker.getAllClearanceViolations()` (`drc-connectivity.md` §2),
//! in the **headless** configuration the batch runner and the benchmark use —
//! which means clearance compensation on the default search tree is off, so tree
//! shapes are un-inflated copper and the clearance is split between the two
//! shapes at test time. The GUI's compensated branch is not ported; §7.1 shows
//! it double-counts.
//!
//! # The predicate, exactly
//!
//! For an ordered pair `(self, other)` of items and a pair of their tile shapes
//! that lie on the *same* layer `L`:
//!
//! ```text
//! is_obstacle = other.isObstacle(self)                  // asymmetric, §2.3
//!               && !tie_pin_exemption(self, other)      // trace <-> trace, §2.4
//! cl = matrix[other.cc][self.cc][L]                     // no safety margin
//! c1 = round(0.5 * cl);  c2 = cl - c1
//! violation <=> dimension(enlarge(s1, c1) ∩ enlarge(s2, c2)) == 2
//! ```
//!
//! Note what is *not* there: no Euclidean distance, no `gap < cl` comparison,
//! no tolerance, and no `+16`. The `clearance_safety_margin = 16` belongs to the
//! candidate query and to the router's insertion-time predicates only, which is
//! why the router is conservative by 16 units relative to this referee.
//!
//! # Candidate generation
//!
//! Upstream queries its R-tree with the bounds inflated by `1.2 * maxClearance`
//! and the `+16` margin, then re-tests every hit uncompensated. §2.2 licenses
//! any generator that is a **superset** of the true violating pairs, so this
//! module uses a per-layer sweep line over tile-shape bounding boxes inflated by
//! `max_clearance(cc, layer) + 16`. That is a superset because the required
//! clearance of a pair never exceeds either side's row maximum, and because
//! `enlarge` never moves a bound by more than `round(sqrt(2) * d)` — which the
//! `+16` absorbs at the rounding boundary. `property_test.rs` pins the sweep
//! against an `O(n²)` checker that skips candidate generation entirely.
//!
//! # Counting
//!
//! Deduplication is by `(min_id, max_id, layer)` (§2.7), so a pair overlapping
//! on three layers is three violations while a pair overlapping in five places
//! on one layer is one. The reported `expected`/`actual` then come from an
//! arbitrary one of the overlapping shape pairs — arbitrary upstream too, and
//! `expected` is identical across them because the matrix is symmetric in
//! practice.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use fr_board::item::{Item, ItemId, ItemKind};
use fr_geometry::planar::{IntBox, Limits};

use crate::anchors::AnchorIndex;
use crate::shape::TileShape;
use crate::view::{BoardView, ItemShape};

/// `ClearanceMatrix.clearance_safety_margin` — the query-side fudge, never part
/// of the verdict (`drc-connectivity.md` §1.2).
pub const CLEARANCE_SAFETY_MARGIN: i32 = 16;

/// Number of bisection steps in `calculateClearanceBetweenTwoShapes`
/// (`drc-connectivity.md` §2.6). Fixed at 16 upstream; kept fixed here so the
/// reported `actual` is reproducible rather than "as precise as we felt like".
pub const ACTUAL_CLEARANCE_STEPS: u32 = 16;

/// One deduplicated clearance violation.
///
/// `a` and `b` are the pair; which of the two upstream calls `firstItem` depends
/// on the order its item scan happened to hit them in, so this port normalises:
/// the first *evaluated* direction is the one whose `self` has the smaller id,
/// matching upstream's ascending insertion-order scan. When only the other
/// direction of the asymmetric obstacle predicate fires, `a` is the larger id.
#[derive(Debug, Clone, PartialEq)]
pub struct Violation {
    /// The item whose obstacle evaluation produced this record.
    pub a: ItemId,
    /// The other item.
    pub b: ItemId,
    /// The layer both offending tile shapes lie on.
    pub layer: i32,
    /// `matrix[b.cc][a.cc][layer]`, without the safety margin.
    pub expected: i32,
    /// The 16-step bisection lower bound on the true octagon-metric gap, in
    /// board units, clamped to `[0, expected]`.
    pub actual: f64,
    /// True when the **uncompensated** shapes already overlap in 2-D, i.e. this
    /// is bare copper on copper and `actual` is exactly `0`. This is the first
    /// branch of the bisection and the only case where `actual == 0` is a fact
    /// rather than a bound.
    pub actual_dim2: bool,
}

impl Violation {
    /// `expected - actual`, upstream's severity, used for sorting and for the
    /// µm shortfall statistics.
    pub fn shortfall(&self) -> f64 {
        (self.expected as f64 - self.actual).max(0.0)
    }

    /// The deduplication key: `(min id, max id, layer)`.
    pub fn key(&self) -> (u32, u32, i32) {
        let (lo, hi) = if self.a.raw() <= self.b.raw() {
            (self.a.raw(), self.b.raw())
        } else {
            (self.b.raw(), self.a.raw())
        };
        (lo, hi, self.layer)
    }
}

/// Whole-board clearance DRC.
///
/// Returns the deduplicated violation list, sorted by `(min id, max id, layer)`
/// so the output is a canonical function of the board and not of the sweep
/// order. `Vec::len()` is the `M` in Freerouting's
/// `"score (N unrouted and M violations)"` log line.
pub fn check_clearance<V: BoardView + ?Sized>(view: &V) -> Vec<Violation> {
    check_clearance_with(view, &AnchorIndex::build(view))
}

/// [`check_clearance`] reusing an [`AnchorIndex`] the caller already built.
///
/// The index is only needed for the tie-pin exemption (§2.4); building it is
/// `O(n log n)` and the connectivity pass needs one too, so a caller doing both
/// should build it once.
pub fn check_clearance_with<V: BoardView + ?Sized>(
    view: &V,
    anchors: &AnchorIndex,
) -> Vec<Violation> {
    let entries = Entries::build(view);
    let mut out: Vec<Violation> = Vec::new();
    // Membership only; never iterated, so the default hasher cannot leak an
    // order into the result.
    let mut seen: HashSet<(u32, u32, i32)> = HashSet::new();
    let mut active: Vec<u32> = Vec::new();

    for layer_range in entries.layer_runs() {
        active.clear();
        for &ei in layer_range {
            let e = &entries.entries[ei as usize];
            // Retire everything that can no longer reach the sweep front.
            active.retain(|&a| entries.entries[a as usize].inflated.ur.x >= e.inflated.ll.x);
            for &ai in &active {
                let other = &entries.entries[ai as usize];
                if other.item == e.item || !other.inflated.intersects(&e.inflated) {
                    continue;
                }
                test_pair(view, anchors, other, e, &mut seen, &mut out);
            }
            active.push(ei);
        }
    }

    out.sort_by_key(|v| v.key());
    out
}

// ---------------------------------------------------------------------------
// T2 — the same scan, fanned out
// ---------------------------------------------------------------------------

/// [`check_clearance_with`] on `threads` worker threads.
///
/// `threads <= 1` calls [`check_clearance_with`] itself, so a sequential run
/// executes exactly the code it always did.
///
/// # Why this is exactly the same answer, not merely an equivalent one
///
/// The sequential sweep tests the ordered pair `(active_entry, front_entry)`
/// once, in a fixed order, and the *first* pair to produce a violation for a
/// given `(min id, max id, layer)` key is the one whose `expected`/`actual` end
/// up in the output — a pair overlapping on several shape pairs is deduplicated
/// down to whichever the sweep hit first. Reproducing that under a partitioned
/// scan needs two facts:
///
/// 1. **The active set at any point is recoverable without running the sweep up
///    to it.** `order` is sorted by `inflated.ll.x` within a layer, so the
///    retirement test `a.ur.x >= front.ll.x` is monotone: once an entry is
///    retired it stays retired. The active set when the sweep reaches position
///    `i` is therefore exactly `{ j < i : entries[j].ur.x >= entries[i].ll.x }`,
///    which a chunk can rebuild from a prefix scan. That is what
///    [`seed_active`] does, and it is the only reason chunking a sweep line is
///    sound at all.
/// 2. **The sequential encounter order is expressible as a sort key.** It is:
///    `(front position in `order`, active position in `order`)`, because the
///    active list is kept in ascending insertion order by `Vec::retain` and
///    `Vec::push`, and `order` runs layer by layer. Each worker tags every
///    violation it finds with that pair, the merge sorts by
///    `(key, ordinal)` and keeps the first of each key — which *is* the one the
///    sequential scan would have kept.
///
/// The one thing that differs is work, not output: without a shared `seen` set a
/// worker re-evaluates a pair the sequential scan would have short-circuited.
/// That is bounded by the number of extra shape pairs per violating item pair,
/// which is small, and it buys a merge with no cross-thread state at all.
///
/// `the_parallel_scan_returns_exactly_the_sequential_result` in
/// `tests/property_test.rs` is the executable half of this argument: 25 random
/// boards, three thread counts, chunk floor forced down to 8 so that most chunks
/// have to rebuild an active set, `assert_eq!` on the whole `Vec<Violation>`.
///
/// # Measured
///
/// Over the 46-fixture suite under `fr-cli --import-only` (import → referee →
/// session writer, no router), total wall: **7.65 s at one worker, 4.68 s at
/// four, 4.46 s at eight**; a second one-worker pass measured 7.52 s, so the
/// baseline is stable to ~2 %. Import and session writing are unaffected by
/// `--threads` and account for most of what is left, which is why the end-to-end
/// number is 1.6× rather than 4×: the ~3.1 s that four workers remove is the
/// scan, and it is very nearly all of the scan.
pub fn check_clearance_parallel<V: BoardView + ?Sized + Sync>(
    view: &V,
    anchors: &AnchorIndex,
    threads: usize,
) -> Vec<Violation> {
    check_clearance_parallel_chunked(view, anchors, threads, MIN_CHUNK_ENTRIES)
}

/// Fewer entries than this in a chunk and the per-chunk active-set seed — which
/// is `O(distance from the layer run's start)` — costs more than the sweep it
/// enables. It only ever *raises* the chunk size, so a big board's chunk count
/// stays `O(threads)` and the total seeding stays `O(entries · threads)`.
const MIN_CHUNK_ENTRIES: usize = 512;

/// [`check_clearance_parallel`] with the chunk floor exposed.
///
/// Only the differential test needs this: at the real floor a hundred-item
/// random board is one chunk per layer, which never exercises [`seed_active`] —
/// the one piece of the parallel scan that has to reconstruct state rather than
/// merely reorder it.
#[doc(hidden)]
pub fn check_clearance_parallel_chunked<V: BoardView + ?Sized + Sync>(
    view: &V,
    anchors: &AnchorIndex,
    threads: usize,
    min_chunk_entries: usize,
) -> Vec<Violation> {
    if threads <= 1 {
        return check_clearance_with(view, anchors);
    }
    let entries = Entries::build(view);
    let tasks = entries.chunks(threads, min_chunk_entries);
    if tasks.len() <= 1 {
        return check_clearance_with(view, anchors);
    }
    let workers = threads.min(tasks.len());

    let cursor = AtomicUsize::new(0);
    let found: Vec<Vec<(u32, u32, Violation)>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                let cursor = &cursor;
                let entries = &entries;
                let tasks = &tasks;
                scope.spawn(move || {
                    let mut out: Vec<(u32, u32, Violation)> = Vec::new();
                    let mut active: Vec<u32> = Vec::new();
                    loop {
                        // Work stealing is safe here in a way it is not in the
                        // optimizer: a chunk's output carries its own position
                        // in the sequential order, so which worker took it
                        // cannot reach the answer.
                        let next = cursor.fetch_add(1, AtomicOrdering::Relaxed);
                        let Some(chunk) = tasks.get(next) else {
                            break;
                        };
                        scan_chunk(view, anchors, entries, *chunk, &mut active, &mut out);
                    }
                    out
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| match handle.join() {
                Ok(out) => out,
                Err(payload) => std::panic::resume_unwind(payload),
            })
            .collect()
    });

    let mut all: Vec<(u32, u32, Violation)> = found.into_iter().flatten().collect();
    // `(key, ordinal)`: the ordinal is the sequential encounter order, so the
    // first entry of each key run is the record the sequential scan kept.
    all.sort_unstable_by(|a, b| {
        a.2.key()
            .cmp(&b.2.key())
            .then_with(|| (a.0, a.1).cmp(&(b.0, b.1)))
    });
    all.dedup_by(|a, b| a.2.key() == b.2.key());
    all.into_iter().map(|(_, _, violation)| violation).collect()
}

/// One `[start, end)` slice of `Entries::order`, all of it inside the layer run
/// that begins at `run_start`.
type Chunk = (u32, u32, u32);

/// The sweep over one chunk, seeded with the active set the sequential scan
/// would have held on entering it.
///
/// `active` holds **positions in `Entries::order`**, not entry indices, so that
/// a hit's ordinal is a field read rather than a search: the position is exactly
/// the sequential scan's active-list order, because `retain` preserves order and
/// `push` appends the position just visited.
fn scan_chunk<V: BoardView + ?Sized>(
    view: &V,
    anchors: &AnchorIndex,
    entries: &Entries,
    chunk: Chunk,
    active: &mut Vec<u32>,
    out: &mut Vec<(u32, u32, Violation)>,
) {
    let (run_start, start, end) = chunk;
    seed_active(entries, run_start as usize, start as usize, active);
    for front_pos in start as usize..end as usize {
        let e = &entries.entries[entries.order[front_pos] as usize];
        active.retain(|&pos| {
            entries.entries[entries.order[pos as usize] as usize]
                .inflated
                .ur
                .x
                >= e.inflated.ll.x
        });
        for &active_pos in active.iter() {
            let other = &entries.entries[entries.order[active_pos as usize] as usize];
            if other.item == e.item || !other.inflated.intersects(&e.inflated) {
                continue;
            }
            if let Some(violation) = violation_for_pair(view, anchors, other, e) {
                out.push((front_pos as u32, active_pos, violation));
            }
        }
        active.push(front_pos as u32);
    }
}

/// The active set the sequential sweep holds when it reaches `start`.
///
/// `{ j in [run_start, start) : entries[order[j]].ur.x >= entries[order[start]].ll.x }`,
/// in ascending `j` — see [`check_clearance_parallel`] for why that is exact.
fn seed_active(entries: &Entries, run_start: usize, start: usize, active: &mut Vec<u32>) {
    active.clear();
    if start <= run_start {
        return;
    }
    let front_ll_x = entries.entries[entries.order[start] as usize].inflated.ll.x;
    for pos in run_start..start {
        if entries.entries[entries.order[pos] as usize].inflated.ur.x >= front_ll_x {
            active.push(pos as u32);
        }
    }
}

/// The 16-step bisection of `drc-connectivity.md` §2.6, on two raw tile shapes.
///
/// `c1`/`c2` are the split of `min_clearance` used by the outer test; the
/// bisection keeps their ratio so the answer is expressed in the same metric the
/// verdict used. Returns a value in `[0, min_clearance]`: a **lower bound** on
/// the octagon-metric gap, clamped at the requirement — a pair further apart
/// than the requirement never reaches this function.
pub fn clearance_between_shapes(
    s1: &TileShape,
    s2: &TileShape,
    min_clearance: i32,
    c1: i32,
    c2: i32,
) -> f64 {
    if s1.intersection(s2).dimension() == 2 {
        return 0.0;
    }
    let mut low = 0.0_f64;
    let mut high = min_clearance as f64;
    let sum = (c1 + c2) as f64;
    let (f1, f2) = if sum > 0.0 {
        (c1 as f64 / sum, c2 as f64 / sum)
    } else {
        (0.5, 0.5)
    };
    for _ in 0..ACTUAL_CLEARANCE_STEPS {
        let mid = (low + high) * 0.5;
        let e1 = s1.enlarge(mid * f1);
        let e2 = s2.enlarge(mid * f2);
        if e1.intersection(&e2).dimension() == 2 {
            high = mid;
        } else {
            low = mid;
        }
    }
    low
}

/// The tightest clearance between two items, over every layer they share tile
/// shapes on.
///
/// `None` when the two items have no pair of tile shapes on a common layer.
/// Otherwise the minimum over those layers of
/// [`clearance_between_shapes`], evaluated with the clearance the matrix
/// requires for the pair on that layer — so the answer is capped at the
/// requirement exactly as upstream's is, and `0.0` means the copper overlaps.
pub fn actual_clearance<V: BoardView + ?Sized>(view: &V, a: ItemId, b: ItemId) -> Option<f64> {
    let (item_a, item_b) = (view.item(a), view.item(b));
    let mut best: Option<f64> = None;
    for sa in view.shapes(a) {
        for sb in view.shapes(b) {
            if sa.layer != sb.layer {
                continue;
            }
            let cl = view.clearance(item_b.clearance_class, item_a.clearance_class, sa.layer);
            let c1 = half(cl);
            let c2 = cl - c1;
            let gap = clearance_between_shapes(&sa.shape, &sb.shape, cl, c1, c2);
            best = Some(match best {
                Some(prev) if prev <= gap => prev,
                _ => gap,
            });
        }
    }
    best
}

/// `round(0.5 * cl)` — upstream's `(int) Math.round(0.5 * min_clearance)`.
///
/// The matrix forces every clearance even (`ClearanceMatrix.setValue`), so in
/// practice `c1 == c2 == cl / 2` and the split is symmetric; the rounding only
/// matters for a matrix that was written past that guard.
#[inline]
pub(crate) fn half(clearance: i32) -> i32 {
    (0.5 * clearance as f64).round() as i32
}

// ---------------------------------------------------------------------------
// candidate sweep
// ---------------------------------------------------------------------------

/// Grows a bounding box by `margin` in `i64`, clamping to the coordinate
/// envelope.
///
/// A clearance class can legitimately carry `i32::MAX` (upstream's "infinite
/// clearance"), and `IntBox::offset` would wrap on that. The sweep only ever
/// compares these boxes, so clamping to `Limits::CRIT_INT` — outside which no
/// real board geometry lives — keeps the query a superset while keeping the
/// arithmetic in range.
fn inflate(bbox: &IntBox, margin: i32) -> IntBox {
    let crit = Limits::CRIT_INT as i64;
    let m = (margin.max(0) as i64).min(crit);
    let clamp = |v: i64| v.clamp(-crit, crit) as i32;
    IntBox::new(
        clamp(bbox.ll.x as i64 - m),
        clamp(bbox.ll.y as i64 - m),
        clamp(bbox.ur.x as i64 + m),
        clamp(bbox.ur.y as i64 + m),
    )
}

/// One tile shape of one item, with the box that bounds every shape it could
/// possibly violate against.
struct Entry {
    item: u32,
    shape: u32,
    layer: i32,
    inflated: IntBox,
}

struct Entries {
    entries: Vec<Entry>,
    /// Index into `entries`, grouped by layer and sorted by `inflated.ll.x`
    /// inside each group.
    order: Vec<u32>,
    /// Start offsets of the layer groups inside `order`, plus a terminating
    /// `order.len()`.
    layer_bounds: Vec<u32>,
}

impl Entries {
    fn build<V: BoardView + ?Sized>(view: &V) -> Entries {
        let mut entries = Vec::new();
        for (dense, id) in view.item_ids().iter().enumerate() {
            let item = view.item(*id);
            for (si, ItemShape { shape, layer }) in view.shapes(*id).iter().enumerate() {
                if shape.is_empty() {
                    continue;
                }
                let margin = view
                    .max_clearance(item.clearance_class, *layer)
                    .saturating_add(CLEARANCE_SAFETY_MARGIN);
                entries.push(Entry {
                    item: dense as u32,
                    shape: si as u32,
                    layer: *layer,
                    inflated: inflate(&shape.bounding_box(), margin),
                });
            }
        }
        let mut order: Vec<u32> = (0..entries.len() as u32).collect();
        order.sort_unstable_by_key(|&i| {
            let e = &entries[i as usize];
            (e.layer, e.inflated.ll.x, e.item, e.shape)
        });
        let mut layer_bounds = vec![0u32];
        for w in 1..order.len() {
            if entries[order[w] as usize].layer != entries[order[w - 1] as usize].layer {
                layer_bounds.push(w as u32);
            }
        }
        layer_bounds.push(order.len() as u32);
        Entries {
            entries,
            order,
            layer_bounds,
        }
    }

    fn layer_runs(&self) -> impl Iterator<Item = &[u32]> + '_ {
        self.layer_bounds
            .windows(2)
            .map(move |w| &self.order[w[0] as usize..w[1] as usize])
    }

    /// The sweep, cut into pieces one worker can take.
    ///
    /// A chunk never straddles a layer run — the sweep line resets at a layer
    /// boundary and a seeded active set has to reset with it. Within a run the
    /// cut is by count: `MIN_CHUNK_ENTRIES` keeps a chunk's seed scan (which is
    /// `O(distance from the run's start)`) from costing more than the sweep it
    /// enables, and aiming for several chunks per worker is what lets the
    /// cursor balance layers of wildly different sizes — which matters, because
    /// 40 of the 46 suite fixtures are two-layer boards and a per-layer split
    /// alone would cap the scan at 2×.
    fn chunks(&self, threads: usize, min_chunk_entries: usize) -> Vec<Chunk> {
        /// Chunks per worker to aim for, so the cursor has something to steal.
        const CHUNKS_PER_WORKER: usize = 4;

        let target = self
            .order
            .len()
            .div_ceil((threads * CHUNKS_PER_WORKER).max(1))
            .max(min_chunk_entries.max(1));
        let mut chunks = Vec::new();
        for window in self.layer_bounds.windows(2) {
            let (run_start, run_end) = (window[0] as usize, window[1] as usize);
            let mut start = run_start;
            while start < run_end {
                let end = (start + target).min(run_end);
                chunks.push((run_start as u32, start as u32, end as u32));
                start = end;
            }
        }
        chunks
    }
}

/// Evaluates both directions of the asymmetric obstacle predicate for one
/// candidate pair of tile shapes.
fn test_pair<V: BoardView + ?Sized>(
    view: &V,
    anchors: &AnchorIndex,
    x: &Entry,
    y: &Entry,
    seen: &mut HashSet<(u32, u32, i32)>,
    out: &mut Vec<Violation>,
) {
    if x.item == y.item {
        return;
    }
    debug_assert_eq!(x.layer, y.layer);
    let layer = x.layer;
    let (id_x, id_y) = (
        // `ids` is indexed by the dense position, the same space `Entry::item`
        // lives in.
        view.item_ids()[x.item as usize],
        view.item_ids()[y.item as usize],
    );
    let key = if id_x.raw() <= id_y.raw() {
        (id_x.raw(), id_y.raw(), layer)
    } else {
        (id_y.raw(), id_x.raw(), layer)
    };
    if seen.contains(&key) {
        return;
    }
    if let Some(v) = violation_for_pair(view, anchors, x, y) {
        seen.insert(key);
        out.push(v);
    }
}

/// Both directions of the asymmetric obstacle predicate for one candidate pair,
/// with no deduplication state — the half of [`test_pair`] the parallel scan
/// shares, because it has no shared `seen` set to consult.
///
/// `None` for a pair of shapes of the same item, which the sweep can produce and
/// which is never a violation.
fn violation_for_pair<V: BoardView + ?Sized>(
    view: &V,
    anchors: &AnchorIndex,
    x: &Entry,
    y: &Entry,
) -> Option<Violation> {
    if x.item == y.item {
        return None;
    }
    debug_assert_eq!(x.layer, y.layer);
    let layer = x.layer;
    let ids = view.item_ids();
    let (id_x, id_y) = (ids[x.item as usize], ids[y.item as usize]);
    // Upstream's outer scan is in item order, so the lower id is `self` first.
    let (lo, hi) = if id_x.raw() <= id_y.raw() {
        (x, y)
    } else {
        (y, x)
    };
    match evaluate(view, anchors, lo, hi, layer) {
        EvalVerdict::Hit(v) => Some(v),
        EvalVerdict::SymmetricGeomDisjoint => None,
        EvalVerdict::None => match evaluate(view, anchors, hi, lo, layer) {
            EvalVerdict::Hit(v) => Some(v),
            EvalVerdict::SymmetricGeomDisjoint | EvalVerdict::None => None,
        },
    }
}

enum EvalVerdict {
    Hit(Violation),
    /// `(lo, hi)` proved geometrically disjoint with `cl_fwd == cl_rev`, even
    /// `cl_fwd` (`c1 == c2`), and both shapes bounded — so `(hi, lo)` cannot
    /// produce a violation regardless of exemptions or obstacle asymmetry.
    SymmetricGeomDisjoint,
    None,
}

/// One ordered direction: is `other` an obstacle to `this`, and if so do their
/// enlarged shapes overlap in 2-D?
fn evaluate<V: BoardView + ?Sized>(
    view: &V,
    anchors: &AnchorIndex,
    this: &Entry,
    other: &Entry,
    layer: i32,
) -> EvalVerdict {
    let ids = view.item_ids();
    let (this_id, other_id) = (ids[this.item as usize], ids[other.item as usize]);
    let this_item = view.item(this_id);
    let other_item = view.item(other_id);

    if !other_item.is_obstacle(this_item) {
        return EvalVerdict::None;
    }
    if is_tie_pin_exempt(view, anchors, this_item, other_item) {
        return EvalVerdict::None;
    }
    if is_escape_via_exempt(this_item, other_item, layer) {
        return EvalVerdict::None;
    }

    let s1 = &view.shapes(this_id)[this.shape as usize].shape;
    let s2 = &view.shapes(other_id)[other.shape as usize].shape;

    let cl = view.clearance(other_item.clearance_class, this_item.clearance_class, layer);
    let symmetric_disjoint = || {
        if cl % 2 == 0
            && view.clearance(this_item.clearance_class, other_item.clearance_class, layer) == cl
            && s1.bounded_bounding_box().is_some()
            && s2.bounded_bounding_box().is_some()
        {
            EvalVerdict::SymmetricGeomDisjoint
        } else {
            EvalVerdict::None
        }
    };
    if let (Some(b1), Some(b2)) = (s1.bounded_bounding_box(), s2.bounded_bounding_box()) {
        if !inflate(&b1, cl.saturating_add(4)).intersects(&b2) {
            return symmetric_disjoint();
        }
    }
    let c1 = half(cl);
    let c2 = cl - c1;
    // Upstream's `c > 0 ? shape.enlarge(c) : shape` — the zero case must not
    // allocate a copy of the shape, which for a simplex means a `Vec<Line>`.
    let (owned1, owned2);
    let e1: &TileShape = if c1 > 0 {
        owned1 = s1.enlarge(c1 as f64);
        &owned1
    } else {
        s1
    };
    let e2: &TileShape = if c2 > 0 {
        owned2 = s2.enlarge(c2 as f64);
        &owned2
    } else {
        s2
    };
    if !e1.has_2dim_overlap(e2) {
        return symmetric_disjoint();
    }

    let actual_dim2 = s1.intersection(s2).dimension() == 2;
    let actual = if actual_dim2 {
        0.0
    } else {
        clearance_between_shapes(s1, s2, cl, c1, c2)
    };
    EvalVerdict::Hit(Violation {
        a: this_id,
        b: other_id,
        layer,
        expected: cl,
        actual,
        actual_dim2,
    })
}

/// `drc-connectivity.md` §2.4 — two traces of *different* nets may overlap where
/// a multi-net "tie pin" carrying both nets sits at the shared endpoint.
///
/// Faithful to upstream including its bug: when the other trace touches at
/// *both* ends, only the first end's contact list is scanned for the pin.
/// `getNormalContacts(point, ignoreNet = true)` ignores nets, so the contact set
/// is purely geometric; only the pin lookup inside it consults nets.
fn is_tie_pin_exempt<V: BoardView + ?Sized>(
    view: &V,
    anchors: &AnchorIndex,
    this: &Item,
    other: &Item,
) -> bool {
    let ItemKind::Trace(t) = &this.kind else {
        return false;
    };
    if !matches!(other.kind, ItemKind::Trace(_)) {
        return false;
    }
    let ids = view.item_ids();
    for point in [t.first_corner(), t.last_corner()] {
        let run = anchors.at(&point);
        // `found`: does `other` appear in this endpoint's contact list?
        let found = run.iter().any(|a| {
            let id = ids[a.item as usize];
            id == other.id && shares_layer(view.item(id), this)
        });
        if !found {
            continue;
        }
        return run.iter().any(|a| {
            let id = ids[a.item as usize];
            if id == this.id {
                return false;
            }
            let c = view.item(id);
            matches!(c.kind, ItemKind::Pin(_))
                && shares_layer(c, this)
                && c.shares_net(this)
                && c.shares_net(other)
        });
    }
    false
}

/// `drc-connectivity.md` §2.5 — a v2 escape via drops same-net violations on the
/// SMD layer it was fanned out into.
///
/// Ported for fidelity, and one-sided exactly as upstream is: the pin's own
/// evaluation is not filtered, so after deduplication the pair still surfaces.
/// Removing this function would not change any count; keeping it means the two
/// implementations disagree nowhere.
fn is_escape_via_exempt(this: &Item, other: &Item, layer: i32) -> bool {
    let ItemKind::Via(v) = &this.kind else {
        return false;
    };
    v.is_escape_via
        && v.escape_via_smd_layer >= 0
        && v.escape_via_smd_layer == layer
        && other.shares_net(this)
}

/// `Item.sharesLayer` — `max(firstLayer) <= min(lastLayer)`.
#[inline]
pub(crate) fn shares_layer(a: &Item, b: &Item) -> bool {
    let (ra, rb) = (a.layer_range(), b.layer_range());
    ra.first.max(rb.first) <= ra.last.min(rb.last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_rounds_like_java() {
        assert_eq!(half(0), 0);
        assert_eq!(half(20), 10);
        assert_eq!(half(21), 11);
        assert_eq!(half(-4), -2);
    }
}
