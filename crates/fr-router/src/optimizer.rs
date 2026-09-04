//! The batch optimizer — the post-routing stage that trades vias and copper
//! away once the router has closed the connections.
//!
//! Ported from the **2.3.0 release** `autoroute/BatchOptimizer.java`
//! (`/tmp/fr-java-release`), which is what the shipped jar runs: the
//! `autoroute/pipeline/BatchOptimizer` the spec body describes is not in the
//! release, and neither are `BatchOptRoute` / `BatchOptRouteMT`
//! (`batch-orchestration.md` conformance appendix, headline correction and
//! rows #81-#96). Spec: `batch-orchestration.md` §7 plus §2.7 of the appendix.
//!
//! | this module | upstream |
//! |---|---|
//! | [`sweep::SortedRouteItems`] | `BatchOptimizer.ReadSortedRouteItems` (`:464-558`) |
//! | [`improvement::ItemRouteResult`] | `ItemRouteResult` |
//! | [`stats`] | the slice of `core/scoring/BoardStatistics` the optimizer reads |
//! | [`BatchOptimizer::opt_route_item`] | `opt_route_item` (`:308-414`) |
//! | [`BatchOptimizer::opt_route_pass`] | `opt_route_pass` (`:215-296`) |
//! | [`BatchOptimizer::run_batch_loop`] | `runBatchLoop` (`:96-207`) |
//! | [`MazeRerouter`] | `BatchAutorouter.autoroute_passes_for_optimizing_item` (`:146-167`) |
//! | [`run_optimization_stage`] | `RoutingJobSchedulerActionThread:179-189` |
//!
//! # The shape of it
//!
//! ```text
//! run_batch_loop:
//!     use_increased_ripup_costs := true
//!     for pass in 1 ..= optimizer.max_passes:
//!         stop if the job budget is spent                            # (ours)
//!         stop if score_before * (1 + threshold) >= 1000        # already near-perfect
//!         opt_route_pass(pass, with_preferred_directions = pass is odd)
//!         if use_increased_ripup_costs and score did not rise:
//!               drop to normal ripup costs and keep going
//!         else stop when (score_after - score_before)/score_before < threshold
//!
//! opt_route_pass:
//!     min_cumulative_trace_length := board weighted length
//!     for item in ReadSortedRouteItems:                          # (x, y, layer), vias first
//!         stop if the job budget is spent                            # (ours)
//!         opt_route_item(item)
//!         stop after 50 consecutive items that did not improve
//!
//! opt_route_item:
//!     rip the item's whole connection (plus fork traces at either end)
//!     re-route it with the maze engine at optimizer ripup costs, ≤ 6 passes
//!     keep it iff (incompletes, vias, length) strictly fell; otherwise roll back
//! ```
//!
//! # Upstream is single-threaded, and its multi-threaded path is broken
//!
//! `BatchOptimizerMultiThreaded` ships in 2.3.0 but is GUI-only, is reached
//! only when `featureFlags.multiThreading && optimizer.maxThreads > 1`, prints
//! its own authors' "multi-threaded route optimization is broken" warning, and
//! carries two confirmed regressions — its per-task optimizer never sets
//! `minCumulativeTraceLength` (killing the length branch of the improvement
//! rule) and never gets the ×10 start ripup boost (conformance #100). The
//! headless path is *always* single-threaded (#3, #106).
//!
//! This module is the headless path, and it is sequential unless a caller asks
//! for otherwise with [`BatchOptimizer::with_threads`]. The parallel path is
//! **not** a port of the broken one: it is `ARCHITECTURE.md`'s T1
//! clone-eval-promote, in which workers only ever produce verdicts on their own
//! board clones and one promoter re-runs the winners on the live board in sweep
//! order. See [`parallel`], which argues both halves of it.
//!
//! # Rollback is a journal transaction, not a board snapshot
//!
//! Upstream brackets each item with `generate_snapshot()` / `undo(null)` on
//! `UndoableObjects`, which bumps a `ConcurrentSkipListMap` level and clones
//! every touched object. Here it is [`Board::begin`] / [`Board::rollback_to`] —
//! `ARCHITECTURE.md` state mechanism 1, an append to a `Vec` and a reverse
//! replay, O(items touched). The observable contract is identical and it is the
//! reason the optimizer is monotone: a reroute that did not improve leaves the
//! board bit-for-bit as it was.

pub mod attempt;
pub mod improvement;
pub mod parallel;
pub mod stats;
pub mod sweep;
pub(crate) mod trace;

use fr_board::board::Board;
use fr_board::item::ItemId;
use fr_rules::board_rules::BoardRules;
use fr_rules::router_settings::RouterSettings;

use crate::batch::threads::Threads;
use crate::batch::BatchRouter;
use crate::optimizer::attempt::{attempt_item, AttemptConfig, AttemptScratch, Disposition};
use crate::optimizer::parallel::{Verdict, WorkerPool};

pub use attempt::AttemptConfig as ItemAttemptConfig;
pub use improvement::{BoardKey, ItemRouteResult, LengthMetric};
pub use stats::PassScore;
pub use sweep::{SortedRouteItems, SweepKey};

/// The board resolution [`stats::pass_score`] assumes when the caller does not
/// know better: 10 000 units per millimetre, the classic Specctra resolution
/// and the one upstream's own log formatter hardcodes
/// (`fr_check::score`, "the hardcoded 10 000 units/mm").
///
/// It scales only the score's *cost* term — trace length and vias — against a
/// penalty term of five million per unrouted connection, so getting it wrong
/// moves the pass-to-pass improvement fraction, never the improvement lattice.
pub const DEFAULT_BOARD_UNITS_PER_MM: f64 = 10_000.0;

/// Starting wall budget for leftover (non-ripped) nets on a backlog retry.
/// Issue110P leftovers never route and must stay cheap; Issue026's route in a
/// few milliseconds. The budget doubles per retry of the same optimizer pass,
/// up to [`BACKLOG_CONNECTION_BUDGET_CAP_MS`] — Issue155's leftovers need a
/// few hundred milliseconds each. Retries only happen after a structural
/// keep, so unroutable leftovers never see the large budgets.
pub const BACKLOG_CONNECTION_BUDGET_MS: u64 = 100;

/// Cap on the per-connection backlog budget; reached after four doublings
/// (100 → 200 → 400 → 800 → 1000), mirroring the shape of upstream's
/// `100000 * 2^(pass-1)` connection-budget escalation.
pub const BACKLOG_CONNECTION_BUDGET_CAP_MS: u64 = 1000;

// ---------------------------------------------------------------------------
// the reroute seam
// ---------------------------------------------------------------------------

/// What [`BatchOptimizer`] asks its rerouter for, once per item.
///
/// The arguments of `BatchAutorouter.autoroute_passes_for_optimizing_item`
/// (`:146-150`) that vary per item and per pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RerouteRequest {
    /// `p_ripup_costs` — already through the `×10-at-start` and
    /// `×0.6-for-traces` factors. Pass `n` of the inner loop spends
    /// `ripup_costs * n`.
    pub ripup_costs: i32,
    /// `p_max_pass_count` — `optimizer.maxAutoroutePasses`, default 6.
    pub max_passes: i32,
    /// `p_with_preferred_directions`. Already baked into the `settings` handed
    /// to [`ItemRerouter::reroute`]; carried separately so a scripted rerouter
    /// can see which half of the alternation it is in.
    pub with_preferred_directions: bool,
    /// Ratsnest count **before the rip-out**. Restoring the ripped connection
    /// is not progress toward another inner pass — see [`MazeRerouter`].
    pub incomplete_floor: usize,
}

