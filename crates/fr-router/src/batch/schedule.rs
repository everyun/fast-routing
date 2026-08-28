//! The batch loop: pass scheduling and every stop rule.
//!
//! Transcribed from the 2.3.0 release `autoroute/BatchAutorouter.runBatchLoop`
//! (`/tmp/fr-java-release/.../autoroute/BatchAutorouter.java:757-1188`), which
//! the conformance appendix names as "the single authority; constants at
//! `:52-71`". The `autoroute/pipeline/AutorouteBatchLoop` the spec body
//! describes is not in the jar.
//!
//! | this module | upstream |
//! |---|---|
//! | [`BatchLoop::run`] | `runBatchLoop` (`:757-1188`), pass loop `:902-1136` |
//! | [`STOP_AT_PASS_MINIMUM`] … | the constants at `:52-71` *(jar-verified)* |
//! | [`PassDriver::run_pass`] | `autoroute_pass` (`:431-694`) |
//! | [`PassDriver::remove_tails`] | `remove_tails` (`:1247-1252`) |
//! | [`BoardSnapshotStore`] | `autoroute/BoardHistory.java` |
//!
//! # The shape of the loop
//!
//! ```text
//! score the starting board
//! while continuing and not stopped:
//!     job budget spent                       -> stop                     (ours)
//!     pass > max_passes                      -> stop                     (:931)
//!     history.add(pre-pass board)                                        (:944)
//!     continuing = driver.run_pass(pass)                                 (:947)
//!     score the board
//!     job budget spent during the pass       -> stop                     (ours)
//!     restore check, on passes >= 8 that are 0 mod 4                     (:953-995)
//!     stagnation, on passes >= 8 while still routing                     (:1054-1130)
//!         pass-local counter (reset by a restore)
//!             +3 flat passes with fanout -> one-time tail recovery
//!             +10 flat passes            -> stop
//!         global counter (immune to restores)
//!             10 passes since the best score -> stop
//!     pass++
//! restore the best board ever seen if it beats the final one             (:1141-1159)
//! final tail cleanup when the board completed without fanout vias        (:1163-1168)
//! ```
//!
//! # Two things that read like bugs and are the contract
//!
//! * The restore test is **strict** `>` (`:961`). Upstream's own comment records
//!   why: with `>=`, a board whose every state scores the same restores on every
//!   check cycle, growing the history unboundedly and never stopping.
//! * The rank check (`:968`) is **dead code**: ranks run `1..=len` and the cap is
//!   `BOARD_RANK_LIMIT == MAX_HISTORY_SIZE == 30`, so `rank > 30` cannot happen
//!   (conformance appendix row 60). It is transcribed because it is a stop rule
//!   the appendix enumerates, and because a smaller
//!   [`LoopConfig::history_limit`] makes it reachable here.
//!
//! # Divergences
//!
//! Two, both in `docs/PARITY-DIVERGENCES.md`.
//!
//! [`RequeuePolicy`] is the in-pass re-attempt of a connection whose copper a
//! later connection ripped. Upstream never retries inside a pass (§4.1); we do
//! it only when there is no next pass to rely on, i.e. at `-mp 1`.
//!
//! [`JobDeadline`] is a wall budget for the whole job, off unless
//! [`arm_job_deadline`] is called. It is checked between passes here and
//! between connections in
//! [`BatchRouter::autoroute_pass_with`](crate::batch::BatchRouter::autoroute_pass_with),
//! and when it is gone the loop leaves through the *same* tail as every
//! upstream stop rule — so the run still ends on the best board it ever held
//! and the caller still gets a session. It exists because a harness cap that
//! `SIGKILL`s the process produces nothing at all, which is strictly less than
//! the board we were holding.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use fr_board::board::Board;
use fr_board::store::StoreDelta;

use crate::batch::budget::{BudgetMode, BudgetPolicy, ConnectionBudget};
use crate::batch::history::{
    BoardSnapshotStore, BOARD_RANK_LIMIT, MAXIMUM_TRIES_ON_THE_SAME_BOARD,
};
use crate::batch::scoring::BoardScorer;
use crate::batch::{PassOutcome, StopConnection, START_RIPUP_COSTS};

/// `BatchAutorouter.STOP_AT_PASS_MINIMUM` (`:60`) *(jar-verified)* — no restore
/// or stagnation check may fire before this pass.
pub const STOP_AT_PASS_MINIMUM: i32 = 8;

/// `BatchAutorouter.STOP_AT_PASS_MODULO` (`:63`) *(jar-verified)* — the restore
/// check runs on passes `≡ 0 (mod 4)`.
pub const STOP_AT_PASS_MODULO: i32 = 4;

/// `BatchAutorouter.STAGNATION_PASS_LIMIT` (`:66`) *(jar-verified)* —
/// consecutive flat passes before the router gives up.
pub const STAGNATION_PASS_LIMIT: i32 = 10;

/// `BatchAutorouter.FANOUT_RECOVERY_STAGNATION_PASSES` (`:68`) *(jar-verified)*
/// — flat passes before the one-time fanout-tail cleanup.
pub const FANOUT_RECOVERY_STAGNATION_PASSES: i32 = 3;

/// `BatchAutorouter.STAGNATION_SCORE_THRESHOLD` (`:71`) *(jar-verified)* — the
/// score gain, on the 0..1000 scale, that counts as an improvement.
///
/// Upstream's is a `float`; the comparisons it takes part in are `float`
/// comparisons of a `float` score. Ours are `f64` throughout. The difference can
/// only matter for a gain within one float ULP of 0.5 — and per ARCHITECTURE
/// ("f64 never decides topology") this decides *when to stop*, never *what to
/// route*.
pub const STAGNATION_SCORE_THRESHOLD: f64 = 0.5;

/// `settings.maxPasses`'s default (`DefaultSettings.java:95`) *(jar-verified)*.
pub const DEFAULT_MAX_PASSES: i32 = 9_999;

// ---------------------------------------------------------------------------
// the job deadline
// ---------------------------------------------------------------------------

/// The process's job deadline, armed once by [`arm_job_deadline`].
static JOB_DEADLINE: OnceLock<Instant> = OnceLock::new();

/// Set by [`request_job_stop`] — the signal handler's half of the deadline.
static JOB_STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Arms the process-wide job deadline at `now + after`. Returns `false` when
/// one was already armed, which never overwrites the first.
///
/// This is the port's equivalent of the release's job timeout
/// (`RoutingJobScheduler`'s per-job budget): when it expires the batch loop
/// stops **exactly like a stop rule** — best board restored, referee run,
/// session written — instead of the caller killing a process that is holding a
/// routed board it never wrote.
///
/// `after == 0` arms a deadline that is already expired, which is the cheapest
/// end-to-end test of the whole path.
pub fn arm_job_deadline(after: Duration) -> bool {
    JOB_DEADLINE
        .set(
            Instant::now()
                .checked_add(after)
                .unwrap_or_else(Instant::now),
        )
        .is_ok()
}

