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
//!         stop if score_before * (1 + threshold) >= 1000        # already near-perfect
//!         opt_route_pass(pass, with_preferred_directions = pass is odd)
//!         if use_increased_ripup_costs and score did not rise:
//!               drop to normal ripup costs and keep going
//!         else stop when (score_after - score_before)/score_before < threshold
//!
//! opt_route_pass:
//!     min_cumulative_trace_length := board weighted length
//!     for item in ReadSortedRouteItems:                          # (x, y, layer), vias first
//!         opt_route_item(item)
//!         stop after 50 consecutive items that did not improve
//!
//! opt_route_item:
//!     rip the item's whole connection (plus fork traces at either end)
//!     re-route it with the maze engine at optimizer ripup costs, ≤ 6 passes
//!     keep it iff (incompletes, vias, length) strictly fell; otherwise roll back
//! ```
//!
//! # Only single-threaded
//!
//! `BatchOptimizerMultiThreaded` ships in 2.3.0 but is GUI-only, is reached
//! only when `featureFlags.multiThreading && optimizer.maxThreads > 1`, prints
//! its own authors' "multi-threaded route optimization is broken" warning, and
//! carries two confirmed regressions — its per-task optimizer never sets
//! `minCumulativeTraceLength` (killing the length branch of the improvement
//! rule) and never gets the ×10 start ripup boost (conformance #100). The
//! headless path is *always* single-threaded (#3, #106). This module is the
//! headless path; T1 parallelism is an M3 question.
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

pub mod improvement;
pub mod stats;
pub mod sweep;

use std::collections::BTreeSet;

use fr_board::board::Board;
use fr_board::item::{ItemId, ItemKind};
use fr_rules::board_rules::BoardRules;
use fr_rules::router_settings::RouterSettings;

use crate::batch::{BatchRouter, ContactScratch};
use crate::insert::connection_items;

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
#[derive(Debug, Default)]
pub struct MazeRerouter;

impl ItemRerouter for MazeRerouter {
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
        for pass_no in 1..=request.max_passes {
            let outcome = router.autoroute_pass(board, pass_no);
            if !escalation_continues(&outcome) {
                break;
            }
        }
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
/// Then the added one: `routed > 0`. A pass that routed nothing has not moved
/// the board towards the completion this loop exists to reach, and the only
/// thing the next pass changes is the price of ripping the copper that already
/// refused to move.
fn escalation_continues(outcome: &crate::batch::PassOutcome) -> bool {
    !outcome.board_complete && outcome.progressed && outcome.routed > 0
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

// ---------------------------------------------------------------------------
// the optimizer
// ---------------------------------------------------------------------------

/// The single-threaded batch optimizer — `autoroute/BatchOptimizer.java`.
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
    scratch: ContactScratch,
    ripped_items: BTreeSet<ItemId>,
    ripped_connections: BTreeSet<ItemId>,
    contacts: Vec<ItemId>,
}

impl<'r> BatchOptimizer<'r> {
    /// An optimizer over these rules and settings, re-routing with the maze
    /// engine.
    ///
    /// `settings` must already have been through
    /// [`RouterSettings::apply_board_specific_optimizations`], for the same
    /// reason [`BatchRouter::new`] requires it.
    pub fn new(rules: &'r BoardRules, settings: &'r RouterSettings) -> BatchOptimizer<'r> {
        BatchOptimizer::with_rerouter(rules, settings, Box::new(MazeRerouter))
    }