/// How the optimizer re-routes the connection it just ripped out.
///
/// Behind a trait for one reason: the optimizer's *decision* logic — the
/// improvement lattice, the ripup schedule, the pass and loop exits — is worth
/// testing on its own, and doing that through the maze engine would mean
/// building a board on which the engine happens to produce the outcome under
/// test. A scripted rerouter states the outcome directly.
///
/// Production always uses [`MazeRerouter`].
pub trait ItemRerouter {
    /// Re-route whatever is now unconnected on `board`.
    ///
    /// Called with the ripped connection already removed and the board inside
    /// the optimizer's transaction: an implementation may mutate freely, and
    /// everything it does is rolled back if the result does not improve.
    fn reroute(
        &mut self,
        board: &mut Board,
        rules: &BoardRules,
        settings: &RouterSettings,
        request: &RerouteRequest,
    );

    /// Nets of the item about to be ripped. [`MazeRerouter`] uses them as the
    /// inner work-list when leftover unrouted nets have already been tried
    /// against this board. Scripted rerouters ignore it.
    fn prepare_item(&mut self, _focus_nets: &[i32]) {}

    /// Called at the start of each optimizer pass so the first item always
    /// retries the leftover backlog.
    fn begin_pass(&mut self) {}

    /// `true` when the just-attempted item was **kept** and reduced incompletes.
    /// The leftover backlog is retried only then; a via-only keep, a length-only
    /// keep or a rollback leaves the leftover obstacles in place.
    fn note_structural(&mut self, _closed_an_incomplete: bool) {}
}

/// The production rerouter: `BatchAutorouter.autoroute_passes_for_optimizing_item`
/// (`:146-167`).
///
/// A **fresh** router per item, as upstream constructs a fresh
/// `BatchAutorouter` per item, with `p_remove_unconnected_vias = true`
/// (conformance #72) — the optimizer is exactly the stage that should be
/// deleting vias nothing hangs off. Passes escalate ripup as everywhere else:
/// pass `n` spends `ripup_costs * n` (`:1280`).
///
/// It drives [`BatchRouter::autoroute_pass`] in a plain loop rather than
/// [`BatchRouter::run`], because `autoroute_passes_for_optimizing_item` **is** a
/// plain loop: the pass ≥ 8 restore check and the closing "restore the best
/// board ever seen" both live in `runBatchLoop` and neither is reachable inside
/// six passes. Running them here would also mean an arena snapshot/restore in
/// the middle of the optimizer's own journal transaction.
///
/// # The escalation stops when it stops routing (divergence)
///
/// Upstream's loop guard is `still_unrouted_items`, and
/// `autoroute_pass` computes that as `routed > 0 || not_routed > 0` (`:688`) —
/// "did the pass attempt anything", not "did the pass achieve anything". On a
/// board that carries even one connection the router could not close, the guard
/// is permanently true, so **every** item pays all six passes, and each of those
/// passes re-attempts the whole board's unroutable backlog at a ripup cost that
/// grows with the pass number. That is not a corner case: it is where the
/// optimizer's time goes.
///
/// Measured on Issue026 at `-mp 3`, one optimizer pass over 57 items:
///
/// | inner pass | wall | connections queued | routed |
/// |---|---|---|---|
/// | 1 | 22.8 s | 449 | 78 |
/// | 2 | 21.2 s | 346 | 5 |
/// | 3 | 22.2 s | 343 | 3 |
/// | 4 | 22.0 s | 337 | 0 |
/// | 5 | 21.4 s | 337 | 0 |
/// | 6 | 21.0 s | 337 | 0 |
///
/// Passes 4-6 cost half the stage and routed nothing at all; the whole
/// escalation past the first barren pass bought 8 connections out of 86 for
/// 5/6 of the time. So this port stops the escalation at the first pass that
/// routes nothing — the loop's own javadoc says it exists to "auto-route ripup
/// passes **until the board is completed**", and a pass that completed nothing
/// has not moved towards that. `board_complete` and upstream's `progressed` are
/// both still honoured; `routed == 0` is the added rule.
///
/// The board this produces was byte-identical to the unmodified loop's on all
/// **21** suite fixtures that finish at `-mp 1` under the shipped
/// [`LengthMetric`], for 1.2x - 2.7x less wall clock — and on `Issue190` it is
/// the difference between a 150 s timeout and a finished board. It is recorded
/// in `docs/PARITY-DIVERGENCES.md`.
///
/// # ... and it stops when the passes stop closing connections (divergence)
///
/// `routed > 0` is the right rule in the currency of *attempts* and the wrong
/// one in the currency of *results*, because on a saturated board one connection
/// is routed by breaking several others. `Issue103-Board-Routed` is that board
/// end to end — it arrives fully wired, and `FR_OPT_TRACE` over the first
/// optimizer item (a via, 16 items in its connection) reads:
///
/// | inner pass | queued | routed | wall | board incompletes |
/// |---|---|---|---|---|
/// | 1 | 2 | 1 | 43.2 s | 1 → 8 |
/// | 2 | 16 | 2 | 73.3 s | 8 → 13 |
/// | 3 | 25 | 0 | 0.02 s | 13 → 13 |
///
/// Every pass "routed" something and every pass left the board with **more**
/// open connections than it found, because each insertion into full copper
/// shatters ten-odd neighbours (the per-connection records show `incomplete=1
/// ->15`, `8->19`, `19->22`, with `ripped=0` throughout — it is the insertion,
/// not the maze's ripup). So `routed > 0` holds all the way down and the item
/// pays 116 s to arrive somewhere strictly worse than it started, at which point
/// the improvement lattice rejects it and every bit of it is rolled back.
///
/// The escalation therefore also requires the pass to have **reduced the board's
/// incomplete count** against the **pre-rip** ratsnest
/// ([`RerouteRequest::incomplete_floor`]). Restoring the ripped connection
/// brings the count back to where `opt_route_item` started and is not a reason
/// to run another inner pass of leftover unrouted nets. A pass that closes a
/// leftover still drives the count down and earns the next one. [`PassOutcome::incomplete_count`] is already
/// computed at the end of every pass, so the rule costs one extra ratsnest per
/// item and nothing per pass.
///
/// It cannot suppress a reroute that works: a pass that closes what it was given
/// drives the count down and earns the next one, which is exactly the
/// multi-pass repair upstream's escalation is for. What it stops is the
/// treadmill above, where pass `n + 1` is queued more work than pass `n` was.
///
/// [`PassOutcome::incomplete_count`]: crate::batch::PassOutcome::incomplete_count
/// Production rerouter. Stateful across items of one pass: leftover unrouted
/// nets are attempted on the first item (and again after a structural keep)
/// and skipped while the board's obstacles have not structurally moved.
#[derive(Debug, Clone)]
pub struct MazeRerouter {
    /// Try every unrouted net, not just [`Self::focus_nets`]. Consumed by the
    /// retry itself; re-armed only by a structural keep.
    retry_backlog: bool,
    /// Nets of the item being ripped — the connection `opt_route_item` asked
    /// to re-route.
    focus_nets: Vec<i32>,
    /// Per-connection wall budget of the next backlog retry; doubles per
    /// retry, capped at [`BACKLOG_CONNECTION_BUDGET_CAP_MS`].
    backlog_budget_ms: u64,
}