/// Asks every polling loop to stop at its next check.
///
/// **Async-signal-safe**: one relaxed store and nothing else, so a `SIGTERM` /
/// `SIGINT` handler may call it.
pub fn request_job_stop() {
    JOB_STOP_REQUESTED.store(true, Ordering::Relaxed);
}

/// Whether [`request_job_stop`] has been called.
pub fn job_stop_requested() -> bool {
    JOB_STOP_REQUESTED.load(Ordering::Relaxed)
}

/// Whether the ambient job budget is gone — the armed deadline has passed, or a
/// stop was requested.
pub fn job_budget_spent() -> bool {
    JobDeadline::ambient().expired()
}

/// Why a run was cut short from outside the router.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopCause {
    /// The wall budget ran out.
    Deadline,
    /// A signal handler called [`request_job_stop`].
    Interrupt,
}

/// A wall budget for the whole job, polled between connections and between
/// passes.
///
/// Deliberately **not** a second per-connection budget:
/// [`ConnectionBudget`] is upstream's
/// `TimeLimit` and governs *one* connection, while this governs the run. The
/// two meet in [`PassContext::connection_budget`], which never hands a
/// connection more time than the job has left.
#[derive(Debug, Clone, Copy, Default)]
pub struct JobDeadline {
    at: Option<Instant>,
    /// Whether [`job_stop_requested`] also counts as expiry. Only the ambient
    /// deadline watches the flag; an explicit one is a pure clock, so a test
    /// can never be disturbed by a signal handler another test installed.
    watch_stop_flag: bool,
}

/// `Instant` is not `Eq`-comparable through a derive on an `Option`? It is —
/// but the flag makes the derived equality misleading, so it is spelled out.
impl PartialEq for JobDeadline {
    fn eq(&self, other: &Self) -> bool {
        self.at == other.at && self.watch_stop_flag == other.watch_stop_flag
    }
}

impl Eq for JobDeadline {}

impl JobDeadline {
    /// No budget at all — the default, and what every run gets unless a caller
    /// armed one.
    pub const NONE: JobDeadline = JobDeadline {
        at: None,
        watch_stop_flag: false,
    };

    /// The process-wide deadline set by [`arm_job_deadline`], which also
    /// watches the [`request_job_stop`] flag.
    ///
    /// This is what [`LoopConfig::default`] and [`PassContext::new`] carry, so
    /// *every* driver — the batch loop, and the optimizer's per-item
    /// [`BatchRouter::autoroute_pass`](crate::batch::BatchRouter::autoroute_pass)
    /// loop, which never goes through [`BatchLoop`] — honours it without being
    /// told.
    pub fn ambient() -> JobDeadline {
        JobDeadline {
            at: JOB_DEADLINE.get().copied(),
            watch_stop_flag: true,
        }
    }

    /// A deadline `after` from now, ignoring the stop flag. For tests and for
    /// callers that own their own budget.
    pub fn after(after: Duration) -> JobDeadline {
        JobDeadline {
            at: Instant::now().checked_add(after),
            watch_stop_flag: false,
        }
    }

    /// A deadline that has already passed — the cheapest way to test the
    /// stop path.
    pub fn already_passed() -> JobDeadline {
        JobDeadline {
            at: Some(Instant::now()),
            watch_stop_flag: false,
        }
    }

    /// Whether a budget was armed at all. A run with none never reads the
    /// clock.
    pub fn is_armed(&self) -> bool {
        self.at.is_some() || self.watch_stop_flag
    }

    /// Why the budget is gone, or `None` while it lasts.
    ///
    /// The stop flag is checked first: an operator's `SIGTERM` is the more
    /// informative of the two when both are true.
    pub fn cause(&self) -> Option<StopCause> {
        if self.watch_stop_flag && job_stop_requested() {
            return Some(StopCause::Interrupt);
        }
        match self.at {
            Some(at) if Instant::now() >= at => Some(StopCause::Deadline),
            _ => None,
        }
    }

    /// Whether the run must stop now.
    pub fn expired(&self) -> bool {
        self.cause().is_some()
    }

    /// Time left on the clock; `None` when no deadline was armed. Zero once it
    /// has passed.
    pub fn remaining(&self) -> Option<Duration> {
        let at = self.at?;
        Some(at.saturating_duration_since(Instant::now()))
    }
}

/// When a connection whose copper was ripped is re-attempted inside the same
/// pass.
///
/// **A declared divergence** (`docs/PARITY-DIVERGENCES.md`, "Ripped connections
/// are requeued inside the pass"). Upstream has *no* retry inside a pass
/// (`batch-orchestration.md` §4.1): a failed or ripped-out connection waits for
/// the next pass, which re-derives the work list from scratch and retries at
/// `start_ripup_costs · pass_no` with twice the time budget.
///
/// That is the right behaviour whenever there *is* a next pass. At `-mp 1` there
/// is not, and a ripup is then a pure loss: the router trades a routed
/// connection for an unrouted one and reports success. So the switch is
/// explicit, and its default resolves on the pass count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RequeuePolicy {
    /// Requeue only when a single pass was asked for — **the default**.
    ///
    /// `-mp 1` requeues; `-mp >= 2` runs upstream's no-retry-inside-a-pass rule,
    /// so a multi-pass run is a faithful transcription and the intermediate
    /// boards match the release's.
    #[default]
    SinglePassOnly,
    /// Never requeue: upstream's behaviour at every pass count.
    Never,
    /// Always requeue, whatever the pass count. Diverges from upstream on every
    /// multi-pass run; kept for A/B measurement of the divergence.
    Always,
}

impl RequeuePolicy {
    /// Whether a run of `max_passes` passes requeues.
    pub fn applies_to(self, max_passes: i32) -> bool {
        match self {
            RequeuePolicy::Never => false,
            RequeuePolicy::Always => true,
            RequeuePolicy::SinglePassOnly => max_passes == 1,
        }
    }
}

/// What one pass is told before it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PassContext {
    /// 1-based pass number.
    pub pass_no: i32,
    /// `start_ripup_costs · pass_no` (`:1280`) — the pass's ripup budget.
    pub ripup_costs: i32,
    /// The per-connection allowance, `min(100 s · 2^(pass−1), i32::MAX ms)`
    /// (`:1311-1313`).
    pub budget: ConnectionBudget,
    /// Whether the end-of-pass tail cleanup may delete fanout vias
    /// (`:657-661`).
    pub remove_unconnected_vias: bool,
    /// The job's wall budget, polled between the pass's connections.
    /// [`JobDeadline::NONE`] unless one was armed.
    pub deadline: JobDeadline,
}

impl PassContext {
    /// The context of pass `pass_no` under these settings, under the ambient
    /// job deadline ([`JobDeadline::ambient`]).
    pub fn new(
        pass_no: i32,
        start_ripup_costs: i32,
        policy: &BudgetPolicy,
        remove_unconnected_vias: bool,
    ) -> PassContext {
        PassContext {
            pass_no,
            ripup_costs: start_ripup_costs.saturating_mul(pass_no),
            budget: policy.for_pass(pass_no),
            remove_unconnected_vias,
            deadline: JobDeadline::ambient(),
        }
    }