    /// An optimizer with a caller-supplied [`ItemRerouter`].
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
            scratch: ContactScratch::new(),
            ripped_items: BTreeSet::new(),
            ripped_connections: BTreeSet::new(),
            contacts: Vec::new(),
        }
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

        let mut sweep = SortedRouteItems::new();
        let mut items_attempted = 0;
        let mut items_improved = 0;
        let mut consecutive_failures = 0;
        let mut route_improved = 0.0;
        let exit = loop {
            // `:236-240`.
            if max_items > 0 && self.total_items_optimized >= max_items {
                break PassExit::MaxItems;
            }
            // `:243-246`.
            let Some(item) = sweep.next(board, &mut self.scratch) else {
                break PassExit::Exhausted;
            };
            let result = self.opt_route_item(board, item, pass_settings, with_preferred_directions);
            self.total_items_optimized += 1;
            items_attempted += 1;
            if result.improved() {
                items_improved += 1;
                consecutive_failures = 0;
                // `:265-269`. Assignment, not accumulation: whatever the last
                // improved item scored is what the pass reports.
                route_improved = if key_before.via_count != 0 && key_before.trace_length != 0.0 {
                    1.0 - ((result.via_count() as f64 / key_before.via_count as f64)
                        + (result.trace_length() / key_before.trace_length))
                        / 2.0
                } else {
                    0.0
                };
            } else {
                consecutive_failures += 1;
                // `:257-263`.
                if consecutive_failures >= max_consecutive_failures {
                    break PassExit::ConsecutiveFailures;
                }
            }
        };

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

    /// `opt_route_item(p_item, p_with_preferred_directions, false)`
    /// (`:308-414`) — rip one item's connection out, re-route it, keep it only
    /// if the board strictly improved.
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
        // `:315-321` — measured *before* the ripout.
        let key_before = stats::board_key(board);

        let Some(item_ref) = board.store().get(item) else {
            return ItemRouteResult::not_attempted();
        };
        let nets: Vec<i32> = item_ref.nets.iter().copied().collect();
        let trace_corners = match &item_ref.kind {
            ItemKind::Trace(trace) => Some((trace.first_corner(), trace.last_corner())),
            _ => None,
        };
        let is_trace = trace_corners.is_some();

        // `:327-340` — step 1. The sweep matches connection *endpoints*, so a
        // fork hanging off either end of this trace is not enumerated on its
        // own; rip it with us, but only when the whole contact group is
        // unfixed trace, or we would be promising to restore copper we are not
        // allowed to touch.
        let mut ripped_items = std::mem::take(&mut self.ripped_items);
        ripped_items.clear();
        ripped_items.insert(item);
        if let Some((first, last)) = trace_corners {
            for corner in [first, last] {
                self.contacts.clear();
                board.normal_contacts_at(item, &corner, &mut self.contacts);
                if contains_only_unfixed_traces(board, &self.contacts) {
                    ripped_items.extend(self.contacts.iter().copied());
                }
            }
        }

        // `:342-346` — step 2, `StopConnectionOption.NONE`: the entire
        // electrical connection, fanout vias included.
        let mut ripped_connections = std::mem::take(&mut self.ripped_connections);
        ripped_connections.clear();
        for member in ripped_items.iter().copied() {
            ripped_connections.extend(connection_items(board, member, false));
        }

        // `:348-354` — step 3. One user-fixed member and the whole item is
        // skipped: a partial ripout would leave the connection unroutable.
        let blocked = ripped_connections.iter().copied().any(|id| {
            board
                .store()
                .get(id)
                .is_some_and(fr_board::item::Item::is_user_fixed)
        });
        if blocked {
            self.ripped_items = ripped_items;
            self.ripped_connections = ripped_connections;
            return ItemRouteResult::not_attempted();
        }

        // `:356-357` — step 4. The undo point.
        let mark = board.begin();

        // `:360-363` — `remove_items` skips what it may not delete rather than
        // failing (`BasicBoard.java:722-733`); `remove_item` already makes the
        // `isDeletionForbidden` half of that check.
        for id in ripped_connections.iter().copied() {
            board.remove_item(id);
        }
        for net_no in nets.iter().copied() {
            board.combine_traces(net_no);
        }

        let request = RerouteRequest {
            ripup_costs: self.item_ripup_costs(is_trace),
            max_passes: self.settings.optimizer.max_autoroute_passes,
            with_preferred_directions,
        };
        // `:376-378`.
        self.rerouter
            .reroute(board, self.rules, pass_settings, &request);

        // `:381-393`.
        let key_after = stats::board_key(board);
        let result = ItemRouteResult::new(
            &key_before,
            &key_after,
            self.min_cumulative_trace_length,
            self.length_metric,
        );

        if result.improved() {
            // `:397-399` — the release's plain `min`, without v19's extra
            // "adopt outright when incompletes or vias fell" branch
            // (conformance #86). Always the *weighted* length, whichever
            // metric the comparison above used.
            self.min_cumulative_trace_length = self
                .min_cumulative_trace_length
                .min(key_after.weighted_trace_length);
            board.commit(mark);
        } else {
            // `:405-411`.
            board.rollback_to(mark);
        }

        self.ripped_items = ripped_items;
        self.ripped_connections = ripped_connections;
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

/// `contains_only_unfixed_traces` (`:54-61`). An empty group is vacuously true,
/// as upstream's loop over an empty collection is.
fn contains_only_unfixed_traces(board: &Board, items: &[ItemId]) -> bool {
    items.iter().copied().all(|id| {
        board
            .store()
            .get(id)
            .is_some_and(|item| !item.is_user_fixed() && matches!(item.kind, ItemKind::Trace(_)))
    })
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
    if !settings.run_optimizer() {
        return None;
    }
    let mut optimizer = BatchOptimizer::new(rules, settings);
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
        assert!(plain.requeue_active(), "the default is upstream's -mp 1 rule");

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
            ..crate::batch::PassOutcome::default()
        };

        // A pass that routed something and left work behind earns the next one.
        assert!(escalation_continues(&pass(1, 3, false)));
        // `:437-440` — nothing left to route at all.
        assert!(!escalation_continues(&pass(2, 0, true)));
        // `:688` — upstream's own `still_unrouted_items == false`: every queued
        // connection was skipped.
        assert!(!escalation_continues(&pass(0, 0, false)));
        // This port's rule: connections remain, none of them moved, so the only
        // difference the next pass would make is a dearer ripup on the same
        // copper that already refused.
        assert!(!escalation_continues(&pass(0, 5, false)));
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