impl Default for MazeRerouter {
    fn default() -> MazeRerouter {
        MazeRerouter {
            retry_backlog: true,
            focus_nets: Vec::new(),
            backlog_budget_ms: BACKLOG_CONNECTION_BUDGET_MS,
        }
    }
}

impl ItemRerouter for MazeRerouter {
    fn prepare_item(&mut self, focus_nets: &[i32]) {
        self.focus_nets.clear();
        self.focus_nets.extend_from_slice(focus_nets);
    }

    fn begin_pass(&mut self) {
        self.retry_backlog = true;
        self.backlog_budget_ms = BACKLOG_CONNECTION_BUDGET_MS;
    }

    fn note_structural(&mut self, structural: bool) {
        self.retry_backlog = structural;
    }

    fn reroute(
        &mut self,
        board: &mut Board,
        rules: &BoardRules,
        settings: &RouterSettings,
        request: &RerouteRequest,
    ) {
        let mut router = BatchRouter::new(rules, settings);
        router.start_ripup_costs = request.ripup_costs;
        router.remove_unconnected_vias = true;
        // Upstream re-derives the work list on the next of its ≤ 6 passes
        // instead of re-queueing inside one, so the requeue divergence — which
        // exists to cover a *one*-pass run — does not apply here. `run` is what
        // resolves the policy into the flag the pass reads, and this path does
        // not go through it, so the flag is set directly.
        router.requeue_policy = crate::batch::schedule::RequeuePolicy::Never;
        router.set_requeue_active(false);
        // Always restore the ripped connection first, at the full 100 s
        // allowance. Mixing it with leftover unrouted nets is how Issue110P
        // paid 9 s on item 1 for ten NotRouted searches.
        if !self.focus_nets.is_empty() {
            router.net_filter = Some(self.focus_nets.iter().copied().collect());
        }
        let mut incomplete_floor = request.incomplete_floor;
        for pass_no in 1..=request.max_passes {
            let started = trace::is_enabled().then(std::time::Instant::now);
            let outcome = router.autoroute_pass(board, pass_no);
            if let Some(started) = started {
                trace::line(&format!(
                    "inner, inner={pass_no}, queued={}, routed={}, not_routed={}, skipped={}, budget_hits={}, incomplete={incomplete_floor}->{}, ms={}",
                    outcome.queued,
                    outcome.routed,
                    outcome.not_routed,
                    outcome.skipped,
                    outcome.connections_hitting_budget,
                    outcome.incomplete_count,
                    started.elapsed().as_millis(),
                ));
            }
            if !escalation_continues(&outcome, incomplete_floor) {
                break;
            }
            incomplete_floor = outcome.incomplete_count;
        }
        if !self.retry_backlog {
            return;
        }
        // Consumed: the next retry needs a new structural keep.
        self.retry_backlog = false;
        // Probe cheaply, then escalate only while the probing keeps routing:
        // Issue155's leftovers close (three tiers, 100 → 400 ms); Issue110P's
        // never route, so the probe is all they get — 10 × 100 ms instead of
        // the 10 × 1.4 s the whole-board work list used to pay per item.
        let mut budget_ms = self.backlog_budget_ms;
        loop {
            let mut backlog = BatchRouter::new(rules, settings);
            backlog.start_ripup_costs = request.ripup_costs;
            backlog.remove_unconnected_vias = true;
            backlog.requeue_policy = crate::batch::schedule::RequeuePolicy::Never;
            backlog.set_requeue_active(false);
            backlog.budget.base_connection_ms = budget_ms;
            let started = trace::is_enabled().then(std::time::Instant::now);
            let outcome = backlog.autoroute_pass(board, 1);
            if let Some(started) = started {
                trace::line(&format!(
                    "inner, inner=backlog, queued={}, routed={}, not_routed={}, skipped={}, budget_hits={}, budget_ms={budget_ms}, incomplete={incomplete_floor}->{}, ms={}",
                    outcome.queued,
                    outcome.routed,
                    outcome.not_routed,
                    outcome.skipped,
                    outcome.connections_hitting_budget,
                    outcome.incomplete_count,
                    started.elapsed().as_millis(),
                ));
            }
            incomplete_floor = outcome.incomplete_count;
            if outcome.routed == 0 || budget_ms >= BACKLOG_CONNECTION_BUDGET_CAP_MS {
                break;
            }
            budget_ms = (budget_ms * 2).min(BACKLOG_CONNECTION_BUDGET_CAP_MS);
        }
        self.backlog_budget_ms = budget_ms;
    }
}

/// Whether the item's reroute is worth another, dearer pass — the `:156-161`
/// loop guard, plus this port's rule (see [`MazeRerouter`]).
///
/// Upstream's two terms first, in its own order:
///
/// * `board_complete` — `autoroute_pass` found nothing to route (`:437-440`,
///   upstream's early `return false`);
/// * `progressed` — `routed > 0 || not_routed > 0` (`:688`), the value upstream
///   assigns to `still_unrouted_items`.
///
/// Then this port's two, both of them the same argument in different currencies
/// (see [`MazeRerouter`]):
///
/// * `routed > 0`. A pass that routed nothing has not moved the board towards
///   the completion this loop exists to reach, and the only thing the next pass
///   changes is the price of ripping the copper that already refused to move.
/// * `incomplete_count < incomplete_floor`, where the floor is what the board
///   carried when the pass began. A pass that routed connections *and left more
///   of them open than it found* has moved away from completion, however busy it
///   was; the next pass would be handed that larger backlog at a dearer ripup.
fn escalation_continues(outcome: &crate::batch::PassOutcome, incomplete_floor: usize) -> bool {
    !outcome.board_complete
        && outcome.progressed
        && outcome.routed > 0
        && outcome.incomplete_count < incomplete_floor
}

// ---------------------------------------------------------------------------
// outcomes
// ---------------------------------------------------------------------------

/// Why one optimizer pass stopped iterating items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassExit {
    /// `ReadSortedRouteItems.next()` returned null (`:243-246`) — every item
    /// after the cursor has been visited.
    Exhausted,
    /// `optimizer.maxConsecutiveFailures` (50) items in a row did not improve
    /// (`:257-263`).
    ConsecutiveFailures,
    /// `optimizer.maxItems` reached mid-pass (`:236-240`).
    MaxItems,
    /// The job's wall budget ran out mid-sweep, or a signal handler asked the
    /// run to stop ([`crate::batch::job_budget_spent`]).
    ///
    /// Not an upstream exit. Without it the sweep runs on: a spent budget makes
    /// every remaining attempt *fail* rather than end the pass, so the pass
    /// walks its whole 50-item failure streak before
    /// [`PassExit::ConsecutiveFailures`] stops it — measured at **3.0-5.7 s per
    /// pass** on `Issue209-split10`, past a deadline the caller set precisely
    /// so that it would not be killed.
    JobDeadline,
}

