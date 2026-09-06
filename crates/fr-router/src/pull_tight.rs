//! Post-route trace optimization — upstream's `PullTightAlgo` family.
//!
//! Ported from the 2.3.0 release tree (`/tmp/fr-java-release`, revision
//! `2d4de019`): `board/PullTightAlgo.java` (the framework, 473 L),
//! `board/PullTightAlgo45.java` (the 45° local moves, 622 L) and
//! `RoutingBoard.opt_changed_area`. The spec is
//! `docs/upstream-study/pull-tight-insert.md` §5–§8, whose "Release-jar
//! conformance" appendix **overrides** the spec body.
//!
//! # What is here
//!
//! | item | upstream |
//! |---|---|
//! | [`PullTightAlgo`] | `PullTightAlgo` + `get_instance` (`:71-85`) |
//! | [`PullTightAlgo::opt_changed_area`] | the regional driver (`:88-133`) |
//! | [`PullTightAlgo::check_trace_shape`] | `BasicBoard.check_trace_shape` (`:1166-1221`) |
//! | [`PullTightAlgo::reposition_lines`] | base driver (`:175`) + base `reposition_line` (`:195`) |
//! | [`PullTightAlgo::skip_segments_of_length_0`] | `:292` |
//! | [`PullTightAlgo::trace_pull_tight`] | `PolylineTrace.pull_tight` (`:827-875`) |
//! | `PullTightAlgo::pull_tight_flavor` | `get_instance`'s angle dispatch (`:71-85`) |
//! | [`crate::pull_tight45`] | `PullTightAlgo45` — `reduce_corners`, `smoothen_corners` |
//! | [`crate::pull_tight90`] | `PullTightAlgo90` — `try_skip_second_corner`, `try_skip_corners` |
//! | [`crate::pull_tight_any`] | `PullTightAlgoAnyAngle` — the `NONE`-mode rules |
//!
//! # Deliberate deltas from the Java text
//!
//! * **Explicit worklists, not live iterators.** `opt_changed_area` snapshots the
//!   region's trace ids and revalidates each against the store before touching
//!   it, instead of upstream's break-out-of-a-live-`Collection` dance
//!   (`:118-127`). Spec §11 "Iterator invalidation discipline" prescribes this,
//!   and it is what makes the driver safe when a move removes an item. The
//!   revalidation is **not** about id churn — `change` keeps the id — but about
//!   the normalization that follows every move: it can split a trace into new
//!   ids or delete one that collapsed, and the §6.4 branch removes and
//!   re-inserts outright.
//! * **The binding-constraint cache** (spec §11, "`check_trace_shape` dominates
//!   runtime"). Every binary search re-queries the tree from scratch upstream;
//!   here the first obstacle that rejected a candidate is remembered as
//!   `(item, tile index)` and re-tested directly before the full query runs. The
//!   candidate shapes of one binary search shrink monotonically toward the same
//!   blocker, so the cached tile answers most rejections without a traversal.
//!   The cache is only ever consulted to prove a *rejection*: an acceptance
//!   always costs the full query, so it cannot change the result.
//! * **`change(new_polyline)` is in place**, upstream's
//!   `search_tree_manager.change_entries` leaf reuse
//!   (`PolylineTrace.java:942-1009`) — see
//!   [`Board::change_trace_polyline`](fr_board::board::Board::change_trace_polyline).
//!   The item id survives the move, the leaves and tile shapes of every segment
//!   the move did not touch are reused, and the trace's journal pre-image is
//!   cloned once per transaction rather than once per move. This is the call the
//!   whole family's cost lands on: a move edits two or three lines of a polyline
//!   that routinely has thirty, and re-offsetting the other twenty-seven was the
//!   port's largest single performance debt.
//!
//!   **The id surviving is not free of consequences, and that is the point.**
//!   The previous shape of this call was `remove_item` +
//!   `insert_trace_without_cleaning`, so every pull-tight handed the trace a
//!   fresh, maximal id — and `fr-board` scans the store and sorts every obstacle
//!   list by *descending* id, so a just-optimized trace jumped to the head of
//!   both. Restoring upstream's in-place `change` restores upstream's ordering,
//!   which moves routed results on some boards. The leaf reuse itself is
//!   result-neutral and is checked to be (`FR_CHANGE_ENTRIES=rebuild`); the id
//!   is the variable that moves boards. Do not read a board that got worse
//!   across this change as a bug in the reuse without running that knob first.
//! * **Fork/end smoothing (`smoothen_end_corners_at_trace`, §6.4) is ported
//!   for both flavors**, including the improvement round's
//!   `insert_trace_without_cleaning` + `split_traces` + `normalize_traces`
//!   triple. The one thing it does not carry over is upstream's second
//!   `board.remove_item(curr_trace)` at `:383`, which is a no-op on an item
//!   already off the board. §6.4 is also why a *round* is not a unit of bounded
//!   work — one of its rounds normalizes a whole net — and therefore why the
//!   driver arms a [`StallGuard`] as well as the round counter; see that type.
//! * **The clearance oracle is the DRC's own arithmetic.** Upstream dispatches
//!   `BasicBoard.check_trace_shape` on `is_clearance_compensation_used()`, which
//!   is `false` in every headless run (`SearchTreeManager.java:34`; only the GUI
//!   flips it). So the acceptance check runs against the **default,
//!   uncompensated** tree via `overlapping_tree_entries_with_clearance`
//!   (`ShapeSearchTree.java:430-480`): both the candidate and the stored shape
//!   are enlarged by half the pairwise clearance **plus
//!   `clearance_safety_margin`** (`get_value(..., true)`), "to create symmetry".
//!   That is exactly the checker's enlargement with a margin on top, so a move
//!   this oracle accepts cannot come back as a clearance violation. The
//!   compensated tree the maze searches against is a *rounding-different* model
//!   (its pre-inflated diagonal borders land up to one intercept unit inside the
//!   symmetric enlargement), and a pull-tight bisection that homes to compensated
//!   tangency commits a trace the referee then flags — that was Issue034's +1
//!   violation with fanout on (`fanout034`). The compensated tree remains the
//!   maze's guide; it is never the acceptance oracle here.
//! * **90° mode has its own rules** ([`crate::pull_tight90`]): upstream's
//!   `get_instance` maps `NINETY_DEGREE` to `PullTightAlgo90` (`:73-75`), whose
//!   pipeline is `try_skip_second_corner` / `try_skip_corners` /
//!   `reposition_lines` — no chamfering — and whose fork-smoothing hooks both
//!   return `null`. (An earlier revision of this file claimed `NINETY_DEGREE`
//!   shared the 45° rules; that contradicts the release source and chamfered
//!   90° boards into diagonals.)

use std::time::{Duration, Instant};

use fr_board::board::Board;
use fr_board::clearance::{ClearanceSource, CLEARANCE_SAFETY_MARGIN};
use fr_board::item::{FixedState, ItemId, ItemKind};
use fr_board::net_nos::NetNos;
use fr_board::search_tree::TreeEntry;
use fr_board::shape::{AngleRestriction, TileShape};
use fr_datastructures::arena_shape_tree::QueryStack;
use fr_datastructures::signum::Signum;
use fr_geometry::planar::{
    Direction, FloatPoint, IntBox, IntOctagon, IntPoint, Limits, Line, Point, Polyline, Side,
    Simplex,
};
use fr_rules::board_rules::BoardRules;

use crate::batch::budget::BudgetMode;
use crate::batch::schedule::JobDeadline;
use crate::insert_forced::ChangedArea;

/// `c_max_cos_angle` (`PullTightAlgo.java:24`) — angles closer than ~2.6° to
/// 180° are numerically unstable and are not divided further.
pub const MAX_COS_ANGLE: f64 = 0.999;

/// `c_min_corner_dist_square` (`PullTightAlgo.java:27`) — corners closer than
/// `sqrt(0.9)` count as equal.
pub const MIN_CORNER_DIST_SQUARE: f64 = 0.9;

/// The floor upstream puts under the user's `trace_pull_tight_accuracy`
/// (`PullTightAlgo.java:83`: `Math.max(p_min_translate_dist, 100)`).
pub const MIN_TRANSLATE_DIST_FLOOR: i32 = 100;

/// The default `pull_tight_accuracy` (`AutorouteControl.java:182`).
pub const DEFAULT_PULL_TIGHT_ACCURACY: i32 = 500;

/// `TIME_LIMIT_TO_PREVENT_ENDLESS_LOOP` — the per-connection pull-tight budget
/// (`BatchAutorouter.java:57`, used at `:1331`), which
/// [`crate::batch::route_with_control`] arms verbatim in
/// [`BudgetMode::Wall`].
pub const TIME_LIMIT_TO_PREVENT_ENDLESS_LOOP_MS: u64 = 1000;

/// `PULL_TIGHT_TIME_LIMIT` — the budget upstream's removal path uses
/// (`RoutingBoard.java:49`, used at `:144`), and what the keep-point pull-tight
/// inside [`crate::insert_forced`] is armed with.
pub const PULL_TIGHT_TIME_LIMIT_MS: u64 = 2000;

/// Optimization rounds that buy one *virtual* millisecond in
/// [`BudgetMode::Deterministic`] — the pull-tight half of
/// [`crate::batch::budget::DEFAULT_EFFORT_PER_VIRTUAL_MS`], in the currency this
/// family actually spends.
///
/// **Provisional, and deliberately generous.** Measured on the heaviest single
/// `opt_changed_area` in the fixture suite (`Issue102`, one connection):
/// 17 229 rounds in 1 050 ms, i.e. ~16 rounds per millisecond. At 100 the
/// deterministic guard is ~6× looser than the wall clock it stands in for, so
/// it cannot bind *earlier* than upstream's own cutoff would on a healthy
/// board — erring in that direction keeps `--deterministic` a mode that changes
/// timing, not results — while still stopping a region that has stopped
/// converging.
pub const ROUNDS_PER_VIRTUAL_MS: u64 = 100;

/// The stall guard a caller that never armed one falls back to: the number of
/// per-polyline optimization rounds one [`PullTightAlgo`] may spend.
///
/// Every individual move is monotone (spec §5.5), so this exists only to stop a
/// pathological *region* from spinning; it is a safety valve, not a quality
/// knob. It is far above what any fixture's healthy connection reaches
/// (measured maximum: 17 229), and a run that trips it gets upstream's
/// behaviour of leaving the region partly optimized.
///
/// It is **not** a substitute for a wall budget, and the M3 gate is why that
/// sentence is here. §6.4 fork smoothing made a round unboundedly expensive —
/// one round normalizes a whole net — and on `Issue209-split10` the driver
/// diverges outright: each round adds a trace to the net (372 → 650 over 300
/// regional rounds) so each subsequent round costs more, and 2 000 000 of them
/// is not a bound anybody can wait for. Drivers arm a [`StallGuard`], which in
/// [`BudgetMode::Wall`] is upstream's own millisecond cutoff.
pub const DEFAULT_ROUND_BUDGET: u64 = 2_000_000;

/// The numeric backoff every binary search takes when a translation overshot
/// its own side test (`shorten_value = sign * 0.5`, all loops).
pub const NUMERIC_BACKOFF: f64 = 0.5;

/// What stops a pull-tight that has stopped converging, in whichever currency
/// the run's [`BudgetMode`] spends.
///
/// This is `ARCHITECTURE.md` "Budgets" applied to `PullTightAlgo`: the default
/// mode keeps *upstream's wall-clock constants verbatim* — "per-connection
/// `100 s·2^(pass−1)`, **pull-tight 1000/2000 ms**" — and `--deterministic`
/// "replaces wall checks with effort-based virtual time". The two currencies
/// are the same duality [`crate::batch::budget::ConnectionClock`] already
/// spends for the maze; here the effort unit is one optimization round.
///
/// Whichever guard is armed, it is only ever the *inner* bound: the ambient job
/// deadline ([`JobDeadline::ambient`], `--job-timeout` and the signal handler's
/// stop flag) is polled by [`PullTightAlgo::is_stop_requested`] in both modes,
/// because a stop from outside the router is not a budget decision. That is the
/// same rule [`crate::batch::schedule::PassContext::connection_budget`] states
/// for the per-connection clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StallGuard {
    /// Upstream's millisecond cutoff, verbatim.
    Wall(Duration),
    /// Its deterministic equivalent, in optimization rounds.
    Rounds(u64),
}