    /// The same context under an explicit job budget — what [`BatchLoop`]
    /// hands its driver, and what a test uses to avoid the ambient one.
    pub fn with_deadline(mut self, deadline: JobDeadline) -> PassContext {
        self.deadline = deadline;
        self
    }

    /// The allowance to arm the *next* connection with: upstream's
    /// per-connection budget, never longer than the job has left.
    ///
    /// Without the clamp a single connection may overrun the job deadline by
    /// its whole allowance — 100 s on pass 1, doubling — which is exactly the
    /// case the deadline exists to prevent. The clamp is skipped in
    /// [`BudgetMode::Deterministic`], where by construction nothing reads the
    /// wall clock (gate G2.2); a deterministic run that wants a wall budget is
    /// a contradiction, and the between-connections poll still stops it.
    pub fn connection_budget(&self) -> ConnectionBudget {
        if self.budget.mode() != BudgetMode::Wall {
            return self.budget;
        }
        let Some(remaining) = self.deadline.remaining() else {
            return self.budget;
        };
        let remaining_ms = u64::try_from(remaining.as_millis()).unwrap_or(u64::MAX);
        if remaining_ms >= self.budget.limit_ms() {
            return self.budget;
        }
        // Rebuilt through the policy rather than mutated, because
        // `ConnectionBudget`'s fields are the budget module's business;
        // `for_pass(1)` is the identity on the base allowance.
        BudgetPolicy {
            mode: BudgetMode::Wall,
            base_connection_ms: remaining_ms,
            effort_ceiling: self.budget.effort_ceiling(),
            ..BudgetPolicy::default()
        }
        .for_pass(1)
    }
}

/// What the loop drives: one pass, the tail cleanup, and the restore callback.
///
/// [`BatchRouter`](crate::batch::BatchRouter) is the production implementation.
/// The trait exists so the scheduler can be tested against a scripted pass
/// sequence: every stop rule is a function of the (score, incompletes,
/// progressed) triple a pass reports, and nothing about them needs a maze.
pub trait PassDriver {
    /// One pass — `BatchAutorouter.autoroute_pass` (`:431-694`).
    fn run_pass(&mut self, board: &mut Board, ctx: &PassContext) -> PassOutcome;

    /// `remove_tails(stop)` (`:1247-1252`), used by the fanout recovery and the
    /// final cleanup.
    fn remove_tails(&mut self, board: &mut Board, stop: StopConnection);

    /// Dependent-state invalidation after a board restore.
    ///
    /// `ARCHITECTURE.md`: *"restore must also restore tree state and preserves
    /// the room DB interaction (restore invalidates per-item room caches —
    /// budgeted)"*. The board that arrives has its own trees and connectivity
    /// already; `delta` says which items moved, so anything the router caches
    /// *outside* the board — the autoroute room database, per-item autoroute
    /// info — can be invalidated per item instead of wholesale. The default is
    /// a no-op because a driver that builds its engine per connection has
    /// nothing to invalidate.
    fn on_board_restored(&mut self, _board: &Board, _delta: &StoreDelta) {}
}

/// Why the loop stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// `autoroute_pass` found no work: the board is complete (`:437-440`).
    BoardComplete,
    /// A pass attempted nothing — `routed == 0 && not_routed == 0` (`:688`).
    NoProgress,
    /// `currentPass > maxPasses` (`:931`).
    MaxPasses,
    /// A pass stopped on `settings.maxItems` (`:496-505`), which upstream turns
    /// into a stop request over every stage.
    ItemLimit,
    /// The restore check found no history board with tries left (`:963-967`).
    NoBetterBoard,
    /// The board to restore ranked past [`BOARD_RANK_LIMIT`] (`:968-972`) —
    /// unreachable at the upstream cap; see the module docs.
    RankLimit,
    /// Ten consecutive passes without a `> 0.5` gain (`:1085-1097`).
    PassLocalStagnation,
    /// Ten passes since the best score anywhere (`:1108-1121`).
    GlobalStagnation,
    /// The job's wall budget ran out ([`JobDeadline`]). Not an upstream stop
    /// rule — it is the port's answer to being killed at a harness cap while
    /// holding a routed board — but it *behaves* like one: the loop leaves
    /// through the same "restore the best board ever seen" tail, so the run
    /// still ends on the best board it held.
    JobDeadline,
    /// A signal handler called [`request_job_stop`]. Same path as
    /// [`StopReason::JobDeadline`].
    Interrupted,
}

/// The knobs of one batch run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoopConfig {
    /// Pass cap. `<= 0` is upstream's uncapped loop (`:931` skips the check),
    /// which only the stop rules then end — see the CLI divergence in
    /// `docs/PARITY-DIVERGENCES.md`.
    pub max_passes: i32,
    /// `start_ripup_costs`, default 100 (`DefaultSettings.java:147`).
    pub start_ripup_costs: i32,
    /// Whether the fanout stage ran, gating the one-time tail recovery
    /// (`:1073`).
    ///
    /// Upstream reads `settings.isFanoutEnabled()`, which for it is the same
    /// thing because fanout always runs when enabled. This port runs fanout as
    /// a separate opt-in stage, so the honest gate is "did fanout run".
    pub fanout_enabled: bool,
    /// `remove_unconnected_vias` (`:657-661`, `:1165`).
    pub remove_unconnected_vias: bool,
    /// [`BoardSnapshotStore`] cap; [`MAX_HISTORY_SIZE`](crate::batch::history::MAX_HISTORY_SIZE)
    /// upstream.
    pub history_limit: usize,
    /// The per-connection budget policy.
    pub budget: BudgetPolicy,
    /// The job's wall budget. Defaults to [`JobDeadline::ambient`], so a
    /// process that armed one gets it everywhere without threading it through
    /// every constructor; [`JobDeadline::NONE`] opts out.
    pub deadline: JobDeadline,
}

impl Default for LoopConfig {
    fn default() -> Self {
        LoopConfig {
            max_passes: DEFAULT_MAX_PASSES,
            start_ripup_costs: START_RIPUP_COSTS,
            fanout_enabled: false,
            remove_unconnected_vias: false,
            history_limit: crate::batch::history::MAX_HISTORY_SIZE,
            budget: BudgetPolicy::default(),
            deadline: JobDeadline::ambient(),
        }
    }
}

/// Everything a batch run produced.
#[derive(Debug, Clone, Default)]
pub struct RunOutcome {
    /// One entry per pass actually run.
    pub passes: Vec<PassOutcome>,
    /// Ratsnest incompletes when the run stopped, as the referee counts them.
    pub incomplete_count: usize,
    /// Connections routed over the whole run.
    pub inserted_count: usize,
    /// The final board's score.
    pub final_score: f64,
    /// The best score any retained board reached — the loop guarantees
    /// `final_score >= best_history_score` (`:1141-1159`).
    pub best_history_score: f64,
    /// Why the loop stopped; `None` only when it never entered.
    pub stop_reason: Option<StopReason>,
    /// Board restores performed by the pass ≥ 8 check.
    pub restores: usize,
    /// Whether the loop ended by restoring the best board ever seen.
    pub best_board_restored: bool,
    /// Why the job budget cut the run short, or `None` when it did not.
    pub stop_cause: Option<StopCause>,
    /// Whether the one-time fanout-tail recovery fired.
    pub fanout_recovery_applied: bool,
    /// Deepest the snapshot store got.
    pub max_history_depth: usize,
}