/// Why [`BatchOptimizer::run_batch_loop`] stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopExit {
    /// `optimizer.maxPasses` (100) passes run (`:120`).
    MaxPasses,
    /// `optimizer.maxItems` item attempts made (`:121`).
    MaxItems,
    /// `scoreBefore * (1 + threshold) >= 1000` — the board is already within a
    /// threshold's width of a perfect score (`:142-147`).
    NearMaximumScore,
    /// The pass gained less than `optimizer.optimizationImprovementThreshold`
    /// (1 %) of score (`:172-177`).
    BelowImprovementThreshold,
    /// The job's wall budget ran out, or a signal handler asked the run to stop
    /// ([`crate::batch::job_budget_spent`]). Not an upstream exit; see
    /// [`PassExit::JobDeadline`] for what it costs not to have it.
    JobDeadline,
}

/// What one pass did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OptimizerPassOutcome {
    /// 1-based pass number.
    pub pass_no: i32,
    /// `pass_no % 2 != 0` (`:153`).
    pub with_preferred_directions: bool,
    /// Whether items in this pass paid the ×10 start ripup surcharge.
    pub used_increased_ripup_costs: bool,
    /// Items handed out by the sweep and actually attempted.
    pub items_attempted: usize,
    /// Of those, the ones whose reroute was kept.
    pub items_improved: usize,
    /// The failure streak when the pass ended.
    pub consecutive_failures: i32,
    /// Why it stopped iterating.
    pub exit: PassExit,
    /// `route_improved` — the via/length ratio of the **last improved item**,
    /// not a maximum and not a mean (`:265-269`). Its only consumer is the
    /// `== 0` test that drops the ripup surcharge (`:281-284`).
    pub route_improved: f64,
    /// Board measurements at pass start.
    pub key_before: BoardKey,
    /// Board measurements at pass end.
    pub key_after: BoardKey,
    /// `getNormalizedScore` before the pass.
    pub score_before: f64,
    /// `getNormalizedScore` after the pass.
    pub score_after: f64,
}

/// What the whole optimizer stage did.
#[derive(Debug, Clone)]
pub struct OptimizerOutcome {
    /// One entry per pass actually run.
    pub passes: Vec<OptimizerPassOutcome>,
    /// Why the loop stopped.
    pub exit: LoopExit,
    /// `totalItemsOptimized`.
    pub total_items_optimized: i32,
    /// The referee's verdict before the first pass.
    pub score_before: PassScore,
    /// The referee's verdict after the last one.
    pub score_after: PassScore,
}

impl OptimizerOutcome {
    /// Items kept over every pass.
    pub fn items_improved(&self) -> usize {
        self.passes.iter().map(|pass| pass.items_improved).sum()
    }
}

/// The counters one pass keeps while it walks its items, plus the two stop
/// rules that are counted rather than measured.
///
/// Extracted so that the sequential driver and the T1 promoter cannot drift:
/// both call [`PassTally::record`] once per attempted item, in sweep order, and
/// a pass therefore reports the same `items_attempted` / `items_improved` /
/// `route_improved` triple whichever one ran it.
#[derive(Debug, Clone, PartialEq)]
struct PassTally {
    /// `:315-321`'s board measurement at pass start; the denominator of
    /// `route_improved`.
    key_before: BoardKey,
    /// `optimizer.maxItems` (`:236-240`).
    max_items: i32,
    /// `optimizer.maxConsecutiveFailures` (`:257-263`).
    max_consecutive_failures: i32,
    items_attempted: usize,
    items_improved: usize,
    consecutive_failures: i32,
    route_improved: f64,
}

impl PassTally {
    fn new(key_before: BoardKey, max_items: i32, max_consecutive_failures: i32) -> PassTally {
        PassTally {
            key_before,
            max_items,
            max_consecutive_failures,
            items_attempted: 0,
            items_improved: 0,
            consecutive_failures: 0,
            route_improved: 0.0,
        }
    }

    /// `:236-240`. Upstream's guard, with `max_items <= 0` meaning "no cap".
    fn item_cap_reached(&self, total_items_optimized: i32) -> bool {
        self.max_items > 0 && total_items_optimized >= self.max_items
    }

    /// One attempted item. `Some(exit)` means the pass has to stop here.
    fn record(&mut self, result: &ItemRouteResult) -> Option<PassExit> {
        self.items_attempted += 1;
        if result.improved() {
            self.items_improved += 1;
            // Upstream resets the streak on any `improved()` (`:257-259`). A
            // length-only micro-win (Issue110P: 911.91 → 911.91) is
            // `improved()` under both metrics, and a via-only keep is too;
            // either one would hold the 50-item abort off. The streak
            // therefore only clears when an *incomplete* dropped — that is
            // the keep that actually moved leftover-unroutable copper.
            // Length-only and via-only still commit. See PARITY-DIVERGENCES.
            if result.reduced_incompletes() {
                self.consecutive_failures = 0;
            } else {
                self.consecutive_failures += 1;
            }
            // `:265-269`. Assignment, not accumulation: whatever the last
            // improved item scored is what the pass reports.
            self.route_improved =
                if self.key_before.via_count != 0 && self.key_before.trace_length != 0.0 {
                    1.0 - ((result.via_count() as f64 / self.key_before.via_count as f64)
                        + (result.trace_length() / self.key_before.trace_length))
                        / 2.0
                } else {
                    0.0
                };
        } else {
            self.consecutive_failures += 1;
        }
        // `:257-263`. Checked after both branches so a run of length-only
        // keeps can still fire the abort.
        if self.consecutive_failures >= self.max_consecutive_failures {
            Some(PassExit::ConsecutiveFailures)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// the optimizer
// ---------------------------------------------------------------------------

/// The batch optimizer — `autoroute/BatchOptimizer.java`; sequential by default
/// and T1-parallel under [`BatchOptimizer::with_threads`].
pub struct BatchOptimizer<'r> {
    rules: &'r BoardRules,
    settings: &'r RouterSettings,
    rerouter: Box<dyn ItemRerouter + 'r>,
    /// Board units per millimetre, for the score's trace-length cost term.
    /// Defaults to [`DEFAULT_BOARD_UNITS_PER_MM`]; a caller that imported the
    /// design knows the real value and should set it.
    pub board_units_per_mm: f64,
    /// Which length pair the improvement lattice compares. Defaults to
    /// [`LengthMetric::WeightedBoth`], the metric under which "improved" means
    /// the board's key actually fell; [`LengthMetric::UpstreamMixed`] is the
    /// release's behaviour, bug included. See [`improvement`].
    pub length_metric: LengthMetric,
    /// `use_increased_ripup_costs` (`:32`, `:99`): the first pass(es) pay the
    /// `additionalRipupCostFactorAtStart` surcharge, and two independent rules
    /// can clear it (conformance #95).
    use_increased_ripup_costs: bool,
    /// `min_cumulative_trace_length` (`:35`): a **weighted** length, reset at
    /// the start of every pass and lowered whenever an item improves. It is the
    /// `before` side of every item's length comparison.
    min_cumulative_trace_length: f64,
    /// `totalItemsOptimized` (`:36`), across every pass.
    total_items_optimized: i32,
    /// How many workers evaluate candidates. [`Threads::single`] is the
    /// sequential release path; anything more is [`parallel`].
    threads: Threads,
    /// Whether [`Self::threads`] may be honoured at all. False once a caller has
    /// supplied its own [`ItemRerouter`]: a worker has to build a rerouter of
    /// its own on its own thread, and a `Box<dyn ItemRerouter>` handed in from
    /// outside is neither `Send` nor cloneable. Scripted rerouters exist to test
    /// the decision logic, which is the promoter's, so nothing is lost.
    parallel_capable: bool,
    /// The workers' board clones, kept across windows.
    pool: WorkerPool,
    scratch: AttemptScratch,
}

impl<'r> BatchOptimizer<'r> {
    /// An optimizer over these rules and settings, re-routing with the maze
    /// engine.
    ///
    /// `settings` must already have been through
    /// [`RouterSettings::apply_board_specific_optimizations`], for the same
    /// reason [`BatchRouter::new`] requires it.
    pub fn new(rules: &'r BoardRules, settings: &'r RouterSettings) -> BatchOptimizer<'r> {
        let mut optimizer =
            BatchOptimizer::with_rerouter(rules, settings, Box::new(MazeRerouter::default()));
        optimizer.parallel_capable = true;
        optimizer
    }

    /// An optimizer with a caller-supplied [`ItemRerouter`].
    ///
    /// Always sequential; see [`BatchOptimizer::parallel_capable`].
    pub fn with_rerouter(
        rules: &'r BoardRules,
        settings: &'r RouterSettings,
        rerouter: Box<dyn ItemRerouter + 'r>,
    ) -> BatchOptimizer<'r> {
        BatchOptimizer {
            rules,
            settings,
            rerouter,
            board_units_per_mm: DEFAULT_BOARD_UNITS_PER_MM,
            length_metric: LengthMetric::default(),
            use_increased_ripup_costs: true,
            min_cumulative_trace_length: 0.0,
            total_items_optimized: 0,
            threads: Threads::default(),
            parallel_capable: false,
            pool: WorkerPool::new(),
            scratch: AttemptScratch::new(),
        }
    }