impl StallGuard {
    /// The guard for `limit_ms` of upstream's budget under `mode`.
    ///
    /// [`BudgetMode::Wall`] is the constant itself; [`BudgetMode::Deterministic`]
    /// buys rounds with it at [`ROUNDS_PER_VIRTUAL_MS`].
    pub fn for_mode(mode: BudgetMode, limit_ms: u64) -> StallGuard {
        match mode {
            BudgetMode::Wall => StallGuard::Wall(Duration::from_millis(limit_ms)),
            BudgetMode::Deterministic => {
                StallGuard::Rounds(limit_ms.saturating_mul(ROUNDS_PER_VIRTUAL_MS))
            }
        }
    }
}

/// The obstacle that rejected the previous candidate shape, remembered so the
/// next candidate can be rejected without a tree traversal.
#[derive(Debug, Clone, Copy)]
struct BindingObstacle {
    item: ItemId,
    /// Index of the blocking tile inside the item's tree shapes.
    shape_index: u16,
    /// Store revision the cache was taken at; anything else invalidates it.
    revision: u64,
}

/// One contact trace's geometry at a fork, as §6.4's rules read it.
///
/// The four fields are upstream's `curr_other_trace_corner_approx`,
/// `curr_other_trace_line`, `curr_other_prev_trace_line` and the contact's own
/// corner count, already resolved for whichever end of the contact meets the
/// trace being smoothed (`PullTightAlgo45.java:439-450`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct ContactTraceLines {
    /// The contact's neighbouring corner, approximated.
    pub(crate) corner_approx: FloatPoint,
    /// The contact's line at the fork, oriented **away** from the fork.
    pub(crate) line: Line,
    /// The line one further into the contact.
    pub(crate) prev_line: Line,
    /// The contact's corner count, which the any-angle end rule filters on.
    pub(crate) corner_count: usize,
}

/// The winning contact of a fork-smoothing scan: which branch fired, and the
/// contact geometry that fired it.
pub(crate) struct ForkCandidate {
    /// True when the joint doubles back at an acute angle (the acid trap).
    pub(crate) acute_angle: bool,
    /// True when the joint is perpendicular and the next line continues along
    /// the contact.
    pub(crate) bend: bool,
    /// The contact that set the flags — `None` when neither fired.
    pub(crate) contact: Option<ContactTraceLines>,
    /// `curr_prev_end_corner.side_of(contact.line)` for that contact.
    pub(crate) prev_corner_side: Side,
}

/// Everything `prepare_trace` reads off a trace before optimizing it, so the
/// board borrow can be released before the optimization mutates it.
struct TracePrep {
    layer: i32,
    half_width: i32,
    polyline: Polyline,
    clearance_class: i32,
    nets: NetNos,
}

/// `PullTightAlgo` — the trace optimizer, bound to one board for its lifetime.
///
/// Construct with [`PullTightAlgo::new`], which is upstream's `get_instance`
/// (`:71-85`) minus the angle dispatch: the 45° rules are the only ones ported,
/// see the module docs.
pub struct PullTightAlgo<'b> {
    /// The board being optimized.
    pub board: &'b mut Board,
    rules: &'b BoardRules,
    /// When non-empty, only traces whose net array equals this are optimized
    /// (`PolylineTrace.java:838`, and the D2 wrapper filter).
    only_net_no_arr: Vec<i32>,
    /// Moves whose touched corners fall outside this are rejected (`:96`).
    curr_clip_shape: Option<IntOctagon>,
    /// Termination granularity of every binary search, in board units.
    min_translate_dist: f64,
    keep_point: Option<Point>,
    keep_point_layer: i32,
    /// Optional wall-clock cutoff — upstream's `TimeLimit`, armed by
    /// [`PullTightAlgo::with_stall_guard`] in [`BudgetMode::Wall`].
    deadline: Option<Instant>,
    /// The deterministic stall guard, decremented once per optimization round.
    round_budget: u64,
    /// The job's own budget: `--job-timeout` and the signal handler's stop
    /// flag. Polled in **both** budget modes — see [`StallGuard`].
    job_deadline: JobDeadline,
    /// Set when a guard fired, so callers can tell "converged" from "gave up".
    budget_exhausted: bool,

    // ---- per-trace working state (`pull_tight(Polyline,…)`, `:140-148`) ----
    curr_layer: i32,
    /// The half width every check shape is built with — the trace's **raw** half
    /// width, because the oracle adds the clearance itself
    /// (`overlapping_tree_entries_with_clearance`). Upstream's
    /// `curr_half_width = p_half_width + search_tree.clearance_compensation_value(...)`
    /// (`PullTightAlgo.java:143`) reads the **default** tree, whose compensation
    /// is 0 in every headless run (`SearchTreeManager.java:34`), so upstream's
    /// number is the raw half width too.
    curr_half_width: i32,
    /// Kept in sync with `curr_half_width`; the two fields exist because an
    /// earlier revision of this port checked against the compensated tree and
    /// had to tell the compensated and raw widths apart.
    curr_raw_half_width: i32,
    curr_net_no_arr: NetNos,
    curr_cl_type: i32,
    /// The pins at the trace's end corners. `None` is upstream's `null`, which
    /// switches the acid-trap pin rule off entirely (`:1183-1190`).
    contact_pins: Option<Vec<ItemId>>,
    /// Active `pin_edge_to_turn_dist` override. `None` falls back to `rules.get_pin_edge_to_turn_dist()`.
    pin_edge_to_turn_dist_override: Option<f64>,

    bounds: IntBox,
    binding: Option<BindingObstacle>,

    // ---- scratch, reused across every candidate (spec §11 "Arena the polylines") ----
    scratch_entries: Vec<TreeEntry>,
    scratch_stack: QueryStack,
    scratch_ids: Vec<ItemId>,
    scratch_lines: Vec<Line>,
    check_lines: [Line; 3],
}

