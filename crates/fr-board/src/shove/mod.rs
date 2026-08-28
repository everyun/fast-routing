//! Push-and-shove: the board-side half of `docs/upstream-study/shove.md`.
//!
//! Ported from the **2.3.0 release tree**
//! (`/tmp/fr-java-release/src/main/java/app/freerouting/board/`), which ships
//! the v19 class names — `ShapeTraceEntries`, `CalcFromSide`,
//! `CalcShapeAndFromSide`, `ShoveTraceAlgo`, `ForcedPadAlgo`, `ForcedViaAlgo`,
//! `MoveDrillItemAlgo` — not the `board.optimize` / `board.actions` split of
//! git master (conformance appendix, "Headline"). Semantic deltas B1-B14 of
//! that appendix are applied where they touch this crate: B3/B4/B5 (the extra
//! start-trace circle in the forced-via check), §13.1 (the `CalcFromSide`
//! nearest-side fallback) and §13.2 (the hole-clearance probe).
//!
//! | module | upstream |
//! |---|---|
//! | [`from_side`] | `CalcFromSide` — which border edge the shove comes from |
//! | [`entries`] | `ShapeTraceEntries` — entry points, stack levels, substitute pieces |
//! | [`shape_and_from_side`] | `CalcShapeAndFromSide` — dog-ear cutting per substitute segment |
//! | [`trace`] | `ShoveTraceAlgo` — static length check, instance check, insert, spring-over |
//! | [`drill`] | `MoveDrillItemAlgo` — via translation and via cascades |
//! | [`pad`] | `ForcedPadAlgo` — pad shoving and `in_front_of_pad` |
//! | [`via`] | `ForcedViaAlgo` — per-layer forced via check/insert |
//! | [`geom`] | the geometry predicates `fr-geometry` does not expose |
//!
//! # The two contexts
//!
//! Upstream's contract is *"`check` is side-effect free; `insert` may damage the
//! board if it fails"* (`ShoveTraceAlgo.java:295-297`). Here that split is in
//! the type system: [`ShoveCheck`] borrows the board immutably and cannot
//! mutate it at all, while [`ShoveInsert`] borrows it mutably.
//!
//! On top of that, `shove.md` §15 asks for the damage to stop being real:
//!
//! > Make insert transactional instead of "may damage the board". […] failures
//! > become free rollbacks. Keep an equivalence mode that mimics
//! > check-then-insert for regression testing.
//!
//! [`ShoveInsert::insert_transaction`] is that: it opens a
//! [`Board::begin`](crate::board::Board::begin) mark, runs the cascade, and
//! rolls the whole thing back when any level returns false. The board a failed
//! shove leaves behind is bit-identical to the one it started from, so callers
//! no longer have to own an undo. `check` is still ported in full and still
//! run by the eager-equivalence tests, which assert that
//! `check == insert_transaction` on the same board.
//!
//! # Budgets
//!
//! [`ShoveBudget`] carries the three recursion depths with upstream's
//! decrement points: trace depth only on substitute-trace recursion, via depth
//! only on via cascades, spring-over depth only when spring-over actually
//! changed a polyline (conformance appendix, "Recursion budgets").

pub mod drill;
pub mod entries;
pub mod from_side;
pub mod geom;
pub mod pad;
pub mod shape_and_from_side;
pub mod trace;
pub mod via;

use fr_geometry::planar::{FloatPoint, IntBox, IntOctagon};

use crate::board::Board;
use crate::item::ItemId;
use crate::shape::AngleRestriction;

pub use entries::{ShapeTraceEntries, SubstitutePiece, C_OFFSET_ADD};
pub use from_side::FromSide;
pub use pad::CheckDrillResult;

/// `ShoveTraceAlgo.check`'s "the shove succeeds over the whole segment"
/// sentinel, upstream's `Integer.MAX_VALUE`.
///
/// Kept as that exact value rather than [`f64::INFINITY`] because the maze
/// compares against it (`MazeShoveTraceAlgo.java:158`) *and* feeds it into
/// `Math.min` arithmetic further down.
pub const FULL_SHOVE: f64 = i32::MAX as f64;

/// The three recursion depths, from `AutorouteControl` (`:182-185`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShoveBudget {
    /// `max_shove_trace_recursion_depth`, default 20.
    pub max_recursion_depth: i32,
    /// `max_shove_via_recursion_depth`, default 5.
    pub max_via_recursion_depth: i32,
    /// `max_spring_over_recursion_depth`, default 5.
    pub max_spring_over_recursion_depth: i32,
}

impl ShoveBudget {
    /// Upstream's defaults.
    pub const DEFAULT: ShoveBudget = ShoveBudget {
        max_recursion_depth: 20,
        max_via_recursion_depth: 5,
        max_spring_over_recursion_depth: 5,
    };
}

impl Default for ShoveBudget {
    fn default() -> Self {
        ShoveBudget::DEFAULT
    }
}

/// `ChangedArea` as the shove stack sees it: a place to record touched points
/// and a per-layer clip for `normalize`.
///
/// The concrete octagon-per-layer implementation belongs to the router
/// (`fr_router::insert_forced::ChangedArea`); the board only needs these two
/// operations, and the check paths need neither, which is why
/// [`NoChangedArea`] exists.
pub trait ChangedAreaSink {
    /// `ChangedArea.join(FloatPoint, layer)`.
    fn join_point(&mut self, point: FloatPoint, layer: i32);
    /// `ChangedArea.get_area(layer)`, or `None` when there is no region — which
    /// is upstream's `board.changed_area == null` case in
    /// `ForcedPadAlgo.forced_pad` (`:293-298`).
    fn area(&self, layer: i32) -> Option<IntOctagon>;
}