    /// Sets the worker budget for candidate evaluation ([`parallel`]).
    ///
    /// Ignored — silently, and [`BatchOptimizer::threads`] reports the effective
    /// value — when the optimizer was built with a scripted rerouter.
    pub fn with_threads(mut self, threads: Threads) -> BatchOptimizer<'r> {
        self.threads = threads;
        self
    }

    /// The worker budget actually in force: [`Threads::single`] whenever the
    /// parallel path is unavailable.
    pub fn threads(&self) -> Threads {
        if self.parallel_capable {
            self.threads
        } else {
            Threads::single()
        }
    }

    /// Whether the parallel evaluation path may run at all — i.e. whether this
    /// optimizer owns the production [`MazeRerouter`] a worker can rebuild.
    pub fn parallel_capable(&self) -> bool {
        self.parallel_capable
    }

    /// `totalItemsOptimized`.
    pub fn total_items_optimized(&self) -> i32 {
        self.total_items_optimized
    }

    /// Whether the next item still pays the start-of-run ripup surcharge.
    pub fn uses_increased_ripup_costs(&self) -> bool {
        self.use_increased_ripup_costs
    }

    // -- the loop ---------------------------------------------------------

    /// `runBatchLoop()` (`:96-207`).
    ///
    /// Stop conditions, in the order the release evaluates them: `maxPasses`,
    /// `maxItems`, the near-perfect early exit, then — after the pass — the
    /// ripup-surcharge drop and the improvement threshold.
    pub fn run_batch_loop(&mut self, board: &mut Board) -> OptimizerOutcome {
        let optimizer = &self.settings.optimizer;
        let max_passes = optimizer.max_passes;
        let max_items = optimizer.max_items;
        let threshold = f64::from(optimizer.improvement_threshold);

        let score_before = self.score(board);
        let mut passes: Vec<OptimizerPassOutcome> = Vec::new();
        let mut current_pass = 0;

        let exit = loop {
            // Ours, and checked before upstream's rules for the same reason the
            // batch loop checks its own first (`schedule.rs`): whatever the
            // loop would do next costs at least one more pass, and the budget
            // exists to leave time to write and judge the board we have. A
            // process with no budget armed never gets past this line — it is
            // one relaxed atomic load.
            if crate::batch::job_budget_spent() {
                break LoopExit::JobDeadline;
            }
            if current_pass >= max_passes {
                break LoopExit::MaxPasses;
            }
            if self.total_items_optimized >= max_items {
                break LoopExit::MaxItems;
            }
            // `:129` — the counter advances before the near-perfect check, so
            // a board that is already good enough still reports the pass it
            // declined to run.
            current_pass += 1;

            let score_at_pass_start = self.score(board).normalized;
            // `:141-147`. `1000` is `getNormalizedScore`'s maximum.
            if score_at_pass_start * (1.0 + threshold) >= 1000.0 {
                break LoopExit::NearMaximumScore;
            }

            // `:153` — "to create more variations".
            let with_preferred_directions = current_pass % 2 != 0;
            let mut pass = self.opt_route_pass(board, current_pass, with_preferred_directions);
            pass.score_before = score_at_pass_start;

            let score_at_pass_end = self.score(board).normalized;
            pass.score_after = score_at_pass_end;
            passes.push(pass);

            let pass_improvement = if score_at_pass_start > 0.0 {
                (score_at_pass_end - score_at_pass_start) / score_at_pass_start
            } else {
                0.0
            };

            // `:164-170` — the *second* of the two independent surcharge-drop
            // rules (conformance #95); the first one lives at the end of
            // `opt_route_pass` and fires on the via/length ratio instead.
            //
            // Upstream's `double scoreImprovement = -1` means "run another pass
            // whatever the number says", and it is later tested with
            // `scoreImprovement != -1`. A sentinel value inside an `f64` that
            // control flow then compares for equality is exactly what this port
            // does not do, so the sentinel is the `None`.
            let score_improvement =
                if self.use_increased_ripup_costs && score_at_pass_end <= score_at_pass_start {
                    self.use_increased_ripup_costs = false;
                    None
                } else {
                    Some(pass_improvement)
                };

            // `:172-177`.
            if score_improvement.is_some_and(|improvement| improvement < threshold) {
                break LoopExit::BelowImprovementThreshold;
            }
        };

        let score_after = self.score(board);
        OptimizerOutcome {
            passes,
            exit,
            total_items_optimized: self.total_items_optimized,
            score_before,
            score_after,
        }
    }

    /// `opt_route_pass(p_pass_no, p_with_preferred_directions)` (`:215-296`).
    ///
    /// `score_before` / `score_after` on the returned outcome are left at zero;
    /// [`Self::run_batch_loop`] fills them in from the scores it already took.
    pub fn opt_route_pass(
        &mut self,
        board: &mut Board,
        pass_no: i32,
        with_preferred_directions: bool,
    ) -> OptimizerPassOutcome {
        let used_increased_ripup_costs = self.use_increased_ripup_costs;
        let key_before = stats::board_key(board);
        // `:225` — the pass's length floor is the board's weighted length.
        self.min_cumulative_trace_length = key_before.weighted_trace_length;

        let max_items = self.settings.optimizer.max_items;
        let max_consecutive_failures = self.settings.optimizer.max_consecutive_failures;
        // One clone per pass, not per item: the whole effect of
        // `p_with_preferred_directions` is on the layer cost pairs
        // (`BatchAutorouter.java:123-132`).
        let pass_settings = if with_preferred_directions {
            None
        } else {
            Some(without_preferred_directions(self.settings))
        };
        let pass_settings = pass_settings.as_ref().unwrap_or(self.settings);

        self.rerouter.begin_pass();
        let mut sweep = SortedRouteItems::new();
        let mut tally = PassTally::new(key_before, max_items, max_consecutive_failures);
        let exit = if self.threads().is_parallel() {
            self.drive_items_in_parallel(
                board,
                &mut sweep,
                pass_settings,
                with_preferred_directions,
                &mut tally,
            )
        } else {
            self.drive_items_sequentially(
                board,
                &mut sweep,
                pass_settings,
                with_preferred_directions,
                &mut tally,
            )
        };
        let PassTally {
            items_attempted,
            items_improved,
            consecutive_failures,
            route_improved,
            ..
        } = tally;

        // `:281-284` — the *first* surcharge-drop rule. `runBatchLoop`
        // discards this function's return value (`:154`), so upstream's
        // `route_improved = -1` here is dead; only the flag matters
        // (conformance #95). The `== 0.0` is upstream's exact test on a float
        // that is either untouched (no item improved all pass) or a ratio; it
        // gates a cost schedule, never a piece of geometry.
        if self.use_increased_ripup_costs && route_improved == 0.0 {
            self.use_increased_ripup_costs = false;
        }

        let key_after = stats::board_key(board);
        OptimizerPassOutcome {
            pass_no,
            with_preferred_directions,
            used_increased_ripup_costs,
            items_attempted,
            items_improved,
            consecutive_failures,
            exit,
            route_improved,
            key_before,
            key_after,
            score_before: 0.0,
            score_after: 0.0,
        }
    }

    /// The pass's running counters, shared by the sequential and the parallel
    /// item drivers so that a pass reports the same numbers either way.
    ///
    /// It owns the two stop rules that are counted rather than measured —
    /// `optimizer.maxItems` (`:236-240`) and `optimizer.maxConsecutiveFailures`
    /// (`:257-263`) — because the parallel driver has to apply them at exactly
    /// the same point in the sweep order the sequential one does.
    fn record(&mut self, tally: &mut PassTally, result: &ItemRouteResult) -> Option<PassExit> {
        self.total_items_optimized += 1;
        tally.record(result)
    }

    /// The release's item loop: one item at a time, each one measured against
    /// the board the previous one left (`:230-279`).
    fn drive_items_sequentially(
        &mut self,
        board: &mut Board,
        sweep: &mut SortedRouteItems,
        pass_settings: &RouterSettings,
        with_preferred_directions: bool,
        tally: &mut PassTally,
    ) -> PassExit {
        loop {
            // Ours: a spent budget ends the *pass*, not just the attempt. The
            // per-item router polls the same flag, so every remaining item
            // would still be ripped, re-attempted against a zero allowance and
            // rolled back — the work the deadline is trying to avoid.
            if crate::batch::job_budget_spent() {
                break PassExit::JobDeadline;
            }
            // `:236-240`.
            if tally.item_cap_reached(self.total_items_optimized) {
                break PassExit::MaxItems;
            }
            // `:243-246`.
            let Some(item) = sweep.next(board, &mut self.scratch.contacts) else {
                break PassExit::Exhausted;
            };
            let result = self.opt_route_item(board, item, pass_settings, with_preferred_directions);
            if let Some(exit) = self.record(tally, &result) {
                break exit;
            }
        }
    }

    /// **T1** — the same loop, with candidate evaluation fanned out over
    /// [`parallel::WorkerPool`].
    ///
    /// The sweep is still drawn sequentially against the live board, one window
    /// at a time; the workers only answer "would this item's reroute have
    /// improved the board as it stands now"; and every item the promoter acts on
    /// goes through [`BatchOptimizer::opt_route_item`] on the live board, in
    /// sweep order. See [`parallel`] for the soundness and determinism
    /// arguments, including why a `Rejected` verdict may be trusted and an
    /// `Improved` one may not.
    fn drive_items_in_parallel(
        &mut self,
        board: &mut Board,
        sweep: &mut SortedRouteItems,
        pass_settings: &RouterSettings,
        with_preferred_directions: bool,
        tally: &mut PassTally,
    ) -> PassExit {
        let threads = self.threads();
        // The window adapts: it starts narrow and widens only while the board
        // stops moving. `parallel::next_window_size` carries the measurement
        // that argues for it.
        let mut window_size = parallel::initial_window_size(threads);
        let mut window: Vec<ItemId> = Vec::with_capacity(parallel::max_window_size(threads));
        loop {
            // As in the sequential driver: one window is the granularity at
            // which T1 can stop, and stopping here is what keeps the parallel
            // mode's post-deadline tail the same size as the sequential one's.
            if crate::batch::job_budget_spent() {
                break PassExit::JobDeadline;
            }
            if tally.item_cap_reached(self.total_items_optimized) {
                break PassExit::MaxItems;
            }
            // Draw the window against the live board — `ReadSortedRouteItems`
            // is a cursor over a mutating board and this is the only place the
            // parallel driver lets it see one.
            window.clear();
            let mut exhausted = false;
            while window.len() < window_size {
                match sweep.next(board, &mut self.scratch.contacts) {
                    Some(item) => window.push(item),
                    None => {
                        exhausted = true;
                        break;
                    }
                }
            }
            if window.is_empty() {
                break PassExit::Exhausted;
            }

            let cfg = self.attempt_config(pass_settings, with_preferred_directions);
            let verdicts = self.pool.evaluate(board, &cfg, &window, threads);

            let improved_before = tally.items_improved;
            for (index, item) in window.iter().copied().enumerate() {
                // The budget can go while this window is being promoted, and it
                // is worse here than anywhere else: `parallel::evaluate_one`
                // cancels the workers the moment it goes, so every item left in
                // the window comes back `Unevaluated` — which the promoter
                // re-runs *live*, at full price, one attempt per remaining
                // item. That is exactly the work the deadline exists to skip.
                if crate::batch::job_budget_spent() {
                    return PassExit::JobDeadline;
                }
                if tally.item_cap_reached(self.total_items_optimized) {
                    return PassExit::MaxItems;
                }
                // A promotion earlier in this window may have ripped it out.
                if !board.store().contains(item) {
                    continue;
                }
                match verdicts[index] {
                    // Trusted, and counted exactly as the sequential loop counts
                    // a failed attempt — including towards the consecutive
                    // failure exit, which is what keeps the two drivers' pass
                    // lengths comparable.
                    Verdict::Rejected => {
                        if let Some(exit) = self.record(tally, &ItemRouteResult::not_attempted()) {
                            return exit;
                        }
                    }
                    // Re-run for real: the verdict was measured on a clone of a
                    // board that may already have moved, and only a live attempt
                    // may commit copper.
                    Verdict::Improved | Verdict::Unevaluated => {
                        let result = self.opt_route_item(
                            board,
                            item,
                            pass_settings,
                            with_preferred_directions,
                        );
                        if let Some(exit) = self.record(tally, &result) {
                            return exit;
                        }
                    }
                }
            }
            window_size = parallel::next_window_size(
                window_size,
                threads,
                tally.items_improved > improved_before,
            );
            if exhausted {
                break PassExit::Exhausted;
            }
        }
    }

    /// Everything one item attempt reads, as of right now.
    fn attempt_config<'c>(
        &self,
        pass_settings: &'c RouterSettings,
        with_preferred_directions: bool,
    ) -> AttemptConfig<'c>
    where
        'r: 'c,
    {
        AttemptConfig {
            rules: self.rules,
            pass_settings,
            min_cumulative_trace_length: self.min_cumulative_trace_length,
            length_metric: self.length_metric,
            via_ripup_costs: self.item_ripup_costs(false),
            trace_ripup_costs: self.item_ripup_costs(true),
            max_autoroute_passes: self.settings.optimizer.max_autoroute_passes,
            with_preferred_directions,
        }
    }

    /// `opt_route_item(p_item, p_with_preferred_directions, false)`
    /// (`:308-414`) — rip one item's connection out, re-route it, keep it only
    /// if the board strictly improved.
    ///
    /// The attempt itself is [`attempt::attempt_item`], factored out so a T1
    /// worker can run the identical code on a clone under
    /// [`Disposition::Discard`]. What stays here is the one piece of state the
    /// attempt updates rather than reads: `min_cumulative_trace_length`
    /// (`:397-399`).
    ///
    /// Snapshots are always on: `disableSnapshots = true` is the multi-threaded
    /// optimizer's call, and that path is not ported (conformance #100).
    pub fn opt_route_item(
        &mut self,
        board: &mut Board,
        item: ItemId,
        pass_settings: &RouterSettings,
        with_preferred_directions: bool,
    ) -> ItemRouteResult {
        let cfg = self.attempt_config(pass_settings, with_preferred_directions);
        let result = attempt_item(
            board,
            item,
            &cfg,
            self.rerouter.as_mut(),
            &mut self.scratch,
            Disposition::Keep,
        );
        if result.improved() {
            // `:397-399` — the release's plain `min`, without v19's extra
            // "adopt outright when incompletes or vias fell" branch
            // (conformance #86). Always the *weighted* length, whichever metric
            // the comparison above used.
            self.min_cumulative_trace_length = self
                .min_cumulative_trace_length
                .min(attempt::weighted_trace_length(board));
        }
        result
    }

    /// `:366-374` — the ripup schedule: `100`, `×10` while the surcharge is on,
    /// `×0.6` (rounded) for a trace, "taking less ripup costs seems to produce
    /// better results".
    pub fn item_ripup_costs(&self, item_is_trace: bool) -> i32 {
        let optimizer = &self.settings.optimizer;
        let mut costs = self.settings.scoring.start_ripup_costs();
        if self.use_increased_ripup_costs {
            costs = costs.saturating_mul(optimizer.additional_ripup_cost_factor_at_start);
        }
        if item_is_trace {
            costs =
                (f64::from(optimizer.trace_ripup_cost_factor) * f64::from(costs)).round() as i32;
        }
        costs
    }

    fn score(&self, board: &mut Board) -> PassScore {
        stats::pass_score(
            board,
            self.rules,
            &self.settings.scoring,
            self.board_units_per_mm,
        )
    }
}