impl<'b> PullTightAlgo<'b> {
    /// `PullTightAlgo.get_instance` (`:71-85`).
    ///
    /// `min_translate_dist` is floored at [`MIN_TRANSLATE_DIST_FLOOR`] exactly as
    /// upstream does (`:83`). `time_limit` of `None` means "no limit"; upstream
    /// spells that `p_time_limit <= 0` (`:56-60`) — the argument
    /// `RoutingBoard.java:834` actually passes, and what a caller that arms a
    /// [`StallGuard`] with [`PullTightAlgo::with_stall_guard`] passes too.
    ///
    /// A `PullTightAlgo` built and never given a guard falls back to
    /// [`DEFAULT_ROUND_BUDGET`], which is a *safety valve* and not a budget;
    /// every production driver arms one.
    // Upstream's `get_instance` takes the same eight; keeping them positional
    // keeps the call sites diffable against the Java.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        board: &'b mut Board,
        rules: &'b BoardRules,
        only_net_no_arr: &[i32],
        clip_shape: Option<IntOctagon>,
        min_translate_dist: i32,
        time_limit: Option<Duration>,
        keep_point: Option<Point>,
        keep_point_layer: i32,
    ) -> PullTightAlgo<'b> {
        let bounds = board_bounds(board);
        let mut nets = only_net_no_arr.to_vec();
        nets.sort_unstable();
        nets.dedup();
        let placeholder = Line::new_from_coords(0, 0, 1, 0);
        PullTightAlgo {
            board,
            rules,
            only_net_no_arr: nets,
            curr_clip_shape: clip_shape,
            min_translate_dist: f64::from(min_translate_dist.max(MIN_TRANSLATE_DIST_FLOOR)),
            keep_point,
            keep_point_layer,
            deadline: time_limit.map(|limit| Instant::now() + limit),
            round_budget: DEFAULT_ROUND_BUDGET,
            job_deadline: JobDeadline::ambient(),
            budget_exhausted: false,
            curr_layer: 0,
            curr_half_width: 0,
            curr_raw_half_width: 0,
            curr_net_no_arr: NetNos::default(),
            curr_cl_type: 0,
            contact_pins: None,
            pin_edge_to_turn_dist_override: None,
            bounds,
            binding: None,
            scratch_entries: Vec::new(),
            scratch_stack: QueryStack::new(),
            scratch_ids: Vec::new(),
            scratch_lines: Vec::new(),
            check_lines: [placeholder; 3],
        }
    }

    /// Arms the driver's stall guard — upstream's `p_time_limit`, in whichever
    /// currency the run's [`BudgetMode`] spends. See [`StallGuard`].
    ///
    /// A [`StallGuard::Rounds`] guard *replaces* [`DEFAULT_ROUND_BUDGET`]
    /// rather than adding to it: the fallback is the value for a caller that
    /// said nothing, and a caller that named a number means that number.
    #[must_use]
    pub fn with_stall_guard(mut self, guard: StallGuard) -> PullTightAlgo<'b> {
        match guard {
            StallGuard::Wall(limit) => {
                self.deadline = Instant::now().checked_add(limit);
            }
            StallGuard::Rounds(rounds) => self.round_budget = rounds,
        }
        self
    }

    /// Replaces the ambient job budget this was built with.
    ///
    /// The mirror of
    /// [`PassContext::with_deadline`](crate::batch::schedule::PassContext::with_deadline),
    /// and for the same reason: a test needs a budget it owns, so that proving
    /// the poll does not mean setting the process-wide stop flag every other
    /// test in the binary is also reading.
    #[must_use]
    pub fn with_job_deadline(mut self, deadline: JobDeadline) -> PullTightAlgo<'b> {
        self.job_deadline = deadline;
        self
    }

    /// `is_stop_requested` (`:150-170`) — upstream's time limit and its
    /// stoppable thread, plus the deterministic round budget.
    ///
    /// The three guards, in the order they are cheapest to answer:
    ///
    /// * the **round budget**, which is a plain integer and is what
    ///   [`BudgetMode::Deterministic`] spends;
    /// * the **job deadline**, which is `--job-timeout` and the `SIGTERM`
    ///   handler's flag — upstream's `stoppable_thread.is_stop_requested()`
    ///   (`:151`), the argument `BatchAutorouter` passes its `this.thread` as
    ///   (`:1332`). With no `--job-timeout` armed this is one relaxed atomic
    ///   load and no clock read at all, so a plain run pays nothing for it;
    /// * the **wall cutoff**, upstream's `TimeLimit.limit_exceeded()` (`:154`).
    ///
    /// The job deadline is polled in both budget modes on purpose: an operator
    /// stopping the process is not a budget decision, and a run that is killed
    /// mid-pull-tight loses the board it never wrote. `--deterministic` with a
    /// wall budget is already documented as a contradiction whose
    /// between-connections poll still stops it
    /// ([`crate::batch::schedule::PassContext::connection_budget`]); this is the
    /// same rule one level down.
    pub fn is_stop_requested(&self) -> bool {
        if self.round_budget == 0 {
            return true;
        }
        if self.job_deadline.expired() {
            return true;
        }
        self.deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
    }

    /// Consumes one round of the deterministic budget.
    ///
    /// Returns false once the budget is gone, which is what ends every
    /// optimization loop.
    pub(crate) fn take_round(&mut self) -> bool {
        match self.round_budget.checked_sub(1) {
            Some(remaining) => {
                self.round_budget = remaining;
                true
            }
            None => {
                self.budget_exhausted = true;
                false
            }
        }
    }

    /// True when a stall guard stopped the optimization before it converged.
    #[must_use]
    pub fn budget_exhausted(&self) -> bool {
        self.budget_exhausted
    }

    // ---- §5.2 the regional driver ----------------------------------------

    /// `opt_changed_area` (`:88-133`) — optimize every trace overlapping the
    /// per-layer changed region, until a whole round changes nothing.
    ///
    /// `changed_area` is consumed as upstream consumes `board.changed_area`:
    /// each layer's region is read, **cleared**, then enlarged by
    /// `1.5 * (max_clearance(layer) + 2 * max_trace_half_width)` (`:108-109`).
    /// Every accepted move re-joins its touched corners into it, so the loop
    /// chases ripples to a fixed point — or to the time limit.
    ///
    /// Returns true when anything changed.
    ///
    /// The via branch (`OptViaAlgo.opt_via_location`, `:128`) is not ported;
    /// vias are left where the inserter put them.
    pub fn opt_changed_area(&mut self, changed_area: &mut ChangedArea) -> bool {
        let mut anything_changed = false;
        let mut something_changed = true;
        while something_changed {
            // The regional round itself has to be polled, not just the traces
            // inside it: a round that finds no candidate on any layer still
            // pays the per-layer region query, and the §6.4 branch's `break`
            // makes one improvement cost a whole extra round of them.
            if self.is_stop_requested() {
                return anything_changed;
            }
            something_changed = false;
            for layer in 0..changed_area.layer_count() as i32 {
                let region = changed_area.area(layer);
                if region.is_empty() {
                    continue;
                }
                changed_area.set_empty(layer);
                let offset = 1.5
                    * f64::from(
                        self.max_clearance_on_layer(layer) + 2 * self.board.max_trace_half_width(),
                    );
                let region = region.offset(offset);

                // The explicit worklist: snapshot the ids, then revalidate each
                // one before use. Upstream iterates a live collection and breaks
                // out of it whenever an item may have been removed (`:121-127`).
                // `change` itself is in place and id-preserving; what still
                // invalidates a snapshot is the normalization after it (split,
                // combine, drop) and the §6.4 branch's re-insert.
                let mut candidates = Vec::new();
                self.board.overlapping_items(
                    &TileShape::Simplex(region.to_simplex()),
                    Some(layer),
                    &[],
                    &mut candidates,
                );
                candidates.retain(|id| {
                    self.board
                        .store()
                        .get(*id)
                        .is_some_and(|item| matches!(item.kind, ItemKind::Trace(_)))
                });
                candidates.sort_unstable();

                for id in candidates {
                    if self.is_stop_requested() {
                        return anything_changed;
                    }
                    // Liveness: a previous move's normalization may have
                    // removed or split this trace.
                    if !self
                        .board
                        .store()
                        .get(id)
                        .is_some_and(|item| matches!(item.kind, ItemKind::Trace(_)))
                    {
                        continue;
                    }
                    if self.trace_pull_tight(id, changed_area).is_some() {
                        something_changed = true;
                        anything_changed = true;
                        if self.split_traces_at_keep_point() {
                            break;
                        }
                    } else if self.smoothen_end_corners_at_trace(id, changed_area) {
                        // `:123-126` — §6.4. The `break` is upstream's, and it
                        // is not cosmetic: this branch removes and re-inserts
                        // items, so the rest of the snapshot may name ids that
                        // no longer exist.
                        something_changed = true;
                        anything_changed = true;
                        break;
                    }
                }
            }
        }
        anything_changed
    }

    /// `rules.clearance_matrix.max_value(layer)` (`:108`).
    fn max_clearance_on_layer(&self, layer: i32) -> i32 {
        let clearances = self.board.clearances();
        let class_count = clearances.class_count() as i32;
        let mut max = 0;
        for i in 0..class_count {
            for j in i..class_count {
                max = max.max(clearances.value(i, j, layer));
            }
        }
        max
    }

    // ---- §5.4 the per-trace entry point ----------------------------------

    /// `PolylineTrace.pull_tight` (`:827-875`) — the guards, the optimize call,
    /// and `change(new_lines)` on success.
    ///
    /// Returns the trace's id after the change — **the same id**, because
    /// `change` is in place — or `None` when nothing was changed. The trace may
    /// be **gone** on success: normalization deletes a piece that collapsed, and
    /// splits it into new ids when it did not. So the returned id still has to
    /// be looked up before use, it just never *silently* denotes a different
    /// object than it did before the call.
    ///
    /// The `swap_connection_to_pin` / `correct_connection_to_pin` ladder
    /// (`:855-873`) is gated on `rules.pin_edge_to_turn_dist() > 0` upstream and
    /// is not ported; the gate itself is honoured by
    /// [`crate::insert`], which forces the knob to `-1` for the duration of an
    /// insertion so this path stays off exactly when upstream turns it off.
    pub fn trace_pull_tight(
        &mut self,
        id: ItemId,
        changed_area: &mut ChangedArea,
    ) -> Option<ItemId> {
        let prepared = self.prepare_trace(id)?;
        let TracePrep {
            layer, polyline, ..
        } = prepared;

        if let Some(new_polyline) = self.pull_tight_flavor(&polyline, changed_area) {
            // `change(new_polyline)` (`PolylineTrace.java:942-1009`): swap the
            // geometry in place — reusing the tree leaves of every segment the move
            // did not touch — then normalize against the changed-area clip.
            //
            // The optimizer's whole job is small local edits to long polylines, so
            // this is the single call the family's cost is decided by; see
            // [`Board::change_trace_polyline`] for what it reuses and why.
            if !self.board.change_trace_polyline(id, new_polyline) {
                return None;
            }
            // `change` ends in `this.normalize(clip_shape)` and nothing else
            // (`PolylineTrace.java:1004`); `normalize` itself splits and then
            // combines each piece, so an extra `combine_trace` here would be a
            // second, unsynchronised merge pass. It is per *trace*, not per net —
            // the whole-net `normalize_traces` upstream reaches for lives in
            // fork smoothing alone (`PullTightAlgo.java:391`), where an end corner
            // really has moved onto another trace.
            let clip = changed_area.area(layer);
            self.board.normalize_trace(id, Some(&clip));
            return Some(id);
        }

        // Tail ladder (`PolylineTrace.java:855-873`):
        if self.board.angle_restriction() != AngleRestriction::Ninety
            && self.pin_edge_to_turn_dist() > 0.0
        {
            if self.swap_connection_to_pin(id, true, changed_area) {
                self.trace_pull_tight(id, changed_area);
                return Some(id);
            }
            if self.swap_connection_to_pin(id, false, changed_area) {
                self.trace_pull_tight(id, changed_area);
                return Some(id);
            }
            if self.correct_connection_to_pin(id, true, changed_area) {
                self.trace_pull_tight(id, changed_area);
                return Some(id);
            }
            if self.correct_connection_to_pin(id, false, changed_area) {
                self.trace_pull_tight(id, changed_area);
                return Some(id);
            }
        }

        None
    }

    /// Checks that connection restrictions to contact pins are satisfied.
    /// If `at_start`, the start of this trace is checked, else the end.
    /// Upstream `PolylineTrace.check_connection_to_pin` (`:995-1060`).
    pub fn check_connection_to_pin(&self, trace_id: ItemId, at_start: bool) -> bool {
        let Some(item) = self.board.store().get(trace_id) else {
            return true;
        };
        let ItemKind::Trace(trace) = &item.kind else {
            return true;
        };
        if trace.polyline.corner_count() < 2 {
            return true;
        }

        let end_corner = if at_start {
            trace.polyline.first_corner()
        } else {
            trace.polyline.last_corner()
        };
        let prev_end_corner = if at_start {
            trace.polyline.corner(1)
        } else {
            trace.polyline.corner(trace.polyline.corner_count() - 2)
        };

        let mut contacts = Vec::new();
        self.board.normal_contacts_at(trace_id, &end_corner, &mut contacts);
        let mut contact_pin = None;
        for &contact_id in &contacts {
            if let Some(c_item) = self.board.store().get(contact_id) {
                if let ItemKind::Pin(pin) = &c_item.kind {
                    contact_pin = Some((c_item.clearance_class, pin));
                    break;
                }
            }
        }
        let Some((pin_clearance_class, pin)) = contact_pin else {
            return true;
        };

        let restrictions = pin.exit_restrictions(self.board, trace.layer);
        if restrictions.is_empty() {
            return true;
        }

        if end_corner == prev_end_corner {
            return true;
        }
        let trace_end_direction = Direction::get_instance(&end_corner, &prev_end_corner);

        let Some(matching) = restrictions.iter().find(|r| r.direction == trace_end_direction) else {
            return false;
        };

        let edge_to_turn_dist = self.pin_edge_to_turn_dist();
        if edge_to_turn_dist < 0.0 {
            return false;
        }

        let end_line_length = end_corner.to_float().distance(&prev_end_corner.to_float());
        let curr_clearance = self.board.clearances().value(
            item.clearance_class,
            pin_clearance_class,
            trace.layer,
        );
        let add_width = edge_to_turn_dist.max(f64::from(curr_clearance) + 1.0);
        let preserve_length = matching.min_length + f64::from(trace.half_width) + add_width;
        !(preserve_length > end_line_length)
    }

    /// Checks clearance for all segment shapes of `polyline`.
    pub fn check_polyline_trace(&mut self, polyline: &Polyline) -> bool {
        if polyline.arr.len() < 3 {
            return true;
        }
        for i in 0..polyline.arr.len() - 2 {
            let Some(shape) = polyline.offset_shape(self.curr_half_width, i) else {
                return false;
            };
            if !self.check_trace_shape(&shape) {
                return false;
            }
        }
        true
    }

    /// Tries to correct a connection restriction of this trace.
    /// If `at_start`, the start of the trace is corrected, else the end.
    /// Returns true if the trace was changed.
    /// Upstream `PolylineTrace.correct_connection_to_pin` (`:1064-1235`).
    pub fn correct_connection_to_pin(
        &mut self,
        id: ItemId,
        at_start: bool,
        changed_area: &mut ChangedArea,
    ) -> bool {
        if self.check_connection_to_pin(id, at_start) {
            return false;
        }

        let Some(item) = self.board.store().get(id) else {
            return false;
        };
        let ItemKind::Trace(trace) = &item.kind else {
            return false;
        };
        let layer = trace.layer;
        let half_width = trace.half_width;
        let nets = item.nets.clone();
        let clearance_class = item.clearance_class;

        let trace_polyline = if at_start {
            trace.polyline.clone()
        } else {
            trace.polyline.reverse()
        };

        let end_corner = trace_polyline.first_corner();
        let mut contacts = Vec::new();
        self.board.normal_contacts_at(id, &end_corner, &mut contacts);
        let mut contact_pin = None;
        for &contact_id in &contacts {
            if let Some(c_item) = self.board.store().get(contact_id) {
                if let ItemKind::Pin(pin) = &c_item.kind {
                    contact_pin = Some((c_item.clearance_class, pin.clone()));
                    break;
                }
            }
        }
        let Some((pin_clearance_class, pin)) = contact_pin else {
            return false;
        };

        let restrictions = pin.exit_restrictions(self.board, layer);
        if restrictions.is_empty() {
            return false;
        }

        let padstack = self.board.padstacks().get(pin.padstack_no);
        let Some(pin_shape) = padstack.and_then(|p| p.shape_on_layer(layer)) else {
            return false;
        };
        let pin_center = pin.center;

        let edge_to_turn_dist = self.pin_edge_to_turn_dist();
        if edge_to_turn_dist < 0.0 {
            return false;
        }
        let curr_clearance = self.board.clearances().value(clearance_class, pin_clearance_class, layer);
        let add_width = edge_to_turn_dist.max(f64::from(curr_clearance) + 1.0);

        let offset_vec = fr_geometry::planar::Vector::Int(pin_center.difference_by_int(&IntPoint::ZERO));
        let absolute_pin_shape = pin_shape.translate_by(&offset_vec);
        let mut offset_pin_shape = absolute_pin_shape.offset(f64::from(half_width) + add_width);
        if self.board.angle_restriction() == AngleRestriction::Ninety || offset_pin_shape.is_int_box() {
            offset_pin_shape = TileShape::Box(offset_pin_shape.bounding_box());
        } else if self.board.angle_restriction() == AngleRestriction::FortyFive {
            offset_pin_shape = TileShape::Oct(offset_pin_shape.bounding_octagon());
        }

        let entries = offset_pin_shape.entrance_points(&trace_polyline);
        if entries.is_empty() {
            return false;
        }
        let latest_entry_tuple = *entries.last().unwrap();
        let border_line_at_entry = offset_pin_shape.border_line(latest_entry_tuple.1);
        let trace_entry_location_approx = trace_polyline.arr[latest_entry_tuple.0].intersection_approx(&border_line_at_entry);

        let mut min_exit_corner_distance = f64::MAX;
        let mut nearest_pin_exit_ray = None;
        let mut nearest_border_line_no = None;
        let mut pin_exit_direction = None;
        let mut nearest_exit_corner = None;
        const TOLERANCE: f64 = 1.0;

        for r in restrictions {
            let Some(curr_border_no) = offset_pin_shape.intersecting_border_line_no(&pin_center, &r.direction) else {
                continue;
            };
            let curr_ray = Line::new_from_point_and_dir(pin_center, &r.direction);
            let curr_corner = curr_ray.intersection_approx(&offset_pin_shape.border_line(curr_border_no));
            let curr_dist = curr_corner.distance_square(&trace_entry_location_approx);
            let mut new_nearest = false;
            if curr_dist + TOLERANCE < min_exit_corner_distance {
                new_nearest = true;
            } else if curr_dist < min_exit_corner_distance + TOLERANCE {
                let old_corner = nearest_exit_corner.unwrap_or(curr_corner);
                for i in 1..trace_polyline.corner_count() {
                    let tc = trace_polyline.corner_approx(i);
                    let d_new = tc.distance_square(&curr_corner);
                    let d_old = tc.distance_square(&old_corner);
                    if d_new + TOLERANCE < d_old {
                        new_nearest = true;
                        break;
                    } else if d_new > d_old + TOLERANCE {
                        break;
                    }
                }
            }
            if new_nearest {
                min_exit_corner_distance = curr_dist;
                nearest_pin_exit_ray = Some(curr_ray);
                nearest_border_line_no = Some(curr_border_no);
                pin_exit_direction = Some(r.direction);
                nearest_exit_corner = Some(curr_corner);
            }
        }

        let (Some(nearest_pin_exit_ray), Some(nearest_border_line_no), Some(pin_exit_direction)) =
            (nearest_pin_exit_ray, nearest_border_line_no, pin_exit_direction)
        else {
            return false;
        };

        // Append the polygon piece around the border of the pin shape
        let corner_count = offset_pin_shape.border_line_count();
        let clock_wise_side_diff = (nearest_border_line_no as isize - latest_entry_tuple.1 as isize + corner_count as isize) as usize % corner_count;
        let counter_clock_wise_side_diff = (latest_entry_tuple.1 as isize - nearest_border_line_no as isize + corner_count as isize) as usize % corner_count;

        let mut curr_lines = Vec::new();
        curr_lines.push(nearest_pin_exit_ray);
        let mut curr_border_line_no = nearest_border_line_no;
        if counter_clock_wise_side_diff <= clock_wise_side_diff {
            for _ in 0..=counter_clock_wise_side_diff {
                curr_lines.push(offset_pin_shape.border_line(curr_border_line_no));
                curr_border_line_no = (curr_border_line_no + 1) % corner_count;
            }
        } else {
            for _ in 0..=clock_wise_side_diff {
                curr_lines.push(offset_pin_shape.border_line(curr_border_line_no));
                curr_border_line_no = (curr_border_line_no + corner_count - 1) % corner_count;
            }
        }
        curr_lines.push(trace_polyline.arr[latest_entry_tuple.0]);

        let border_polyline = Polyline::new_from_lines(&curr_lines);
        if !self.check_polyline_trace(&border_polyline) {
            return false;
        }

        let mut cut_lines = Vec::with_capacity(trace_polyline.arr.len() - latest_entry_tuple.0 + 1);
        cut_lines.push(curr_lines[curr_lines.len() - 2]);
        cut_lines.extend_from_slice(&trace_polyline.arr[latest_entry_tuple.0..]);
        let cut_polyline = Polyline::new_from_lines(&cut_lines);
        let mut changed_polyline = if cut_polyline.first_corner() == cut_polyline.last_corner() {
            border_polyline
        } else {
            border_polyline.combine(&cut_polyline)
        };
        if !at_start {
            changed_polyline = changed_polyline.reverse();
        }

        if !self.board.change_trace_polyline(id, changed_polyline.clone()) {
            return false;
        }
        for corner in changed_polyline.corner_approx_arr() {
            changed_area.join_point(corner, layer);
        }
        let clip = changed_area.area(layer);
        self.board.normalize_trace(id, Some(&clip));

        // Create a shove_fixed exit line segment
        let exit_lines = [
            Line::new_from_point_and_dir(pin_center, &pin_exit_direction.turn_45_degree(2)),
            nearest_pin_exit_ray,
            offset_pin_shape.border_line(nearest_border_line_no),
        ];
        let exit_line_segment = Polyline::new_from_lines(&exit_lines);
        for corner in exit_line_segment.corner_approx_arr() {
            changed_area.join_point(corner, layer);
        }
        self.board.insert_trace(
            exit_line_segment,
            layer,
            half_width,
            nets,
            clearance_class,
            FixedState::ShoveFixed,
            Some(&clip),
        );

        true
    }

    /// Looks if another pin connection restriction fits better than the current one
    /// and changes this trace in that case.
    /// Upstream `PolylineTrace.swap_connection_to_pin` (`:1237-1298`).
    pub fn swap_connection_to_pin(
        &mut self,
        id: ItemId,
        at_start: bool,
        _changed_area: &mut ChangedArea,
    ) -> bool {
        let Some(item) = self.board.store().get(id) else {
            return false;
        };
        let ItemKind::Trace(trace) = &item.kind else {
            return false;
        };
        let half_width = trace.half_width;
        let layer = trace.layer;

        let trace_polyline = if at_start {
            trace.polyline.clone()
        } else {
            trace.polyline.reverse()
        };

        let end_corner = trace_polyline.first_corner();
        let mut contacts = Vec::new();
        self.board.normal_contacts_at(id, &end_corner, &mut contacts);
        if contacts.len() != 1 {
            return false;
        }
        let contact_id = contacts[0];
        let Some(contact_item) = self.board.store().get(contact_id) else {
            return false;
        };
        if contact_item.fixed_state != FixedState::ShoveFixed {
            return false;
        }
        let ItemKind::Trace(contact_trace) = &contact_item.kind else {
            return false;
        };
        let contact_polyline = contact_trace.polyline.clone();
        if contact_polyline.arr.len() < 3 || trace_polyline.arr.len() < 3 {
            return false;
        }

        let contact_last_line = contact_polyline.arr[contact_polyline.arr.len() - 2];
        let first_line = trace_polyline.arr[1];

        let mut check_swap = contact_last_line.direction().projection(&first_line.direction()) == Signum::Negative;
        if !check_swap {
            let hw = f64::from(half_width);
            if trace_polyline.arr.len() > 3
                && trace_polyline.corner_approx(0).distance_square(&trace_polyline.corner_approx(1)) <= hw * hw
            {
                check_swap = contact_last_line.direction().projection(&trace_polyline.arr[2].direction()) == Signum::Negative;
            }
        }
        if !check_swap {
            return false;
        }

        // Find pin at start of contact_trace
        let contact_start = contact_polyline.first_corner();
        let mut contact_pins = Vec::new();
        self.board.normal_contacts_at(contact_id, &contact_start, &mut contact_pins);
        let mut found_pin = None;
        for &cid in &contact_pins {
            if let Some(c_item) = self.board.store().get(cid) {
                if let ItemKind::Pin(pin) = &c_item.kind {
                    found_pin = Some(pin.clone());
                    break;
                }
            }
        }
        let Some(contact_pin) = found_pin else {
            return false;
        };

        let combined_polyline = contact_polyline.combine(&trace_polyline);
        let nearest_dir = contact_pin.calc_nearest_exit_restriction_direction(
            self.board,
            &combined_polyline,
            half_width,
            layer,
            self.pin_edge_to_turn_dist(),
        );

        if nearest_dir.is_none() || nearest_dir == Some(contact_polyline.arr[1].direction()) {
            return false;
        }

        // Unfix contact trace and combine
        self.board.set_item_fixed_state(contact_id, item.fixed_state);
        self.board.combine_trace(id);
        true
    }

    /// Overrides `pin_edge_to_turn_dist` for this pull-tight run (e.g. -1.0 during trace insertion).
    pub fn with_pin_edge_to_turn_dist(mut self, dist: f64) -> Self {
        self.pin_edge_to_turn_dist_override = Some(dist);
        self
    }

    /// Returns the active `pin_edge_to_turn_dist`.
    pub fn pin_edge_to_turn_dist(&self) -> f64 {
        self.pin_edge_to_turn_dist_override
            .unwrap_or_else(|| self.rules.get_pin_edge_to_turn_dist())
    }

    /// `PullTightAlgo.get_instance`'s flavor dispatch (`:71-85`).
    ///
    /// | board angle restriction | upstream class | here |
    /// |---|---|---|
    /// | `NINETY_DEGREE` | `PullTightAlgo90` | [`crate::pull_tight90`] |
    /// | `FORTYFIVE_DEGREE` | `PullTightAlgo45` | [`crate::pull_tight45`] |
    /// | `NONE` | `PullTightAlgoAnyAngle` | [`crate::pull_tight_any`] |
    ///
    /// Upstream chooses the subclass once, in the constructor, from
    /// `p_board.rules.get_trace_angle_restriction()`; the same value is read
    /// here per trace because the flavor is a pure function of it and this port
    /// has one struct rather than three.
    pub(crate) fn pull_tight_flavor(
        &mut self,
        polyline: &Polyline,
        changed_area: &mut ChangedArea,
    ) -> Option<Polyline> {
        match self.board.angle_restriction() {
            AngleRestriction::None => self.pull_tight_polyline_any_angle(polyline, changed_area),
            AngleRestriction::Ninety => self.pull_tight_polyline_90(polyline, changed_area),
            AngleRestriction::FortyFive => self.pull_tight_polyline(polyline, changed_area),
        }
    }

    /// Runs `PolylineTrace.pull_tight`'s guards (`:832-851`) and installs the
    /// per-trace working state of `pull_tight(Polyline, …)` (`:140-148`).
    ///
    /// `None` means a guard refused the trace, which is upstream's "return
    /// false without touching anything".
    fn prepare_trace(&mut self, id: ItemId) -> Option<TracePrep> {
        let item = self.board.store().get(id)?;
        let ItemKind::Trace(trace) = &item.kind else {
            return None;
        };
        // `:832-843` — on board, not shove fixed, normal nets, net filter.
        if item.is_shove_fixed() || !item.nets_normal() {
            return None;
        }
        let nets = item.nets.clone();
        if !self.only_net_no_arr.is_empty() && nets.as_slice() != self.only_net_no_arr.as_slice() {
            return None;
        }
        // `:844-851` — the per-net-class `get_pull_tight()` disable knob.
        if let Some(&net_no) = nets.as_slice().first() {
            if !self.net_class_pull_tight(net_no) {
                return None;
            }
        }

        let prep = TracePrep {
            layer: trace.layer,
            half_width: trace.half_width,
            polyline: trace.polyline.clone(),
            clearance_class: item.clearance_class,
            nets,
        };

        // `:140-148` — the working state. The half width stays **raw**: the
        // oracle is the default tree's symmetric `with_clearance` check, which
        // adds the clearance itself (see the module docs, "The clearance
        // oracle is the DRC's own arithmetic").
        self.curr_layer = prep.layer;
        self.curr_raw_half_width = prep.half_width;
        self.curr_half_width = prep.half_width;
        self.curr_net_no_arr = prep.nets.clone();
        self.curr_cl_type = prep.clearance_class;
        self.contact_pins = Some(self.touching_pins_at_end_corners(
            &prep.polyline,
            prep.layer,
            prep.half_width,
            &prep.nets,
            prep.clearance_class,
        ));
        self.binding = None;
        Some(prep)
    }

    /// How many of the trace's own segments currently fail the oracle.
    ///
    /// `None` when `id` is not a trace this optimizer would touch. Installs the
    /// same working state [`PullTightAlgo::trace_pull_tight`] would, so the
    /// answer is the one pull-tight itself would compute.
    pub fn trace_polyline_violations(&mut self, id: ItemId) -> Option<usize> {
        let prep = self.prepare_trace(id)?;
        Some(self.polyline_violation_count(&prep.polyline))
    }

    /// The net class' `get_pull_tight()` flag (`PolylineTrace.java:845-850`).
    fn net_class_pull_tight(&self, net_no: i32) -> bool {
        self.rules
            .nets
            .get(net_no)
            .map(|net| net.net_class_index())
            .and_then(|index| self.rules.net_classes.get(index))
            .is_none_or(|class| class.pull_tight)
    }

    /// `Trace.touching_pins_at_end_corners` (`Trace.java:386-404`): pins of the
    /// own net overlapping the octagon around either end corner, enlarged by the
    /// half width.
    ///
    /// These are the pins pull-tight is allowed to touch. **Every other pin is
    /// an obstacle, even one of the own net** — that is the acid-trap rule, and
    /// it is the whole reason this set has to be computed
    /// (`BasicBoard.java:1183-1190`).
    fn touching_pins_at_end_corners(
        &mut self,
        polyline: &Polyline,
        layer: i32,
        half_width: i32,
        nets: &NetNos,
        clearance_class: i32,
    ) -> Vec<ItemId> {
        let mut result = Vec::new();
        // Upstream queries `overlapping_items_with_clearance` on the default
        // tree (`Trace.java:396-400`): the end-corner octagon, enlarged by the
        // raw half width, is then enlarged again by half the pairwise clearance
        // plus the safety margin — and so is the candidate pin's shape.
        let reach = half_width + self.max_clearance_on(clearance_class, layer);
        for corner in [polyline.first_corner(), polyline.last_corner()] {
            let octagon = corner.surrounding_octagon().offset(f64::from(half_width));
            let query = TileShape::Oct(octagon);
            // The broad phase must cover everything the exact test can still
            // reach: the query grows by another half clearance plus margin.
            let broad = TileShape::Oct(
                corner
                    .surrounding_octagon()
                    .offset(f64::from(reach + CLEARANCE_SAFETY_MARGIN)),
            );
            let PullTightAlgo {
                board,
                scratch_ids,
                scratch_stack,
                ..
            } = self;
            fr_metrics::counters::bump::tree_queries(1);
            board.default_tree().overlapping_items(
                board.store(),
                &broad,
                Some(layer),
                &[],
                scratch_stack,
                scratch_ids,
            );
            for &id in scratch_ids.iter() {
                let Some(item) = board.store().get(id) else {
                    continue;
                };
                if !matches!(item.kind, ItemKind::Pin(_)) || !item.nets.shares_any(nets) {
                    continue;
                }
                let half = board.clearances().value_with_margin(
                    clearance_class,
                    item.clearance_class,
                    layer,
                ) / 2;
                let Some(slot) = board.store().slot_of(id) else {
                    continue;
                };
                let touching = board
                    .default_tree()
                    .item_shapes(slot, id)
                    .iter()
                    .filter(|tile| tile.layer == layer)
                    .filter_map(|tile| tile.shape.as_ref())
                    .any(|tile| query.enlarge(half).intersects(&tile.enlarge(half)));
                if touching {
                    result.push(id);
                }
            }
        }
        result.sort_unstable();
        result.dedup();
        result
    }

    /// `split_traces_at_keep_point` (`:411-427`): the keep point must stay a
    /// corner of the net, so every trace running through it is split there.
    ///
    /// Returns true when something was split — upstream's signal to restart the
    /// driver's region scan (`:124`).
    pub fn split_traces_at_keep_point(&mut self) -> bool {
        let Some(keep_point) = self.keep_point.clone() else {
            return false;
        };
        let layer = self.keep_point_layer;
        let mut picked = Vec::new();
        self.board.pick_items(&keep_point, Some(layer), &mut picked);
        picked.retain(|id| {
            self.board
                .store()
                .get(*id)
                .is_some_and(|item| matches!(item.kind, ItemKind::Trace(_)))
        });
        picked.sort_unstable();
        for id in picked {
            if let Some(pieces) = self.board.split_trace_at_point(id, &keep_point) {
                if pieces.iter().filter(|piece| piece.is_some()).count() > 1 {
                    return true;
                }
            }
        }
        false
    }

    // ---- §6.4 fork/end smoothing -----------------------------------------

    /// `smoothen_end_corners_at_trace` (`:347-356`) — the net-filtered wrapper
    /// that installs the per-trace working state, then `_1` (`:361-405`), the
    /// remove/re-insert/split/normalize improvement loop.
    ///
    /// This is the branch `opt_changed_area` takes when `pull_tight` could not
    /// improve a trace: it slides the trace's *end corner* along the trace it
    /// forks off, chamfering the acute joint upstream's `insert` leaves behind.
    ///
    /// Three details of the wrapper are load-bearing and easy to lose:
    ///
    /// * the net filter is `nets_equal(only_net_no_arr)` — the same equality the
    ///   per-trace guard uses, not a subset test (`:348`);
    /// * `curr_half_width` here is the trace's **raw** half width (`:352`) —
    ///   the same value [`PullTightAlgo::prepare_trace`] installs, because the
    ///   oracle adds the clearance at query time in both. It is also what the
    ///   new trace is re-inserted with, so compensating it would fatten every
    ///   smoothed trace;
    /// * `contact_pins` is forced to `None` for the whole loop (`:369-371`) so
    ///   the trace may slide onto a contact trace that ends at a pin, and
    ///   restored afterwards. `_2` sets it back to the *new* trace's touching
    ///   pins before `skip_segments_of_length_0` runs (`:441`).
    ///
    /// Returns true when the board changed.
    pub fn smoothen_end_corners_at_trace(
        &mut self,
        id: ItemId,
        changed_area: &mut ChangedArea,
    ) -> bool {
        let Some(item) = self.board.store().get(id) else {
            return false;
        };
        let ItemKind::Trace(trace) = &item.kind else {
            return false;
        };
        // `:348` — the wrapper's own net filter.
        if !self.only_net_no_arr.is_empty()
            && item.nets.as_slice() != self.only_net_no_arr.as_slice()
        {
            return false;
        }
        // `:362-364` — `_1`'s first guard.
        if item.is_shove_fixed() {
            return false;
        }
        let layer = trace.layer;
        let raw_half_width = trace.half_width;
        self.curr_layer = layer;
        self.curr_raw_half_width = raw_half_width;
        self.curr_net_no_arr = item.nets.clone();
        self.curr_cl_type = item.clearance_class;
        // `:352` — the raw half width, exactly as upstream installs it here;
        // the oracle adds the clearance at query time.
        self.curr_half_width = raw_half_width;
        self.binding = None;

        let saved_contact_pins = self.contact_pins.take();
        let result = self.smoothen_end_corners_at_trace_1(id, changed_area);
        self.contact_pins = saved_contact_pins;
        result
    }

    /// `smoothen_end_corners_at_trace_1` (`:361-405`).
    fn smoothen_end_corners_at_trace_1(
        &mut self,
        id: ItemId,
        changed_area: &mut ChangedArea,
    ) -> bool {
        let mut result = false;
        let mut curr = id;
        while !self.is_stop_requested() && self.take_round() {
            let Some(adjusted) = self.smoothen_end_corners_at_trace_2(curr, changed_area) else {
                break;
            };
            let Some(item) = self.board.store().get(curr) else {
                break;
            };
            let ItemKind::Trace(trace) = &item.kind else {
                break;
            };
            let layer = trace.layer;
            let clearance_class = item.clearance_class;
            let fixed_state = item.fixed_state;
            let nets = item.nets.clone();
            let half_width = self.curr_raw_half_width;

            // `:376-381` — remove, then re-insert the adjusted polyline. The
            // release removes `curr_trace` a second time at `:383`; that call
            // is a no-op and is not reproduced.
            self.board.remove_item(curr);
            let Some(inserted) = self.board.insert_trace_without_cleaning(
                adjusted.clone(),
                layer,
                half_width,
                nets.clone(),
                clearance_class,
                fixed_state,
            ) else {
                break;
            };
            result = true;
            curr = inserted;

            // `:385-399` — split at both new end corners and normalize, per net.
            let mut keep_point_split = false;
            for net_no in nets.as_slice().iter().copied() {
                self.board
                    .split_traces(&adjusted.first_corner(), layer, net_no);
                self.board
                    .split_traces(&adjusted.last_corner(), layer, net_no);
                self.board.normalize_traces(net_no);
                if self.split_traces_at_keep_point() {
                    keep_point_split = true;
                    break;
                }
            }
            if keep_point_split {
                // `:397` returns straight out of `_1`.
                return true;
            }
        }
        result
    }

    /// `smoothen_end_corners_at_trace_2` (`:429-450`) — try the start corner,
    /// then the end corner, join the moved corner into the changed area, and
    /// clean the result up.
    fn smoothen_end_corners_at_trace_2(
        &mut self,
        id: ItemId,
        changed_area: &mut ChangedArea,
    ) -> Option<Polyline> {
        // `:430-432` — the trace may have been removed by the previous round's
        // normalization.
        if !self
            .board
            .store()
            .get(id)
            .is_some_and(|item| matches!(item.kind, ItemKind::Trace(_)))
        {
            return None;
        }
        let layer = self.curr_layer;
        let mut result = self.smoothen_start_corner_at_trace(id, changed_area);
        if let Some(polyline) = result.as_ref() {
            changed_area.join_point(polyline.corner_approx(0), layer);
        } else {
            result = self.smoothen_end_corner_at_trace(id, changed_area);
            if let Some(polyline) = result.as_ref() {
                changed_area.join_point(polyline.corner_approx(polyline.corner_count() - 1), layer);
            }
        }
        let polyline = result?;
        // `:441` — the pin set is refreshed from the *adjusted* geometry before
        // the length-0 cleanup, because that cleanup checks clearance.
        let contact_pins = self.touching_pins_at_end_corners(
            &polyline,
            layer,
            self.curr_raw_half_width,
            &self.curr_net_no_arr.clone(),
            self.curr_cl_type,
        );
        self.contact_pins = Some(contact_pins);
        let cleaned = self
            .skip_segments_of_length_0(&polyline)
            .unwrap_or(polyline);
        if cleaned.arr.len() < 3 {
            return None;
        }
        Some(cleaned)
    }

    /// The angle-mode dispatch of `smoothen_start_corner_at_trace` (`:471`).
    /// `PullTightAlgo90`'s hook returns `null` (`PullTightAlgo90.java:135-138`)
    /// — no fork chamfers on a 90° board.
    fn smoothen_start_corner_at_trace(
        &mut self,
        id: ItemId,
        changed_area: &mut ChangedArea,
    ) -> Option<Polyline> {
        match self.board.angle_restriction() {
            AngleRestriction::None => self.smoothen_start_corner_at_trace_any(id, changed_area),
            AngleRestriction::Ninety => None,
            AngleRestriction::FortyFive => self.smoothen_start_corner_at_trace_45(id, changed_area),
        }
    }

    /// The angle-mode dispatch of `smoothen_end_corner_at_trace` (`:473`).
    /// `PullTightAlgo90`'s hook returns `null` (`PullTightAlgo90.java:140-143`).
    fn smoothen_end_corner_at_trace(
        &mut self,
        id: ItemId,
        changed_area: &mut ChangedArea,
    ) -> Option<Polyline> {
        match self.board.angle_restriction() {
            AngleRestriction::None => self.smoothen_end_corner_at_trace_any(id, changed_area),
            AngleRestriction::Ninety => None,
            AngleRestriction::FortyFive => self.smoothen_end_corner_at_trace_45(id, changed_area),
        }
    }

    /// The contact traces at one end corner, read the way both fork-smoothing
    /// rules read them (`PullTightAlgo45.java:433-450`, the any-angle copy at
    /// `PullTightAlgoAnyAngle.java:744-758`).
    ///
    /// `None` is upstream's `return null` from the *whole* rule: a contact that
    /// is not a `PolylineTrace`, or is shove-fixed, aborts the smoothing. An
    /// empty vector means "no contacts at all", which upstream lets fall
    /// through to the `acute_angle == bend == false` return.
    ///
    /// **The order is descending by id, and it decides the result.**
    /// `get_start_contacts` / `get_end_contacts` return a `TreeSet<Item>`
    /// (`Trace.java:101-116`, `:172-199`) and `Item.compareTo` is
    /// `other.id_no - this.id_no` (`Item.java:94-99`), so an item sorts *before*
    /// another exactly when its own id is larger — the set iterates **highest id
    /// first**. [`PullTightAlgo::scan_fork_contacts`] keeps the *last* contact
    /// that fires, so at a fork where two contacts both fire, ascending order
    /// picks the opposite one from upstream and the chamfer is built against the
    /// wrong branch. Same tie-break, same reason, as
    /// [`PullTightAlgo::picked_trace_at`].
    pub(crate) fn contact_trace_lines_at(
        &mut self,
        id: ItemId,
        end_corner: &Point,
    ) -> Option<Vec<ContactTraceLines>> {
        let mut contacts = Vec::new();
        self.board.normal_contacts_at(id, end_corner, &mut contacts);
        contacts.sort_unstable_by(|a, b| b.cmp(a));
        let mut result = Vec::with_capacity(contacts.len());
        for contact in contacts {
            let item = self.board.store().get(contact)?;
            let ItemKind::Trace(trace) = &item.kind else {
                return None;
            };
            if item.is_shove_fixed() {
                return None;
            }
            let polyline = &trace.polyline;
            if polyline.arr.len() < 3 {
                return None;
            }
            let lines = if polyline.first_corner() == *end_corner {
                ContactTraceLines {
                    corner_approx: polyline.corner_approx(1),
                    line: polyline.arr[1],
                    prev_line: polyline.arr[2],
                    corner_count: polyline.corner_count(),
                }
            } else {
                let corner_no = polyline.corner_count() - 2;
                ContactTraceLines {
                    corner_approx: polyline.corner_approx(corner_no),
                    line: polyline.arr[corner_no + 1].opposite(),
                    prev_line: polyline.arr[corner_no],
                    corner_count: polyline.corner_count(),
                }
            };
            result.push(lines);
        }
        Some(result)
    }

    /// The polyline of `id`, when `id` is still a trace on the board.
    pub(crate) fn trace_polyline_of(&self, id: ItemId) -> Option<Polyline> {
        match &self.board.store().get(id)?.kind {
            ItemKind::Trace(trace) => Some(trace.polyline.clone()),
            _ => None,
        }
    }

    /// The contact scan shared by all four §6.4 rules
    /// (`PullTightAlgo45.java:433-473` and its three copies).
    ///
    /// Walks the contacts at `end_corner` and classifies the joint. Upstream
    /// keeps **the last** contact that set a flag, not the first, and lets a
    /// later `bend` overwrite an earlier `acute_angle`'s bookkeeping while both
    /// flags stay set — the branch order at the bottom of each rule then
    /// resolves the tie in favour of `acute_angle`. Both are reproduced.
    ///
    /// "Last" is only meaningful against upstream's iteration order, which is
    /// **descending by id** — see [`PullTightAlgo::contact_trace_lines_at`],
    /// which is where that order is established and argued.
    ///
    /// `None` propagates upstream's `return null` out of the whole rule.
    ///
    /// * `orthogonal_contact_line` is the 45° rules' extra condition
    ///   (`PullTightAlgo45.java:454`), absent from the any-angle copies;
    /// * `skip_two_corner_contacts` is the any-angle **end** rule's extra guard
    ///   (`PullTightAlgoAnyAngle.java:867`), absent from the other three.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn scan_fork_contacts(
        &mut self,
        id: ItemId,
        end_corner: &Point,
        prev_end_corner: &Point,
        line_direction: &Direction,
        prev_line_direction: &Direction,
        corner_count: usize,
        orthogonal_contact_line: bool,
        skip_two_corner_contacts: bool,
    ) -> Option<ForkCandidate> {
        let contacts = self.contact_trace_lines_at(id, end_corner)?;
        let mut fork = ForkCandidate {
            acute_angle: false,
            bend: false,
            contact: None,
            prev_corner_side: Side::Collinear,
        };
        for contact in contacts {
            if skip_two_corner_contacts && contact.corner_count <= 2 {
                continue;
            }
            let prev_corner_side = prev_end_corner.side_of(&contact.line);
            let projection = line_direction.projection(&contact.line.direction());
            let mut found = false;
            if projection == Signum::Positive && prev_corner_side != Side::Collinear {
                if !orthogonal_contact_line || contact.line.direction().is_orthogonal() {
                    fork.acute_angle = true;
                    found = true;
                }
            } else if projection == Signum::Zero
                && corner_count > 2
                && prev_line_direction.projection(&contact.line.direction()) == Signum::Positive
            {
                fork.bend = true;
                found = true;
            }
            if found {
                fork.contact = Some(contact);
                fork.prev_corner_side = prev_corner_side;
            }
        }
        Some(fork)
    }

    /// Clearance gate for the acute fork splice (PARITY-DIVERGENCES,
    /// "pull-tight fork-splice recheck"). Upstream's chamfer construction has
    /// no clearance recheck: the contact's line becomes the adjusted trace's
    /// new leading line, so the splice can cover ground the trace never
    /// legally occupied — on easyeda-pcb1 a full-width net-31 trace grew a
    /// spur into netless pin 129's pad (gap 295 < 800, the +1v). Checks the
    /// spliced three-line window at the compensated half width, the same
    /// granularity `reposition_line_base` uses for the bend branch. A
    /// degenerate window is not gated (upstream would produce it too).
    pub(crate) fn check_fork_splice(&mut self, lines: &[Line]) -> bool {
        if lines.len() < 3 {
            return true;
        }
        let window = Polyline::new_from_lines(&lines[..3]);
        if window.arr.len() < 3 {
            return true;
        }
        match window.offset_shape(self.curr_half_width, 0) {
            Some(shape) => self.check_trace_shape(&shape),
            None => true,
        }
    }

    /// The chamfer line the `acute_angle` branch of all four §6.4 rules builds
    /// (`PullTightAlgo45.java:475-497` and its three copies, which differ only
    /// in the two `turn_45_degree` factors).
    ///
    /// The distance is `(√2 − 1) · curr_half_width` clamped by the distances to
    /// the previous end corner and to the contact's neighbouring corner, then
    /// `max(d − 1, 1)` — the −1 is upstream's integer safety margin, and the
    /// `>= 0.99` gate below it is why a joint with no room simply stays acute.
    ///
    /// `None` is upstream's "fall through and return null".
    pub(crate) fn fork_chamfer_line(
        &self,
        end_corner: &Point,
        prev_end_corner: &Point,
        contact: &ContactTraceLines,
        prev_corner_side: Side,
        turns: (i32, i32),
    ) -> Option<Line> {
        let (turn_when_left, turn_otherwise) = turns;
        let new_line_dir = if prev_corner_side == Side::OnTheLeft {
            contact.line.direction().turn_45_degree(turn_when_left)
        } else {
            contact.line.direction().turn_45_degree(turn_otherwise)
        };
        let translate_line =
            Line::new_from_point_and_dir(end_corner.to_float().round(), &new_line_dir);
        // `(√2 − 1) · half_width` is the cut that turns a right angle of a trace
        // this wide into a 45° chamfer — the geometric half width, with no
        // clearance involved.
        let mut translate_dist = (Limits::SQRT2 - 1.0) * f64::from(self.curr_raw_half_width);
        let prev_corner_dist = translate_line
            .signed_distance(&prev_end_corner.to_float())
            .abs();
        let other_dist = translate_line.signed_distance(&contact.corner_approx).abs();
        translate_dist = translate_dist.min(prev_corner_dist).min(other_dist);
        if translate_dist < 0.99 {
            return None;
        }
        translate_dist = (translate_dist - 1.0).max(1.0);
        if translate_line.side_of_point(prev_end_corner) == Side::OnTheLeft {
            translate_dist = -translate_dist;
        }
        Some(translate_line.translate(translate_dist))
    }

    /// The trace `pick_items(point, layer, TRACES).iterator().next()` answers
    /// (`RoutingBoard.java:857-871`).
    ///
    /// `pick_items` builds a `TreeSet<Item>` and `Item.compareTo` is
    /// `other.id_no - this.id_no` (`Item.java:94-99`), so the set iterates
    /// **descending by id** and upstream's `next()` is the *highest* id at the
    /// point. That is the trace `insert_forced_trace_polyline` re-reads its
    /// `new_trace` handle from after a split invalidated the old one, so the
    /// tie-break is routing-visible and is reproduced exactly.
    pub(crate) fn picked_trace_at(&mut self, point: &Point, layer: i32) -> Option<ItemId> {
        let mut picked = Vec::new();
        self.board.pick_items(point, Some(layer), &mut picked);
        picked
            .into_iter()
            .filter(|id| {
                self.board
                    .store()
                    .get(*id)
                    .is_some_and(|item| matches!(item.kind, ItemKind::Trace(_)))
            })
            .max()
    }

    // ---- §5.3 the clearance oracle ---------------------------------------

    /// `BasicBoard.check_trace_shape(shape, layer, nets, cl, contact_pins)`
    /// (`:1166-1221`).
    ///
    /// The three obstacle rules, in upstream's order:
    ///
    /// 1. an item in `contact_pins` is skipped;
    /// 2. with `contact_pins` set, **any other pin is an obstacle** even on the
    ///    own net — acid-trap prevention (`:1186-1190`);
    /// 3. otherwise an item is an obstacle unless `is_trace_obstacle` is false
    ///    for one of the routed nets.
    ///
    /// The tree query is upstream's **headless** branch:
    /// `is_clearance_compensation_used()` is `false` outside the GUI
    /// (`SearchTreeManager.java:34`), so the dispatcher at
    /// `BasicBoard.java:1174-1178` takes
    /// `overlapping_tree_entries_with_clearance` on the default tree — the
    /// symmetric enlargement the DRC itself uses, plus the safety margin. See
    /// the module docs for what the compensated-tree variant of this check
    /// used to let through (Issue034).
    ///
    /// The tie-pin tolerance of `:1198-1217` (a foreign-net trace overlapping
    /// only inside a shared tie pin's pad is not an obstacle) is not ported:
    /// it needs multi-net pins, which no fixture in the gate has. Its absence is
    /// conservative — it can only make a move be rejected, never accepted.
    pub fn check_trace_shape(&mut self, shape: &Simplex) -> bool {
        fr_metrics::counters::bump::oracle_checks(1);
        let check_start = Instant::now();
        let result = self.check_trace_shape_timed(shape);
        fr_metrics::counters::bump::oracle_check_nanos(check_start.elapsed().as_nanos() as u64);
        result
    }

    fn check_trace_shape_timed(&mut self, shape: &Simplex) -> bool {
        if !shape_is_contained_in(shape, &self.bounds) {
            return false;
        }
        // The binding-constraint fast path: re-test the tile that rejected the
        // previous candidate. Only ever used to prove a rejection.
        if let Some(binding) = self.binding {
            if self.binding_still_blocks(&binding, shape) {
                fr_metrics::counters::bump::oracle_binding_rejects(1);
                return false;
            }
        }

        let layer = self.curr_layer;
        let cl_type = self.curr_cl_type;
        // The broad phase on the **default, uncompensated** tree, queried with
        // the shape's bounding octagon offset by upstream's
        // `bounds.offset(1.2 * max_clearance)` reach
        // (`ShapeSearchTree.java:442-454`) plus the safety margin the exact
        // test adds. The query is bounds-only
        // (`SearchTree::overlapping_tree_entries_bounds_only`): the symmetric
        // test below re-does the exact geometry per candidate, so the
        // leaf-level shape test the exact query would run is duplicated cost —
        // profiling put it at roughly half of this oracle. The octagon
        // encloses the shape and `offset` preserves that, so no exact-test hit
        // can be missed; an unbounded simplex falls back to the offset query.
        let reach = (1.2 * f64::from(self.max_clearance_on(cl_type, layer))) as i32
            + CLEARANCE_SAFETY_MARGIN;
        let broad = match shape.bounding_octagon() {
            Some(octagon) => TileShape::Oct(octagon.offset(f64::from(reach))),
            None => TileShape::Simplex(shape.offset(f64::from(reach))),
        };
        fr_metrics::counters::bump::tree_queries(1);
        let PullTightAlgo {
            board,
            scratch_entries,
            scratch_stack,
            contact_pins,
            curr_net_no_arr,
            binding,
            ..
        } = self;
        let tree = board.default_tree();
        tree.overlapping_tree_entries_bounds_only(
            board.store(),
            &broad,
            Some(layer),
            &[],
            scratch_stack,
            scratch_entries,
        );
        fr_metrics::counters::bump::oracle_candidates(scratch_entries.len() as u64);
        // The query shape enlarged by half the pairwise clearance, rebuilt only
        // when the clearance bucket changes — upstream's sorted sweep
        // (`:465-484`) without the sort, because clearance classes repeat
        // heavily inside one query. The enlarged shape's bounding box rides
        // along for the per-candidate pre-filter.
        let mut cached: Option<(i32, TileShape, IntBox)> = None;
        for entry in scratch_entries.iter() {
            let Some(item) = board.store().get(entry.item) else {
                continue;
            };
            // The exact test (`:476-490`): both shapes enlarged by half the
            // pairwise clearance *with* the safety margin
            // (`get_value(..., true)`), then `intersects`. An entry that
            // survives the broad phase but not this is merely close. Upstream
            // adds the margin unconditionally for in-bounds indices
            // (`ClearanceMatrix.java:137-151`), so a zero matrix entry still
            // contributes a half clearance of 8.
            let half = board
                .clearances()
                .value_with_margin(cl_type, item.clearance_class, layer)
                / 2;
            let (query_tile, query_box) = match &cached {
                Some((cached_half, tile, query_box)) if *cached_half == half => (tile, query_box),
                _ => {
                    let query = TileShape::Simplex(shape.clone()).enlarge(half);
                    let query_box = query.bounding_box();
                    cached = Some((half, query, query_box));
                    let Some((_, tile, query_box)) = cached.as_ref() else {
                        unreachable!("just stored");
                    };
                    (tile, query_box)
                }
            };
            let Some(slot) = board.store().slot_of(entry.item) else {
                continue;
            };
            let Some(tile) = tree.tile_shape(slot, entry.item, entry.shape_index) else {
                continue;
            };
            // Bounding-box pre-filter: `enlarge` clips against the bounding
            // octagon offset by the same distance (`Simplex::enlarge`), so an
            // enlarged tile never leaves `bounding_box().offset(half)`, and the
            // boxes are integer-exact — the filter can only drop candidates
            // the symmetric test would reject anyway.
            if !query_box.intersects(&tile.bounding_box().offset(f64::from(half))) {
                fr_metrics::counters::bump::oracle_bbox_pruned(1);
                continue;
            }
            fr_metrics::counters::bump::oracle_symmetric_tests(1);
            let exact_start = Instant::now();
            let hits = query_tile.intersects(&tile.enlarge(half));
            fr_metrics::counters::bump::oracle_exact_nanos(exact_start.elapsed().as_nanos() as u64);
            if !hits {
                continue;
            }
            if let Some(pins) = contact_pins.as_ref() {
                if pins.binary_search(&entry.item).is_ok() {
                    continue;
                }
                if matches!(item.kind, ItemKind::Pin(_)) {
                    *binding =
                        board
                            .store()
                            .revision_of(entry.item)
                            .map(|revision| BindingObstacle {
                                item: entry.item,
                                shape_index: entry.shape_index,
                                revision,
                            });
                    return false;
                }
            }
            let mut is_obstacle = true;
            for &net_no in curr_net_no_arr.as_slice() {
                if !item.is_trace_obstacle(net_no) {
                    is_obstacle = false;
                }
            }
            if is_obstacle {
                *binding = board
                    .store()
                    .revision_of(entry.item)
                    .map(|revision| BindingObstacle {
                        item: entry.item,
                        shape_index: entry.shape_index,
                        revision,
                    });
                return false;
            }
        }
        true
    }

    /// `ClearanceMatrix.max_value(cl_type, layer)` — the largest clearance the
    /// current class asks of any class on `layer`.
    fn max_clearance_on(&self, cl_type: i32, layer: i32) -> i32 {
        let clearances = self.board.clearances();
        let mut max = 0;
        for other in 0..clearances.class_count() as i32 {
            max = max.max(clearances.value(cl_type, other, layer));
        }
        max
    }

    /// True when the cached blocker still exists unchanged **and** still
    /// overlaps `shape`, i.e. the candidate is rejected without a traversal.
    fn binding_still_blocks(&self, binding: &BindingObstacle, shape: &Simplex) -> bool {
        if self.board.store().revision_of(binding.item) != Some(binding.revision) {
            return false;
        }
        let Some(slot) = self.board.store().slot_of(binding.item) else {
            return false;
        };
        let tree = self.board.default_tree();
        if tree.shape_layer(slot, binding.item, binding.shape_index) != Some(self.curr_layer) {
            return false;
        }
        let Some(tile) = tree.tile_shape(slot, binding.item, binding.shape_index) else {
            return false;
        };
        let Some(item) = self.board.store().get(binding.item) else {
            return false;
        };
        let half = self.board.clearances().value_with_margin(
            self.curr_cl_type,
            item.clearance_class,
            self.curr_layer,
        ) / 2;
        // Same bounding-box pre-filter as the traversal: integer-exact, so a
        // miss here means the symmetric test below would miss too.
        if !shape
            .bounding_box()
            .offset(f64::from(half))
            .intersects(&tile.bounding_box().offset(f64::from(half)))
        {
            return false;
        }
        TileShape::Simplex(shape.clone())
            .enlarge(half)
            .intersects(&tile.enlarge(half))
    }

    // ---- §6.1 base moves shared by every angle mode ----------------------

    /// `reposition_lines` (`:175-190`) — try to shorten the polyline by
    /// translating one interior line, first success wins.
    pub fn reposition_lines(
        &mut self,
        polyline: &Polyline,
        changed_area: &mut ChangedArea,
    ) -> Option<Polyline> {
        if polyline.arr.len() < 5 {
            return None;
        }
        for index in 2..polyline.arr.len() - 2 {
            let Some(new_line) = self.reposition_line_base(&polyline.arr, index, changed_area)
            else {
                continue;
            };
            self.scratch_lines.clear();
            self.scratch_lines.extend_from_slice(&polyline.arr);
            self.scratch_lines[index] = new_line;
            let result = Polyline::new_from_lines(&self.scratch_lines);
            if result.arr.is_empty() {
                continue;
            }
            return Some(self.skip_segments_of_length_0(&result).unwrap_or(result));
        }
        None
    }

    /// Base `reposition_line` (`:195-290`) — binary-search the largest
    /// translation of line `no` toward the nearer of its two outer corners that
    /// still passes [`PullTightAlgo::check_trace_shape`].
    ///
    /// The three termination guards are upstream's, verbatim, and all three are
    /// load-bearing (spec §5.5):
    ///
    /// * the loop ends when `|delta| <= min_translate_dist`;
    /// * a first-try translation that lands on the *wrong side* of the nearest
    ///   point is backed off by [`NUMERIC_BACKOFF`] and retried, rather than
    ///   accepted (`:245-255`);
    /// * a translation that does not move the line at all aborts (`:238-242`).
    pub(crate) fn reposition_line_base(
        &mut self,
        line_arr: &[Line],
        no: usize,
        changed_area: &mut ChangedArea,
    ) -> Option<Line> {
        if line_arr.len() - no < 3 || no < 2 {
            return None;
        }
        // `:203-213` — both corners of the line to translate must be inside the
        // clip shape.
        if let Some(clip) = self.curr_clip_shape {
            for offset in 0..2 {
                let corner = line_arr[no - 1 + offset].intersection(&line_arr[no + offset]);
                if octagon_is_outside(&clip, &corner) {
                    return None;
                }
            }
        }

        let translate_line = line_arr[no];
        let prev_corner = line_arr[no - 2].intersection(&line_arr[no - 1]);
        let next_corner = line_arr[no + 1].intersection(&line_arr[no + 2]);
        let prev_dist = translate_line.signed_distance(&prev_corner.to_float());
        let next_dist = translate_line.signed_distance(&next_corner.to_float());
        if Signum::of(prev_dist) != Signum::of(next_dist) {
            // `:221-224` — the corners straddle the line; translating cannot
            // shorten it.
            return None;
        }
        let (nearest_point, max_translate_dist) = if prev_dist.abs() < next_dist.abs() {
            (prev_corner, prev_dist)
        } else {
            (next_corner, next_dist)
        };
        let mut translate_dist = max_translate_dist;
        let mut delta_dist = max_translate_dist;
        let side_of_nearest_point = translate_line.side_of_point(&nearest_point);
        let sign = signum_as_int(max_translate_dist);
        let mut new_line: Option<Line> = None;
        self.check_lines[0] = line_arr[no - 1];
        self.check_lines[2] = line_arr[no + 1];
        let mut first_time = true;

        while first_time || delta_dist.abs() > self.min_translate_dist {
            // `:230-237` — the first candidate is the exact line through the
            // nearest corner when that corner is integral; every later one is a
            // float translation.
            self.check_lines[1] = match (first_time, &nearest_point) {
                (true, Point::Int(point)) => {
                    Line::new_from_point_and_dir(*point, &translate_line.direction())
                }
                _ => translate_line.translate(-translate_dist),
            };
            if self.check_lines[1] == translate_line {
                return None;
            }
            let new_side = self.check_lines[1].side_of_point(&nearest_point);
            if new_side != side_of_nearest_point && new_side != Side::Collinear {
                // `:245-255` — overshot because `nearest_point` was not
                // integral; back off and retry.
                //
                // Upstream also decrements `max_translate_dist` here, but the
                // base version never reads it again — unlike
                // `PullTightAlgo45.smoothen_corner` (`:398-403`), where the very
                // same decrement *is* load-bearing because the accept branch
                // compares `translate_dist == max_translate_dist`. So it is
                // dropped here and kept there.
                let shorten = f64::from(sign) * NUMERIC_BACKOFF;
                translate_dist -= shorten;
                delta_dist -= shorten;
                continue;
            }
            let mut check_ok = false;
            let tmp = Polyline::new_from_lines(&self.check_lines);
            if tmp.arr.len() == 3 {
                if let Some(shape) = tmp.offset_shape(self.curr_half_width, 0) {
                    check_ok = self.check_trace_shape(&shape);
                }
            }
            delta_dist /= 2.0;
            if check_ok {
                new_line = Some(self.check_lines[1]);
                if first_time {
                    // `:272-275` — biggest possible change, stop early.
                    break;
                }
                translate_dist += delta_dist;
            } else {
                translate_dist -= delta_dist;
            }
            first_time = false;
        }

        if let Some(line) = new_line {
            // `:283-289` — mark both the new and the old corners.
            let layer = self.curr_layer;
            changed_area.join_point(self.check_lines[0].intersection_approx(&line), layer);
            changed_area.join_point(self.check_lines[2].intersection_approx(&line), layer);
            changed_area.join_point(line_arr[no - 1].intersection_approx(&line_arr[no]), layer);
            changed_area.join_point(line_arr[no].intersection_approx(&line_arr[no + 1]), layer);
        }
        new_line
    }

    /// `skip_segments_of_length_0` (`:292-341`) — drop lines whose two corners
    /// coincide, re-checking clearance because removing one can create a dog ear.
    ///
    /// The first and last corner positions are retained **exactly** (`:298-303`
    /// uses exact `Point` equality there and the
    /// [`MIN_CORNER_DIST_SQUARE`] float test everywhere else), and 45° lines
    /// skip the clearance check entirely because their shapes are intersected
    /// with the bounding octagon anyway (`:318-321`).
    ///
    /// Takes no changed area: upstream does not mark one here either, because
    /// removing a zero-length segment moves no copper — the corners it joins
    /// were already coincident.
    pub fn skip_segments_of_length_0(&mut self, polyline: &Polyline) -> Option<Polyline> {
        let mut polyline_changed = false;
        let mut curr = polyline.clone();
        let mut i = 1usize;
        while i + 1 < curr.arr.len() {
            let try_skip = if i == 1 || i == curr.arr.len() - 2 {
                curr.corner(i) == curr.corner(i - 1)
            } else {
                curr.corner_approx(i)
                    .distance_square(&curr.corner_approx(i - 1))
                    < MIN_CORNER_DIST_SQUARE
            };
            if try_skip {
                self.scratch_lines.clear();
                self.scratch_lines.extend_from_slice(&curr.arr[..i]);
                self.scratch_lines.extend_from_slice(&curr.arr[i + 1..]);
                let expected = self.scratch_lines.len();
                let tmp = Polyline::new_from_lines(&self.scratch_lines);
                let mut check_ok = tmp.arr.len() == expected;
                if check_ok && !curr.arr[i].is_multiple_of_45_degree() {
                    if i > 1 {
                        check_ok = match tmp.offset_shape(self.curr_half_width, i - 2) {
                            Some(shape) => self.check_trace_shape(&shape),
                            None => false,
                        };
                    }
                    if check_ok && i + 2 < curr.arr.len() {
                        check_ok = match tmp.offset_shape(self.curr_half_width, i - 1) {
                            Some(shape) => self.check_trace_shape(&shape),
                            None => false,
                        };
                    }
                }
                if check_ok {
                    polyline_changed = true;
                    curr = tmp;
                    i = i.saturating_sub(1).max(1);
                    continue;
                }
            }
            i += 1;
        }
        if polyline_changed {
            Some(curr)
        } else {
            None
        }
    }

    /// How many of `polyline`'s segments violate clearance **in the verdict's
    /// sense**, using the working state of the trace currently being optimized.
    ///
    /// This is the invariant pull-tight is supposed to preserve: it may leave a
    /// segment that was *already* violating alone, but it must never turn a
    /// clean segment dirty. Tests assert on it, and it is how the phase that
    /// introduces a violation gets identified.
    ///
    /// The arithmetic is the DRC's (`Item.clearance_violations`,
    /// `Item.java:355-415`), not the acceptance oracle's: no safety margin, and
    /// a boundary touch (dimension < 2) does not count. The oracle is strictly
    /// stricter — a trace may be *unmovable* (every candidate rejected by the
    /// margin) while still reading 0 here, which is upstream's reading too.
    ///
    /// Only meaningful between [`PullTightAlgo::trace_pull_tight`]'s working
    /// state being set and the next trace being started.
    pub fn polyline_violation_count(&mut self, polyline: &Polyline) -> usize {
        let mut bad = 0;
        for index in 0..polyline.arr.len().saturating_sub(2) {
            if let Some(shape) = polyline.offset_shape(self.curr_half_width, index) {
                if self.segment_is_dirty(&shape) {
                    bad += 1;
                }
            }
        }
        bad
    }

    /// `Item.clearance_violations`' verdict for one segment shape: 2-D overlap
    /// of the half-enlarged shapes against the default tree, without the safety
    /// margin (`get_value(..., false)` — the margin only widens upstream's
    /// *candidate* gather, never the verdict).
    fn segment_is_dirty(&mut self, shape: &Simplex) -> bool {
        let layer = self.curr_layer;
        let cl_type = self.curr_cl_type;
        let reach = self.max_clearance_on(cl_type, layer);
        let broad = TileShape::Simplex(shape.offset(f64::from(reach)));
        let PullTightAlgo {
            board,
            scratch_entries,
            scratch_stack,
            contact_pins,
            curr_net_no_arr,
            ..
        } = self;
        let tree = board.default_tree();
        tree.overlapping_tree_entries(
            board.store(),
            &broad,
            Some(layer),
            &[],
            scratch_stack,
            scratch_entries,
        );
        for entry in scratch_entries.iter() {
            let Some(item) = board.store().get(entry.item) else {
                continue;
            };
            if let Some(pins) = contact_pins.as_ref() {
                if pins.binary_search(&entry.item).is_ok() {
                    continue;
                }
            }
            let mut is_obstacle = true;
            for &net_no in curr_net_no_arr.as_slice() {
                if !item.is_trace_obstacle(net_no) {
                    is_obstacle = false;
                }
            }
            if !is_obstacle {
                continue;
            }
            let half = board
                .clearances()
                .value(cl_type, item.clearance_class, layer)
                / 2;
            let Some(slot) = board.store().slot_of(entry.item) else {
                continue;
            };
            let Some(tile) = tree.tile_shape(slot, entry.item, entry.shape_index) else {
                continue;
            };
            let query = TileShape::Simplex(shape.clone()).enlarge(half);
            let other = tile.enlarge(half);
            if crate::geom::tile_to_simplex(&query)
                .intersection(&crate::geom::tile_to_simplex(&other))
                .dimension()
                == 2
            {
                return true;
            }
        }
        false
    }

    // ---- accessors the 45° rules need ------------------------------------

    /// The half width the current trace's check shapes are built with — the
    /// raw width; the oracle adds the clearance (`with_clearance`) itself.
    #[inline]
    pub(crate) fn curr_half_width(&self) -> i32 {
        self.curr_half_width
    }

    /// The layer the current trace is on.
    #[inline]
    pub(crate) fn curr_layer(&self) -> i32 {
        self.curr_layer
    }

    /// The clip shape moves are restricted to, if any.
    #[inline]
    pub(crate) fn clip_shape(&self) -> Option<IntOctagon> {
        self.curr_clip_shape
    }

    /// The binary-search termination granularity.
    #[inline]
    pub(crate) fn min_translate_dist(&self) -> f64 {
        self.min_translate_dist
    }
}