impl RunOutcome {
    /// Nothing left unconnected.
    pub fn is_complete(&self) -> bool {
        self.incomplete_count == 0
    }
}

/// The pass scheduler.
#[derive(Debug)]
pub struct BatchLoop {
    config: LoopConfig,
    history: BoardSnapshotStore,
}

impl BatchLoop {
    /// A loop with this configuration.
    pub fn new(config: LoopConfig) -> BatchLoop {
        let history = BoardSnapshotStore::with_capacity(config.history_limit);
        BatchLoop { config, history }
    }

    /// The configuration.
    pub fn config(&self) -> &LoopConfig {
        &self.config
    }

    /// The snapshot store, for diagnostics between runs.
    pub fn history(&self) -> &BoardSnapshotStore {
        &self.history
    }

    /// `BatchAutorouter.runBatchLoop`'s pass loop and stop machinery
    /// (`:902-1188`).
    pub fn run(
        &mut self,
        board: &mut Board,
        driver: &mut impl PassDriver,
        scorer: &mut impl BoardScorer,
    ) -> RunOutcome {
        let mut outcome = RunOutcome::default();
        self.history.clear();

        // `:936-943` — the loop state. `lastBestScore` is pass-local and reset
        // by a restore; `globalBestScore` is not (`:1100-1121`).
        let mut current_pass = 1;
        let mut consecutive_no_improvement = 0;
        let mut fanout_recovery_applied = false;
        let mut last_best_score = f64::NEG_INFINITY;
        let mut global_best_score = f64::NEG_INFINITY;
        let mut pass_of_best_score = 0;
        let mut continue_autorouting = true;
        let mut stop_requested = false;

        // The pre-pass score of pass 1. Every later pass reuses the score it
        // already computed after the previous pass — the same board, so the same
        // number (see `scoring`'s "Cost" note).
        let mut current_score = scorer.score(board);

        while continue_autorouting && !stop_requested {
            // The job budget, checked **between passes**. It is not one of
            // upstream's rules, so it is checked before them: whatever the pass
            // loop would have done next, it costs at least one more pass, and
            // the whole point is to leave enough of the budget to judge and
            // write the board we already have.
            if let Some(cause) = self.config.deadline.cause() {
                outcome.stop_cause = Some(cause);
                outcome.stop_reason = Some(match cause {
                    StopCause::Deadline => StopReason::JobDeadline,
                    StopCause::Interrupt => StopReason::Interrupted,
                });
                break;
            }
            // `:931` — the cap is skipped entirely when `maxPasses <= 0`.
            if self.config.max_passes > 0 && current_pass > self.config.max_passes {
                // `:932` requests the stop *before* breaking, which is what
                // keeps the final tail cleanup at `:1165` from running on a
                // board that only stopped because it ran out of passes.
                stop_requested = true;
                outcome.stop_reason = Some(StopReason::MaxPasses);
                break;
            }

            // `:944` — snapshot the pre-pass board.
            self.history.add(board, current_score.normalized);
            outcome.max_history_depth = outcome.max_history_depth.max(self.history.len());

            let ctx = PassContext::new(
                current_pass,
                self.config.start_ripup_costs,
                &self.config.budget,
                self.config.remove_unconnected_vias,
            )
            .with_deadline(self.config.deadline);
            let pass = driver.run_pass(board, &ctx);
            // `:947` + `:688` — `autoroute_pass` returns false on an empty work
            // list (the board is complete) and on a pass that attempted nothing.
            continue_autorouting = !pass.board_complete && pass.progressed;
            let stop_reason_if_done = if pass.board_complete {
                StopReason::BoardComplete
            } else {
                StopReason::NoProgress
            };
            if pass.hit_item_cap {
                // `:496-505` — the pass requested a stop of *every* stage. The
                // restore check below reads that as "due", exactly as upstream's
                // `is_stop_auto_router_requested()` branch does, so the run
                // still ends on the best board it has.
                stop_requested = true;
                outcome.stop_reason = Some(StopReason::ItemLimit);
            }
            outcome.inserted_count += pass.routed;
            outcome.passes.push(pass);

            current_score = scorer.score(board);

            // The pass polls the same budget between its connections and
            // returns early when it runs out, so a pass that ended that way is
            // recorded here rather than one iteration later — the loop's own
            // check would otherwise attribute it to the *next* pass that never
            // ran. Leaving through `break` keeps the closing "restore the best
            // board ever seen" below, which is what makes the deadline a stop
            // rule and not a crash.
            if let Some(cause) = self.config.deadline.cause() {
                outcome.stop_cause = Some(cause);
                outcome.stop_reason = Some(match cause {
                    StopCause::Deadline => StopReason::JobDeadline,
                    StopCause::Interrupt => StopReason::Interrupted,
                });
                break;
            }

            // --- board restore check (`:953-995`) -------------------------
            //
            // The outer guard is on the *history size*, the inner on the pass
            // number (conformance appendix row 59 — the spec body omits the
            // outer one).
            if self.history.len() >= STOP_AT_PASS_MINIMUM as usize || stop_requested {
                let due = (current_pass % STOP_AT_PASS_MODULO == 0
                    && current_pass >= STOP_AT_PASS_MINIMUM)
                    || stop_requested;
                if due && self.history.max_score() > current_score.normalized {
                    let restored = self.history.restore(
                        board,
                        MAXIMUM_TRIES_ON_THE_SAME_BOARD,
                        |restored_board, delta| driver.on_board_restored(restored_board, delta),
                    );
                    match restored {
                        // `:963-967` — every retained board is out of tries.
                        None => {
                            outcome.stop_reason = Some(StopReason::NoBetterBoard);
                            stop_requested = true;
                            break;
                        }
                        Some(restored) if restored.rank > BOARD_RANK_LIMIT => {
                            outcome.stop_reason = Some(StopReason::RankLimit);
                            stop_requested = true;
                            break;
                        }
                        Some(_) => {
                            // `:976-993` — a restore resets the pass-local
                            // stagnation state (never the global tracker).
                            outcome.restores += 1;
                            consecutive_no_improvement = 0;
                            current_score = scorer.score(board);
                            last_best_score = current_score.normalized;
                        }
                    }
                }
            }

            // --- stagnation (`:1054-1130`) ---------------------------------
            if current_pass >= STOP_AT_PASS_MINIMUM && continue_autorouting {
                // Pass-local counter.
                if current_score.normalized > last_best_score + STAGNATION_SCORE_THRESHOLD {
                    consecutive_no_improvement = 0;
                    last_best_score = current_score.normalized;
                } else {
                    consecutive_no_improvement += 1;

                    // `:1067-1082` — the one-time fanout-tail recovery.
                    if self.config.fanout_enabled
                        && !fanout_recovery_applied
                        && current_score.incomplete_count > 0
                        && consecutive_no_improvement >= FANOUT_RECOVERY_STAGNATION_PASSES
                    {
                        driver.remove_tails(board, StopConnection::None);
                        current_score = scorer.score(board);
                        last_best_score = current_score.normalized;
                        consecutive_no_improvement = 0;
                        fanout_recovery_applied = true;
                        outcome.fanout_recovery_applied = true;
                    }

                    if consecutive_no_improvement >= STAGNATION_PASS_LIMIT {
                        outcome.stop_reason = Some(StopReason::PassLocalStagnation);
                        stop_requested = true;
                        break;
                    }
                }

                // Global tracker, immune to restores (`:1100-1121`).
                if current_score.normalized > global_best_score + STAGNATION_SCORE_THRESHOLD {
                    global_best_score = current_score.normalized;
                    pass_of_best_score = current_pass;
                } else if current_pass - pass_of_best_score >= STAGNATION_PASS_LIMIT {
                    outcome.stop_reason = Some(StopReason::GlobalStagnation);
                    stop_requested = true;
                    break;
                }
            } else if current_score.incomplete_count == 0
                && current_score.normalized > STAGNATION_SCORE_THRESHOLD
            {
                // `:1123-1130` — a fully routed board resets the counter, but
                // only when its score is positive: a fully routed board scoring
                // 0 (clearance violations from plane routing) must keep
                // accumulating until the global tracker fires.
                consecutive_no_improvement = 0;
                last_best_score = current_score.normalized;
            }

            if !continue_autorouting && outcome.stop_reason.is_none() {
                outcome.stop_reason = Some(stop_reason_if_done);
            }
            // `:1132-1135`.
            if continue_autorouting && !stop_requested {
                current_pass += 1;
            }
        }

        // `:1141-1159` — finish with the best board ever seen. The history holds
        // pre-pass boards, so this is what recovers a pass that made things
        // worse and then stopped.
        outcome.best_history_score = self.history.max_score();
        if outcome.best_history_score > current_score.normalized {
            let restored = self
                .history
                .restore_best(board, |restored_board, delta| {
                    driver.on_board_restored(restored_board, delta)
                })
                .is_some();
            if restored {
                outcome.best_board_restored = true;
                current_score = scorer.score(board);
            }
        }

        // `:1163-1168` — clean the route up when the board completed and fanout
        // vias are being kept.
        if !(self.config.remove_unconnected_vias || continue_autorouting || stop_requested) {
            driver.remove_tails(board, StopConnection::None);
            current_score = scorer.score(board);
        }

        // `:1170` — `bh.clear()`. The snapshots are the run's peak memory and
        // nothing downstream reads them.
        self.history.clear();

        outcome.final_score = current_score.normalized;
        outcome.incomplete_count = current_score.incomplete_count;
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::batch::scoring::BoardScore;
    use crate::test_support::TestFixture;
    use fr_board::item::FixedState;
    use fr_board::net_nos::NetNos;
    use fr_geometry::planar::IntPoint;

    /// The scripted board state: a pass sets the board's stub-trace count, and
    /// the scorer reads the score straight back off it.
    ///
    /// Encoding the score *in the board* rather than in the driver is what makes
    /// these tests cover the restore path honestly: a restore puts items back,
    /// so it puts the score back, exactly as it does on a real board.
    const SCORE_PER_STUB: f64 = 1.0;

    #[derive(Debug, Clone, Copy)]
    struct Script {
        /// Stub count after the pass, i.e. the pass's score.
        stubs: usize,
        /// Replace one stub with a fresh one, so the board changes without the
        /// score changing. See [`ScriptedDriver::set_stubs`].
        churn: bool,
        routed: usize,
        not_routed: usize,
        board_complete: bool,
    }

    impl Script {
        fn scoring(stubs: usize) -> Script {
            Script {
                stubs,
                churn: false,
                routed: 1,
                not_routed: 0,
                board_complete: false,
            }
        }

        fn churning(stubs: usize) -> Script {
            Script {
                churn: true,
                ..Script::scoring(stubs)
            }
        }
    }

    struct ScriptedDriver {
        script: Vec<Script>,
        /// One entry per pass: the context it was given.
        seen: Vec<PassContext>,
        restores: Vec<usize>,
        tail_removals: Vec<StopConnection>,
        base_items: usize,
    }

    impl ScriptedDriver {
        fn new(script: Vec<Script>, base_items: usize) -> ScriptedDriver {
            ScriptedDriver {
                script,
                seen: Vec::new(),
                restores: Vec::new(),
                tail_removals: Vec::new(),
                base_items,
            }
        }

        fn stubs(&self, board: &Board) -> usize {
            board.store().len().saturating_sub(self.base_items)
        }

        fn remove_one_stub(&self, board: &mut Board) {
            let doomed = board
                .store()
                .iter()
                .find(|(_, item)| item.is_routable())
                .map(|(id, _)| id)
                .expect("a stub to remove");
            board.remove_item(doomed);
        }

        fn insert_one_stub(&self, board: &mut Board) {
            let offset = 100 + 40 * self.stubs(board) as i32;
            board
                .insert_trace_points(
                    &[IntPoint::new(offset, -1_200), IntPoint::new(offset, -1_000)],
                    0,
                    20,
                    NetNos::of(1),
                    1,
                    FixedState::Unfixed,
                    None,
                )
                .expect("the stub inserts");
        }

        fn set_stubs(&self, board: &mut Board, script: Script) {
            while self.stubs(board) > script.stubs {
                self.remove_one_stub(board);
            }
            while self.stubs(board) < script.stubs {
                self.insert_one_stub(board);
            }
            // A pass that changes nothing produces a board with the same hash,
            // which `BoardSnapshotStore::add` deduplicates — exactly as upstream
            // does. Churning replaces one stub with a fresh one so the board is
            // *different* at the *same* score, which is what a real pass that
            // reroutes without improving looks like, and the only way the
            // history reaches the eight entries the restore check needs.
            if script.churn && script.stubs > 0 {
                self.remove_one_stub(board);
                self.insert_one_stub(board);
            }
        }
    }

    impl PassDriver for ScriptedDriver {
        fn run_pass(&mut self, board: &mut Board, ctx: &PassContext) -> PassOutcome {
            self.seen.push(*ctx);
            // Past the end of the script the board simply stops changing, which
            // is the flat-score case every stagnation rule is about.
            let script = self
                .script
                .get(ctx.pass_no as usize - 1)
                .copied()
                .unwrap_or_else(|| Script::scoring(self.stubs(board)));
            self.set_stubs(board, script);
            PassOutcome {
                pass_no: ctx.pass_no,
                queued: 1,
                routed: script.routed,
                not_routed: script.not_routed,
                board_complete: script.board_complete,
                progressed: script.routed > 0 || script.not_routed > 0,
                ..PassOutcome::default()
            }
        }

        fn remove_tails(&mut self, _board: &mut Board, stop: StopConnection) {
            self.tail_removals.push(stop);
        }

        fn on_board_restored(&mut self, board: &Board, _delta: &StoreDelta) {
            self.restores.push(self.stubs(board));
        }
    }

    struct StubScorer {
        base_items: usize,
        /// Incompletes reported when the board is at its best.
        target_stubs: usize,
        /// Score points one stub is worth. `0.5` makes a one-stub pass gain
        /// exactly [`STAGNATION_SCORE_THRESHOLD`].
        score_per_stub: f64,
    }

    impl BoardScorer for StubScorer {
        fn score(&mut self, board: &Board) -> BoardScore {
            let stubs = board.store().len().saturating_sub(self.base_items);
            BoardScore {
                normalized: stubs as f64 * self.score_per_stub,
                incomplete_count: self.target_stubs.saturating_sub(stubs),
                max_connections: self.target_stubs,
                ..BoardScore::default()
            }
        }
    }

    struct Harness {
        board: Board,
        driver: ScriptedDriver,
        scorer: StubScorer,
    }

    fn scripted(script: Vec<Script>, target_stubs: usize) -> Harness {
        scripted_scaled(script, target_stubs, SCORE_PER_STUB)
    }

    fn scripted_scaled(script: Vec<Script>, target_stubs: usize, score_per_stub: f64) -> Harness {
        let fixture = TestFixture::empty_two_layer();
        let base_items = fixture.board.store().len();
        Harness {
            board: fixture.board,
            driver: ScriptedDriver::new(script, base_items),
            scorer: StubScorer {
                base_items,
                target_stubs,
                score_per_stub,
            },
        }
    }

    fn run(harness: &mut Harness, config: LoopConfig) -> RunOutcome {
        BatchLoop::new(config).run(&mut harness.board, &mut harness.driver, &mut harness.scorer)
    }

    fn config(max_passes: i32) -> LoopConfig {
        LoopConfig {
            max_passes,
            ..LoopConfig::default()
        }
    }

    /// `:1280` and `:1311-1313`: the ripup budget grows linearly and the
    /// per-connection allowance doubles.
    #[test]
    fn the_escalation_sequence_is_linear_ripup_and_doubling_time() {
        let mut harness = scripted(vec![], 10);
        run(&mut harness, config(5));
        let ripup: Vec<i32> = harness.driver.seen.iter().map(|ctx| ctx.ripup_costs).collect();
        assert_eq!(ripup, vec![100, 200, 300, 400, 500]);
        let budget: Vec<u64> = harness
            .driver
            .seen
            .iter()
            .map(|ctx| ctx.budget.limit_ms())
            .collect();
        assert_eq!(
            budget,
            vec![100_000, 200_000, 400_000, 800_000, 1_600_000],
            "100 s doubling per pass"
        );
        let passes: Vec<i32> = harness.driver.seen.iter().map(|ctx| ctx.pass_no).collect();
        assert_eq!(passes, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn the_pass_cap_stops_the_loop_and_is_skipped_when_non_positive() {
        let mut harness = scripted(vec![], 10);
        let outcome = run(&mut harness, config(3));
        assert_eq!(outcome.passes.len(), 3);
        assert_eq!(outcome.stop_reason, Some(StopReason::MaxPasses));

        // `maxPasses <= 0` is upstream's uncapped loop: it runs until a stop
        // rule fires, which on a flat score is pass-local stagnation.
        let mut uncapped = scripted(vec![], 10);
        let outcome = run(&mut uncapped, config(0));
        assert_eq!(outcome.stop_reason, Some(StopReason::PassLocalStagnation));
    }

    #[test]
    fn a_complete_board_ends_the_loop_immediately() {
        let script = vec![Script {
            routed: 0,
            board_complete: true,
            ..Script::scoring(0)
        }];
        let mut harness = scripted(script, 0);
        let outcome = run(&mut harness, config(9_999));
        assert_eq!(outcome.passes.len(), 1);
        assert_eq!(outcome.stop_reason, Some(StopReason::BoardComplete));
    }

    /// The pass-local tracker's first evaluation is pass 8 (it seeds
    /// `lastBestScore` from `-inf`), so ten flat passes carry it to pass 18.
    #[test]
    fn pass_local_stagnation_fires_exactly_ten_flat_passes_after_the_minimum() {
        let mut harness = scripted(vec![], 10);
        let outcome = run(&mut harness, config(9_999));
        assert_eq!(outcome.stop_reason, Some(StopReason::PassLocalStagnation));
        assert_eq!(
            outcome.passes.len(),
            STOP_AT_PASS_MINIMUM as usize + STAGNATION_PASS_LIMIT as usize,
            "8 seeds the counter, 9..=18 are the ten flat passes"
        );
    }

    /// A gain of *exactly* the threshold is not an improvement: the test is
    /// `score > last_best + 0.5` (`:1057`), strictly.
    ///
    /// At half a point per stub, pass 9 gaining one stub is a gain of exactly
    /// 0.5 and must not reset the counter — so the run still stops ten flat
    /// passes after the minimum, at 18. Gaining two stubs is 1.0, which does
    /// reset it, and the same run stops one pass later. The two together are
    /// what distinguishes `>` from `>=`.
    #[test]
    fn a_gain_of_exactly_the_threshold_does_not_reset_the_counter() {
        fn stop_pass(gain_stubs: usize) -> usize {
            let mut script: Vec<Script> = (1..=8).map(|_| Script::scoring(4)).collect();
            script.extend((9..=40).map(|_| Script::scoring(4 + gain_stubs)));
            let mut harness = scripted_scaled(script, 10, STAGNATION_SCORE_THRESHOLD);
            let outcome = run(&mut harness, config(40));
            assert_eq!(outcome.stop_reason, Some(StopReason::PassLocalStagnation));
            outcome.passes.len()
        }
        assert_eq!(stop_pass(1), 18, "a gain of exactly 0.5 is not an improvement");
        assert_eq!(stop_pass(2), 19, "a gain of 1.0 is");
    }

    /// A pass that improves keeps the loop going; the counter restarts from it.
    #[test]
    fn an_improving_pass_resets_the_pass_local_counter() {
        // Improve on passes 8 and 14, flat everywhere else.
        let mut script: Vec<Script> = (1..=30).map(|_| Script::scoring(5)).collect();
        for pass in 8..=30 {
            script[pass - 1] = Script::scoring(if pass >= 14 { 7 } else { 6 });
        }
        let mut harness = scripted(script, 10);
        let outcome = run(&mut harness, config(9_999));
        assert_eq!(outcome.stop_reason, Some(StopReason::PassLocalStagnation));
        // 8 seeds at 6, 14 improves to 7 and resets, 15..=24 are ten flat passes.
        assert_eq!(outcome.passes.len(), 24);
    }

    /// The regression script for the restore machinery: one good pass, then a
    /// run of passes that each rewire the board without improving it.
    fn regressing_script() -> Vec<Script> {
        let mut script = vec![Script::scoring(9)];
        script.extend((2..=60).map(|_| Script::churning(1)));
        script
    }

    /// `:1100-1121` — the global tracker is not reset by a board restore, so a
    /// board that keeps being restored to the same state still stops.
    #[test]
    fn the_global_tracker_stops_a_run_that_restores_forever() {
        // Pass 1 reaches score 9; every later pass drops it to 1, so the
        // restore check at 8, 12, … keeps pulling the good board back and the
        // pass-local counter keeps being reset.
        let mut harness = scripted(regressing_script(), 10);
        let outcome = run(&mut harness, config(9_999));

        assert_eq!(outcome.restores, 3, "passes 8, 12 and 16");
        assert_eq!(outcome.stop_reason, Some(StopReason::GlobalStagnation));
        // Pass 8 restores to the score-9 board and records it as the global
        // best; ten passes later the global tracker fires, even though the
        // restores kept resetting the pass-local counter.
        assert_eq!(outcome.passes.len(), 18);
    }

    /// `:953-955` + `:958-959` — the restore check runs on passes ≥ 8 that are
    /// 0 mod 4, and never before.
    #[test]
    fn the_restore_check_runs_every_fourth_pass_from_the_eighth() {
        for (cap, expected) in [(7, 0), (8, 1), (11, 1), (12, 2), (13, 2)] {
            let mut harness = scripted(regressing_script(), 10);
            let outcome = run(&mut harness, config(cap));
            assert_eq!(
                outcome.restores, expected,
                "cap {cap}: restores only on passes 8 and 12"
            );
        }
    }

    /// The outer guard is on the **history size**, not the pass number
    /// (conformance appendix row 59): a board that stops changing deduplicates
    /// away, the history never reaches eight entries, and the restore check
    /// therefore never runs however long the loop goes on.
    #[test]
    fn a_history_below_the_minimum_never_restores() {
        let mut script = vec![Script::scoring(9)];
        script.extend((2..=60).map(|_| Script::scoring(1)));
        let mut harness = scripted(script, 10);
        let outcome = run(&mut harness, config(0));
        assert_eq!(outcome.max_history_depth, 3, "empty, score 9, score 1");
        assert_eq!(outcome.restores, 0);
        assert_eq!(outcome.stop_reason, Some(StopReason::PassLocalStagnation));
        // The best board is still recovered at the end.
        assert_eq!(outcome.final_score, 9.0);
    }

    /// `:1141-1159` — the run ends on the best board it ever held, even when the
    /// last pass was worse.
    #[test]
    fn the_final_board_is_the_best_one_ever_seen() {
        // Climb to 9, then fall to 2 and stay there until the cap.
        let mut script = vec![
            Script::scoring(3),
            Script::scoring(6),
            Script::scoring(9),
            Script::scoring(2),
        ];
        script.extend((5..=6).map(|_| Script::scoring(2)));
        let mut harness = scripted(script, 10);
        let outcome = run(&mut harness, config(6));

        assert_eq!(outcome.stop_reason, Some(StopReason::MaxPasses));
        assert!(outcome.best_board_restored, "{outcome:?}");
        assert_eq!(outcome.final_score, 9.0);
        assert!(outcome.final_score >= outcome.best_history_score);
        assert_eq!(
            harness.driver.stubs(&harness.board),
            9,
            "the board itself is the good one, not just the score"
        );
    }

    /// The final restore has **no pass-count guard** (`:1141-1159`): unlike the
    /// in-loop restore check, which upstream arms only at
    /// [`STOP_AT_PASS_MINIMUM`] on passes `≡ 0 (mod 4)`, the end-of-run one is a
    /// bare `bestHistoryScore > currentFinalScore`. A two-pass run whose second
    /// pass wrecked the board therefore still ends on the first pass's board —
    /// the case a short `-mp` is entirely made of, and the reason a low pass cap
    /// cannot end below what a lower one reached.
    #[test]
    fn the_final_restore_is_not_gated_by_the_pass_minimum() {
        for cap in 2..=5 {
            // Pass 1 climbs to 9, every later pass wrecks it back down to 2.
            let mut script = vec![Script::scoring(9)];
            script.extend((2..=cap).map(|_| Script::churning(2)));
            let mut harness = scripted(script, 10);
            let outcome = run(&mut harness, config(cap));

            assert_eq!(outcome.restores, 0, "cap {cap}: no in-loop restore arms");
            assert!(
                outcome.best_board_restored,
                "cap {cap}: the end-of-run restore must fire anyway: {outcome:?}"
            );
            assert_eq!(outcome.final_score, 9.0, "cap {cap}");
            assert_eq!(
                harness.driver.stubs(&harness.board),
                9,
                "cap {cap}: the board itself is pass 1's"
            );
        }
    }

    /// The restore callback is the invalidation seam: it sees the board *after*
    /// the swap.
    #[test]
    fn the_restore_callback_sees_the_restored_board() {
        let mut harness = scripted(regressing_script(), 10);
        run(&mut harness, config(12));
        assert!(!harness.driver.restores.is_empty());
        assert!(
            harness.driver.restores.iter().all(|stubs| *stubs == 9),
            "every restore lands on the good board: {:?}",
            harness.driver.restores
        );
    }

    /// `:29` — the snapshot store never grows past its cap, whatever the pass
    /// count.
    #[test]
    fn the_history_never_grows_past_its_cap() {
        // Strictly improving scores, so every pass wants to be retained.
        let script: Vec<Script> = (1..=60).map(Script::scoring).collect();
        let mut harness = scripted(script, 100);
        let outcome = run(
            &mut harness,
            LoopConfig {
                max_passes: 60,
                ..LoopConfig::default()
            },
        );
        assert_eq!(outcome.passes.len(), 60);
        assert!(
            outcome.max_history_depth <= crate::batch::history::MAX_HISTORY_SIZE,
            "{}",
            outcome.max_history_depth
        );
        assert_eq!(
            outcome.max_history_depth,
            crate::batch::history::MAX_HISTORY_SIZE
        );
    }

    /// `:1067-1082` — three flat passes past the minimum with incompletes left
    /// and fanout on run the one-time tail cleanup, and only once.
    #[test]
    fn the_fanout_tail_recovery_fires_once_after_three_flat_passes() {
        let mut harness = scripted(vec![], 10);
        let outcome = run(
            &mut harness,
            LoopConfig {
                max_passes: 40,
                fanout_enabled: true,
                ..LoopConfig::default()
            },
        );
        assert!(outcome.fanout_recovery_applied);
        assert_eq!(
            harness
                .driver
                .tail_removals
                .iter()
                .filter(|stop| **stop == StopConnection::None)
                .count(),
            1,
            "one-time: {:?}",
            harness.driver.tail_removals
        );
        // The recovery resets the pass-local counter at pass 11, so that one
        // can no longer reach ten — and the *global* tracker, which the
        // recovery does not touch, is what ends the run at pass 18. Upstream
        // behaves the same way, which is the point of having both trackers.
        assert_eq!(outcome.stop_reason, Some(StopReason::GlobalStagnation));
        assert_eq!(outcome.passes.len(), 18);
    }

    #[test]
    fn the_recovery_is_skipped_when_fanout_did_not_run() {
        let mut harness = scripted(vec![], 10);
        let outcome = run(&mut harness, config(40));
        assert!(!outcome.fanout_recovery_applied);
        assert!(harness.driver.tail_removals.is_empty());
        // Without the recovery the pass-local counter is the one that reaches
        // ten first, at the same pass.
        assert_eq!(outcome.stop_reason, Some(StopReason::PassLocalStagnation));
        assert_eq!(outcome.passes.len(), 18);
    }

    /// `:496-505` — a pass that hits `maxItems` stops the whole run, and the
    /// stop is still funnelled through the restore machinery so the run ends on
    /// the best board it holds.
    #[test]
    fn the_item_cap_stops_the_run_at_the_pass_that_hit_it() {
        struct CappedDriver;
        impl PassDriver for CappedDriver {
            fn run_pass(&mut self, _board: &mut Board, ctx: &PassContext) -> PassOutcome {
                PassOutcome {
                    pass_no: ctx.pass_no,
                    routed: 1,
                    progressed: true,
                    hit_item_cap: ctx.pass_no == 2,
                    ..PassOutcome::default()
                }
            }
            fn remove_tails(&mut self, _board: &mut Board, _stop: StopConnection) {}
        }

        let fixture = TestFixture::empty_two_layer();
        let base_items = fixture.board.store().len();
        let mut board = fixture.board;
        let mut scorer = StubScorer {
            base_items,
            target_stubs: 4,
            score_per_stub: SCORE_PER_STUB,
        };
        let outcome = BatchLoop::new(config(9_999)).run(&mut board, &mut CappedDriver, &mut scorer);
        assert_eq!(outcome.passes.len(), 2);
        assert_eq!(outcome.stop_reason, Some(StopReason::ItemLimit));
    }

    /// An armed budget that is already gone stops the loop **before** the first
    /// pass, and the run still leaves through the normal tail — nothing is
    /// half-done and the board is the one it came in on.
    #[test]
    fn an_expired_job_deadline_stops_the_loop_before_any_pass() {
        let mut harness = scripted(vec![], 10);
        let outcome = run(
            &mut harness,
            LoopConfig {
                max_passes: 40,
                deadline: JobDeadline::already_passed(),
                ..LoopConfig::default()
            },
        );
        assert!(outcome.passes.is_empty(), "{outcome:?}");
        assert_eq!(outcome.stop_reason, Some(StopReason::JobDeadline));
        assert_eq!(outcome.stop_cause, Some(StopCause::Deadline));
        assert!(harness.driver.seen.is_empty(), "no pass may have run");
    }

    /// A budget that runs out *during* the run stops it at the next pass
    /// boundary and — the whole point — the board that comes out is the best
    /// one the run held, not the one the last pass happened to leave.
    ///
    /// The one test in this file that spends real time, because the thing under
    /// test is a wall clock. Two passes of 25 ms against a 40 ms budget: the
    /// margins are an order of magnitude apart, so a loaded machine changes
    /// which pass the deadline lands on and not whether the assertions hold —
    /// none of them names a pass count.
    #[test]
    fn a_deadline_that_expires_mid_run_still_finishes_on_the_best_board() {
        struct SlowDriver {
            inner: ScriptedDriver,
            per_pass: Duration,
        }
        impl PassDriver for SlowDriver {
            fn run_pass(&mut self, board: &mut Board, ctx: &PassContext) -> PassOutcome {
                std::thread::sleep(self.per_pass);
                self.inner.run_pass(board, ctx)
            }
            fn remove_tails(&mut self, board: &mut Board, stop: StopConnection) {
                self.inner.remove_tails(board, stop);
            }
        }

        let fixture = TestFixture::empty_two_layer();
        let base_items = fixture.board.store().len();
        let mut board = fixture.board;
        // Climb to 9, then throw it away and stay down. Without the closing
        // restore the deadline would hand back the wrecked board.
        let mut script = vec![Script::scoring(9)];
        script.extend((2..=40).map(|_| Script::scoring(1)));
        let mut driver = SlowDriver {
            inner: ScriptedDriver::new(script, base_items),
            per_pass: Duration::from_millis(25),
        };
        let mut scorer = StubScorer {
            base_items,
            target_stubs: 10,
            score_per_stub: SCORE_PER_STUB,
        };
        let outcome = BatchLoop::new(LoopConfig {
            max_passes: 40,
            deadline: JobDeadline::after(Duration::from_millis(40)),
            ..LoopConfig::default()
        })
        .run(&mut board, &mut driver, &mut scorer);

        assert_eq!(outcome.stop_reason, Some(StopReason::JobDeadline));
        assert_eq!(outcome.stop_cause, Some(StopCause::Deadline));
        assert!(
            outcome.passes.len() < 40,
            "the pass cap must not be what stopped it: {}",
            outcome.passes.len()
        );
        assert!(outcome.best_board_restored, "{outcome:?}");
        assert_eq!(outcome.final_score, 9.0);
        assert_eq!(
            driver.inner.stubs(&board),
            9,
            "the board itself is the best one, not just the score"
        );
    }

    /// `LoopConfig::default` carries the ambient deadline, and with none armed
    /// that is a budget nothing ever spends — no wall clock is read at all.
    #[test]
    fn no_armed_deadline_means_no_budget() {
        assert!(!JobDeadline::NONE.is_armed());
        assert!(!JobDeadline::NONE.expired());
        assert_eq!(JobDeadline::NONE.remaining(), None);
        assert_eq!(JobDeadline::NONE.cause(), None);
        // Nothing in this process armed one, so the ambient deadline is a
        // no-op too — which is what keeps every other test in this file
        // clock-free.
        assert!(!JobDeadline::ambient().expired());
        assert!(JobDeadline::already_passed().expired());
        assert_eq!(
            JobDeadline::already_passed().cause(),
            Some(StopCause::Deadline)
        );
    }

    /// The connection allowance never outlives the job: a connection armed with
    /// 100 s of upstream budget gets what the job has left when that is less.
    #[test]
    fn the_connection_budget_is_clamped_to_what_the_job_has_left() {
        let policy = BudgetPolicy::default();
        let unbounded = PassContext::new(1, 100, &policy, false).with_deadline(JobDeadline::NONE);
        assert_eq!(
            unbounded.connection_budget().limit_ms(),
            unbounded.budget.limit_ms(),
            "no deadline, no clamp"
        );

        let tight = PassContext::new(4, 100, &policy, false)
            .with_deadline(JobDeadline::after(Duration::from_millis(250)));
        assert_eq!(tight.budget.limit_ms(), 800_000, "pass 4 is 8 x 100 s");
        assert!(
            tight.connection_budget().limit_ms() <= 250,
            "{}",
            tight.connection_budget().limit_ms()
        );

        // A deterministic run never reads the wall clock, so it is never
        // clamped by one either (gate G2.2).
        let deterministic = PassContext::new(1, 100, &BudgetPolicy::deterministic(), false)
            .with_deadline(JobDeadline::after(Duration::from_millis(1)));
        assert_eq!(
            deterministic.connection_budget().limit_ms(),
            deterministic.budget.limit_ms()
        );
    }

    #[test]
    fn the_requeue_policy_resolves_on_the_pass_count() {
        assert!(RequeuePolicy::default().applies_to(1));
        assert!(!RequeuePolicy::default().applies_to(2));
        assert!(!RequeuePolicy::Never.applies_to(1));
        assert!(RequeuePolicy::Always.applies_to(8));
    }
}