/// `BatchAutorouter`'s constructor with `p_with_preferred_directions = false`
/// (`:126-132`): every layer's against-preferred cost is pulled down to its
/// preferred cost, so the maze has no direction bias at all on that pass.
fn without_preferred_directions(settings: &RouterSettings) -> RouterSettings {
    let mut stripped = settings.clone();
    for layer in 0..settings.layer_count() {
        let cost = settings.preferred_direction_trace_costs(layer);
        stripped.set_against_preferred_direction_trace_costs(layer, cost);
    }
    stripped
}

// ---------------------------------------------------------------------------
// the stage
// ---------------------------------------------------------------------------

/// The post-routing optimization stage, as the release sequences it.
///
/// Headless (`management/RoutingJobSchedulerActionThread.thread_action`,
/// `:179-189`):
///
/// ```text
/// if (job.routerSettings.getRunOptimizer()) {
///     BatchOptimizer optimizer = new BatchOptimizer(job);   // ALWAYS single-threaded
///     optimizer.runBatchLoop();
/// }
/// ```
///
/// The gate is [`RouterSettings::run_optimizer`], which is **fail-closed**:
/// `optimizer.enabled == null` reads as *false* (`RouterSettings.java:492-494`),
/// unlike `getRunRouter()` next to it. In practice it is never null, because
/// every job's settings are merged onto `DefaultSettings`, which sets it to
/// `true` — so **the optimizer is on by default** (conformance #6, #7), and
/// `--router.optimizer.enabled=true` changes nothing; only `=false` does.
///
/// Returns `None` when the stage is off. `board_units_per_mm` comes from the
/// imported design; see [`DEFAULT_BOARD_UNITS_PER_MM`].
pub fn run_optimization_stage(
    board: &mut Board,
    rules: &BoardRules,
    settings: &RouterSettings,
    board_units_per_mm: f64,
) -> Option<OptimizerOutcome> {
    run_optimization_stage_with_threads(
        board,
        rules,
        settings,
        board_units_per_mm,
        Threads::single(),
    )
}