/// `new Polyline(Line[])` with upstream's degeneracy check folded in.
///
/// The constructor drops consecutive parallel lines and overlaps, and can be
/// left with fewer than three — a polyline with no corners, which upstream then
/// fails to insert. Answering `None` here is the same outcome one step earlier.
pub(crate) fn polyline_from_lines(lines: &[Line]) -> Option<Polyline> {
    if lines.len() < 3 {
        return None;
    }
    let polyline = Polyline::new_from_lines(lines);
    if polyline.arr.len() < 3 {
        return None;
    }
    Some(polyline)
}

/// `IntOctagon.is_outside(Point)` — outside any of the eight half planes.
///
/// Lives here because `fr_geometry`'s `IntOctagon` does not expose it and that
/// crate is gated.
pub(crate) fn octagon_is_outside(octagon: &IntOctagon, point: &Point) -> bool {
    let Point::Int(p) = point else {
        // A rational corner: fall back to its float position, which is what
        // upstream's `is_outside(FloatPoint)` overload does.
        let f = point.to_float();
        return f.x < f64::from(octagon.left_x)
            || f.y < f64::from(octagon.bottom_y)
            || f.x > f64::from(octagon.right_x)
            || f.y > f64::from(octagon.top_y)
            || (f.x - f.y) < f64::from(octagon.upper_left_diagonal_x)
            || (f.x - f.y) > f64::from(octagon.lower_right_diagonal_x)
            || (f.x + f.y) < f64::from(octagon.lower_left_diagonal_x)
            || (f.x + f.y) > f64::from(octagon.upper_right_diagonal_x);
    };
    let (x, y) = (i64::from(p.x), i64::from(p.y));
    x < i64::from(octagon.left_x)
        || y < i64::from(octagon.bottom_y)
        || x > i64::from(octagon.right_x)
        || y > i64::from(octagon.top_y)
        || (x - y) < i64::from(octagon.upper_left_diagonal_x)
        || (x - y) > i64::from(octagon.lower_right_diagonal_x)
        || (x + y) < i64::from(octagon.lower_left_diagonal_x)
        || (x + y) > i64::from(octagon.upper_right_diagonal_x)
}