/// The sink for callers that do not track a changed area.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoChangedArea;

impl ChangedAreaSink for NoChangedArea {
    fn join_point(&mut self, _point: FloatPoint, _layer: i32) {}
    fn area(&self, _layer: i32) -> Option<IntOctagon> {
        None
    }
}

/// Reusable obstacle buffers, one per open recursion level.
///
/// `shove.md` §15 ("Arena the shove transaction"): a cascade allocates, per
/// level, an obstacle `Collection`, a `LinkedList<Via>` and an `EntryPoint` per
/// intersection, *all* of which is garbage on failure. Two of the three are
/// already gone — [`ShapeTraceEntries`] keeps its entry points in a `Vec` with
/// `u32` links, and a substitute piece is a plan rather than a board item — and
/// this is the third: the obstacle id list is borrowed from a stack of at most
/// `max_recursion_depth` buffers and handed straight back, so a warm cascade
/// allocates nothing.
#[derive(Debug, Default)]
pub struct ShoveScratch {
    free: Vec<Vec<ItemId>>,
}

impl ShoveScratch {
    /// An empty buffer, reusing a retired one when there is one.
    pub fn take(&mut self) -> Vec<ItemId> {
        let mut buffer = self.free.pop().unwrap_or_default();
        buffer.clear();
        buffer
    }

    /// Retires a buffer for the next level.
    pub fn give(&mut self, mut buffer: Vec<ItemId>) {
        buffer.clear();
        self.free.push(buffer);
    }
}

/// The read-only half of the shove stack: everything spelled `check` upstream.
pub struct ShoveCheck<'a> {
    /// The board, immutably — the type-level statement of "no side effects".
    pub board: &'a Board,
    /// `RoutingBoard.get_bounding_box()`; a shape leaving it fails.
    pub bounds: IntBox,
    /// `p_shape instanceof IntBox` — true exactly when the board's angle
    /// restriction makes every shove shape a bounding box.
    pub orthogonal: bool,
    /// `RoutingBoard.shove_failing_obstacle`: the blame item, for the UI and
    /// for tests. The autorouter never reads it.
    pub failing_obstacle: Option<ItemId>,
    /// Reusable obstacle buffers.
    pub scratch: ShoveScratch,
}

impl<'a> ShoveCheck<'a> {
    /// Wraps a board for one check cascade.
    pub fn new(board: &'a Board, bounds: IntBox) -> ShoveCheck<'a> {
        let orthogonal = board.angle_restriction() == AngleRestriction::Ninety;
        ShoveCheck {
            board,
            bounds,
            orthogonal,
            failing_obstacle: None,
            scratch: ShoveScratch::default(),
        }
    }

    /// `RoutingBoard.set_shove_failing_obstacle`.
    pub(crate) fn blame(&mut self, item: Option<ItemId>) {
        if item.is_some() {
            self.failing_obstacle = item;
        }
    }
}

/// The mutating half: everything spelled `insert` / `forced_*` upstream.
pub struct ShoveInsert<'a> {
    /// The board being mutated.
    pub board: &'a mut Board,
    /// `RoutingBoard.get_bounding_box()`.
    pub bounds: IntBox,
    /// `p_shape instanceof IntBox`.
    pub orthogonal: bool,
    /// Where inserted geometry is reported for pull-tight and normalize.
    pub changed: &'a mut dyn ChangedAreaSink,
    /// The blame item.
    pub failing_obstacle: Option<ItemId>,
    /// Reusable obstacle buffers.
    pub scratch: ShoveScratch,
}

impl<'a> ShoveInsert<'a> {
    /// Wraps a board for one insert cascade.
    pub fn new(
        board: &'a mut Board,
        bounds: IntBox,
        changed: &'a mut dyn ChangedAreaSink,
    ) -> ShoveInsert<'a> {
        let orthogonal = board.angle_restriction() == AngleRestriction::Ninety;
        ShoveInsert {
            board,
            bounds,
            orthogonal,
            changed,
            failing_obstacle: None,
            scratch: ShoveScratch::default(),
        }
    }

    /// A read-only view over the same board, for the `check` calls the insert
    /// path makes (`MoveDrillItemAlgo.shove_vias` verifies every via move
    /// before performing it).
    pub(crate) fn checker(&self) -> ShoveCheck<'_> {
        ShoveCheck {
            board: self.board,
            bounds: self.bounds,
            orthogonal: self.orthogonal,
            failing_obstacle: None,
            scratch: ShoveScratch::default(),
        }
    }

    pub(crate) fn blame(&mut self, item: Option<ItemId>) {
        if item.is_some() {
            self.failing_obstacle = item;
        }
    }
}

/// Runs `body` inside a board transaction, rolling back unless it returns true.
///
/// This is the "make insert transactional" restructuring of `shove.md` §15 in
/// one function: every `insert`-family entry point that upstream documents as
/// *"the database may be damaged"* is wrapped in it, so a failed shove is a
/// no-op instead of a caller-owned undo.
pub fn in_transaction<R>(board: &mut Board, body: impl FnOnce(&mut Board) -> (bool, R)) -> (bool, R) {
    let mark = board.begin();
    let (ok, result) = body(board);
    if ok {
        board.commit(mark);
    } else {
        board.rollback_to(mark);
    }
    (ok, result)
}