/// [`run_optimization_stage`] with a worker budget for T1 candidate evaluation.
///
/// `Threads::single()` is exactly [`run_optimization_stage`]; anything more
/// takes [`parallel`]'s clone-eval-promote path, which is deterministic at a
/// fixed count but is not bit-identical to the sequential stage.
pub fn run_optimization_stage_with_threads(
    board: &mut Board,
    rules: &BoardRules,
    settings: &RouterSettings,
    board_units_per_mm: f64,
    threads: Threads,
) -> Option<OptimizerOutcome> {
    if !settings.run_optimizer() {
        return None;
    }
    let mut optimizer = BatchOptimizer::new(rules, settings).with_threads(threads);
    optimizer.board_units_per_mm = board_units_per_mm;
    Some(optimizer.run_batch_loop(board))
}

/// What [`route_then_optimize`] produced.
#[derive(Debug)]
pub struct StageOutcome {
    /// The router's batch loop.
    pub routing: crate::batch::RunOutcome,
    /// The optimization stage, or `None` when it was switched off.
    pub optimization: Option<OptimizerOutcome>,
}

/// The routing stage followed by the optimization stage — the release's
/// `thread_action()` order, and the only sequencing 2.3.0 has.
///
/// Headless (`management/RoutingJobSchedulerActionThread.thread_action`, `:96`,
/// `:118`, `:179-189`) and GUI
/// (`interactive/AutorouterAndRouteOptimizerThread`, `:451-701`) both run
/// *router batch loop, then optimizer*, strictly sequentially on one thread. The
/// optimizer sees a board on which routing has already finished — which is what
/// makes its "did the board get better" question answerable at all, and why the
/// release calls its gate `getRunOptimizer()` rather than folding the stage into
/// the router's own pass loop.
///
/// Callers that already ran the router (or the fanout stage) should call
/// [`run_optimization_stage`] directly instead.
pub fn route_then_optimize(
    board: &mut Board,
    rules: &BoardRules,
    settings: &RouterSettings,
    max_passes: i32,
    board_units_per_mm: f64,
) -> StageOutcome {
    let mut router = BatchRouter::new(rules, settings);
    router.board_units_per_mm = board_units_per_mm;
    let routing = router.run(board, max_passes);
    // `:496-505` — a router that stopped on `maxItems` calls `requestStop()`
    // over *all* stages, so hitting the item cap also skips the optimizer
    // (conformance #42). Nothing else about the routing result gates it.
    let routing_hit_the_item_cap =
        settings.max_items > 0 && router.total_items_routed() >= settings.max_items;
    let optimization = if routing_hit_the_item_cap {
        None
    } else {
        run_optimization_stage(board, rules, settings, board_units_per_mm)
    };
    StageOutcome {
        routing,
        optimization,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestFixture;

    #[test]
    fn the_ripup_schedule_is_hundred_times_ten_times_zero_point_six() {
        let fixture = TestFixture::two_layer_with_pads();
        let mut optimizer = BatchOptimizer::new(&fixture.rules, &fixture.settings);
        assert_eq!(fixture.settings.scoring.start_ripup_costs(), 100);

        assert!(optimizer.uses_increased_ripup_costs());
        assert_eq!(optimizer.item_ripup_costs(false), 1_000, "a via at start");
        assert_eq!(optimizer.item_ripup_costs(true), 600, "a trace at start");

        optimizer.use_increased_ripup_costs = false;
        assert_eq!(optimizer.item_ripup_costs(false), 100);
        assert_eq!(optimizer.item_ripup_costs(true), 60);
    }

    /// The optimizer's direct-drive path must not requeue: upstream's
    /// `autoroute_passes_for_optimizing_item` re-derives the work list on the
    /// next pass instead (`BatchAutorouter.java:156-161`).
    #[test]
    fn the_optimizers_rerouter_turns_the_requeue_off() {
        let mut fixture = TestFixture::two_layer_with_pads();
        let rules = fixture.rules.clone();
        let settings = fixture.settings.clone();

        // A plain router at `-mp 1` requeues; the seam has to be reachable.
        let mut plain = BatchRouter::new(&rules, &settings);
        plain.autoroute_pass(&mut fixture.board.clone(), 1);
        assert!(
            plain.requeue_active(),
            "the default is upstream's -mp 1 rule"
        );

        // ... and what `MazeRerouter` builds does not.
        let mut router = BatchRouter::new(&rules, &settings);
        router.requeue_policy = crate::batch::schedule::RequeuePolicy::Never;
        router.set_requeue_active(false);
        assert!(!router.requeue_active());
        router.autoroute_pass(&mut fixture.board, 1);
        assert!(
            !router.requeue_active(),
            "a direct `autoroute_pass` must not re-resolve the flag"
        );
    }

    /// The reroute's escalation ladder, term by term — the guard that decides
    /// whether an item pays for a dearer pass.
    #[test]
    fn the_escalation_stops_on_completion_stagnation_or_a_barren_pass() {
        let pass = |routed: usize, not_routed: usize, complete: bool| crate::batch::PassOutcome {
            routed,
            not_routed,
            board_complete: complete,
            progressed: routed > 0 || not_routed > 0,
            // Below every floor the cases below pass, so this test reads the
            // three original terms; the incomplete rule has its own test.
            incomplete_count: 0,
            ..crate::batch::PassOutcome::default()
        };
        let floor = 9;

        // A pass that routed something and left work behind earns the next one.
        assert!(escalation_continues(&pass(1, 3, false), floor));
        // `:437-440` — nothing left to route at all.
        assert!(!escalation_continues(&pass(2, 0, true), floor));
        // `:688` — upstream's own `still_unrouted_items == false`: every queued
        // connection was skipped.
        assert!(!escalation_continues(&pass(0, 0, false), floor));
        // This port's rule: connections remain, none of them moved, so the only
        // difference the next pass would make is a dearer ripup on the same
        // copper that already refused.
        assert!(!escalation_continues(&pass(0, 5, false), floor));
    }

    /// The second of this port's rules: a pass may route as much as it likes and
    /// still not earn the next one, if it left the board with more open
    /// connections than it found. `Issue103-Board-Routed` is where that costs
    /// 116 s an item — see [`MazeRerouter`].
    #[test]
    fn a_pass_that_opened_more_connections_than_it_closed_ends_the_escalation() {
        let pass = |routed: usize, incomplete_count: usize| crate::batch::PassOutcome {
            routed,
            not_routed: 1,
            board_complete: false,
            progressed: true,
            incomplete_count,
            ..crate::batch::PassOutcome::default()
        };

        // Closed one of the nine it was given: the escalation is working.
        assert!(escalation_continues(&pass(1, 8), 9));
        // Busy and net-destructive — the Issue103 shape, `routed > 0` and the
        // backlog grew from 1 to 8.
        assert!(!escalation_continues(&pass(1, 8), 1));
        // Standing still is not progress either; every tie is a stop, as it is
        // in the improvement lattice.
        assert!(!escalation_continues(&pass(3, 9), 9));
    }

    #[test]
    fn stripping_preferred_directions_equalizes_the_two_costs() {
        let fixture = TestFixture::two_layer_with_pads();
        let stripped = without_preferred_directions(&fixture.settings);
        for layer in 0..fixture.settings.layer_count() {
            let preferred = fixture.settings.preferred_direction_trace_costs(layer);
            assert_eq!(stripped.preferred_direction_trace_costs(layer), preferred);
            assert_eq!(
                stripped.against_preferred_direction_trace_costs(layer),
                preferred,
                "layer {layer}"
            );
        }
        // ... and the original board *does* have a bias, or the test is empty.
        let biased = (0..fixture.settings.layer_count()).any(|layer| {
            fixture
                .settings
                .against_preferred_direction_trace_costs(layer)
                > fixture.settings.preferred_direction_trace_costs(layer)
        });
        assert!(biased, "the fixture settings have a preferred direction");
    }

    #[test]
    fn the_stage_is_skipped_when_the_flag_is_off() {
        let mut fixture = TestFixture::two_layer_with_pads();
        let mut settings = fixture.settings.clone();
        settings.optimizer.enabled = Some(false);
        assert!(run_optimization_stage(
            &mut fixture.board,
            &fixture.rules,
            &settings,
            DEFAULT_BOARD_UNITS_PER_MM
        )
        .is_none());
    }

    #[test]
    fn a_null_enabled_flag_reads_as_off() {
        // `getRunOptimizer()` is fail-closed where `getRunRouter()` is
        // fail-open (conformance #6).
        let mut fixture = TestFixture::two_layer_with_pads();
        let mut settings = fixture.settings.clone();
        settings.optimizer.enabled = None;
        assert!(run_optimization_stage(
            &mut fixture.board,
            &fixture.rules,
            &settings,
            DEFAULT_BOARD_UNITS_PER_MM
        )
        .is_none());
        // ... but the shipped default is on.
        assert!(fixture.settings.run_optimizer());
    }
}