/// `Signum.as_int` — the sign of a double as -1 / 0 / 1.
pub(crate) fn signum_as_int(value: f64) -> i32 {
    if value > 0.0 {
        1
    } else if value < 0.0 {
        -1
    } else {
        0
    }
}

/// `TileShape.is_contained_in(bounding_box)` for a simplex.
fn shape_is_contained_in(shape: &Simplex, bounds: &IntBox) -> bool {
    for index in 0..shape.border_line_count() {
        if !shape.corner_is_bounded(index) {
            return false;
        }
        let corner = shape.corner_approx(index);
        if corner.x < f64::from(bounds.ll.x)
            || corner.y < f64::from(bounds.ll.y)
            || corner.x > f64::from(bounds.ur.x)
            || corner.y > f64::from(bounds.ur.y)
        {
            return false;
        }
    }
    true
}

/// The margin `RoutingBoard` pads its outline with, matching
/// [`crate::engine`]'s `BOUNDS_MARGIN` so both units agree on what "on the
/// board" means.
const BOUNDS_MARGIN: i32 = 1000;

/// The board bounding box: the union of every indexed tile shape plus
/// [`BOUNDS_MARGIN`], upstream's `RoutingBoard.bounding_box`.
///
/// Derived exactly the way [`crate::engine`] derives it, so a shape the maze
/// engine considered on-board is never rejected here as off-board.
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
    match bounds {
        Some(bb) => IntBox::new(
            bb.left_x().saturating_sub(BOUNDS_MARGIN),
            bb.bottom_y().saturating_sub(BOUNDS_MARGIN),
            bb.right_x().saturating_add(BOUNDS_MARGIN),
            bb.top_y().saturating_add(BOUNDS_MARGIN),
        ),
        None => IntBox::new(-BOUNDS_MARGIN, -BOUNDS_MARGIN, BOUNDS_MARGIN, BOUNDS_MARGIN),
    }
}
