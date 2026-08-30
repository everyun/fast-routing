//! **The pipeline.** DSN bytes in, judged board and session file out.
//!
//! This is `RoutingJob` v2 — the M1 replacement for [`crate::legacy::routing_job`], and the
//! thing `fr-cli` actually runs. Four stages, each owned by the crate that specialises in it:
//!
//! ```text
//! bytes ──▶ fr_io::specctra::import ──▶ Board + BoardRules      [`RoutingJob::import`]
//!                                        │
//!                                        ▼
//!                ┌── fr_router: fanout + maze passes + optimizer ──┐  [`RouterStage`]
//!                └────────────── or nothing at all ────────────────┘
//!                                        │
//!                                        ▼
//!                          fr_io::specctra::ses_write           [`write_session`]
//!                          + the file, if one was asked for  [`RoutingJob::with_session_path`]
//!                                        │
//!                                        ▼
//!                          fr_check: clearance + connectivity   [`Referee::judge`]
//!                          + the v2 score
//! ```
//!
//! # The session is written before the board is judged
//!
//! That order is deliberate and it is a contract, not an implementation detail. A run under a
//! harness cap can be killed at any instant, and of the two things left to do at the end — write
//! the artefact, and describe it — only one of them is the *product*. A kill between the phases
//! costs a report; a kill in the other order costs the board. So [`RoutingJob::run`] builds the
//! session, puts it on disk (atomically: a temporary file in the destination directory, then a
//! rename, so a kill mid-write cannot leave a truncated `.ses` behind), and only then calls the
//! referee.
//!
//! The referee is not cheap on a large board and it is not the caller's deliverable. Judging
//! first was measured costing sessions: `bench/run_bench.py` classified `SIGKILL`ed runs as "no
//! session written" when the file would have existed under this order.
//!
//! # The router stage is opt-in, and it is the only thing that may mutate the board
//!
//! [`RouterStage::ImportOnly`] is still the default and is a no-op *by construction*: its match
//! arm never receives `&mut design.board`. The M1 contract it protects — a G1.1 or G1.2a delta
//! can only ever be an importer or a checker bug — therefore still holds for every gate emitter,
//! for `--import-only`, and for both `--*-report` modes.
//!
//! [`RouterStage::Maze`] is [`fr_router::fanout`] (optional), then
//! [`fr_router::BatchRouter`], then [`fr_router::optimizer::run_optimization_stage`],
//! writing into the imported board in place — the release's own sequencing
//! (`management/RoutingJobSchedulerActionThread.thread_action`, `:96`, `:118`,
//! `:179-189`). The pre-rewrite
//! `fr-autoroute` toy is still not reachable from here — it is written against
//! [`fr_board::BasicBoard`], a second item model the importer never produces, and its output has
//! never been judged by [`fr_check`]. It stays behind [`crate::legacy`] and nowhere else.
//!
//! # What "SES passthrough" means
//!
//! Under [`RouterStage::ImportOnly`] the session file carries the wiring the DSN already had —
//! `(type route)` copper, plus whatever a previous tool left behind — re-emitted through the v2
//! writer with its endpoint snapping (`dsn-board-semantics.md` §12.3). It is a *valid* session
//! that KiCad will accept; it is not a *routed* one. Callers that need to tell the difference
//! read [`JobReport::routed`], and `fr-cli` turns that into its own exit code.
//!
//! ```no_run
//! let job = fr_core::RoutingJob::from_path("board.dsn").unwrap();
//! let report = job.run().unwrap();
//! println!("{} violations, {} incomplete, score {:.1}",
//!          report.stats.clearance_violation_count,
//!          report.stats.incomplete_count,
//!          report.score.normalized);
//! ```

use std::path::{Path, PathBuf};

use fr_board::item::ItemKind;
use fr_board::Board;
use fr_check::{
    board_connectivity, check_clearance_parallel, score, shape_store, AnchorIndex,
    BoardConnectivity, BoardStats, Score, ScoreWeights, StoreView, Violation,
};
use fr_io::specctra::{self, ImportError, ImportOptions, ImportedDesign, SesInput, Unit};
use fr_router::batch::budget::BudgetPolicy;
use fr_router::batch::portfolio::{self, PortfolioConfig};
use fr_router::batch::threads::Threads;
use fr_router::batch::BatchRouter;
use fr_router::fanout::{fanout_board, FanoutConfig};
use fr_router::optimizer::{run_optimization_stage_with_threads, LoopExit};
use fr_router::StopReason;
use fr_rules::router_settings::{BoardAspectRatio, RouterSettings};

/// The router stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RouterStage {
    /// Import, judge, write the imported wiring back out. No routing, no board mutation.
    #[default]
    ImportOnly,
    /// M2a: the shape-expansion maze router, in place on the imported board.
    Maze(MazeOptions),
}

/// How the [`RouterStage::Maze`] stage is run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MazeOptions {
    /// Ripup-and-reroute passes. Pass *n* routes at `start_ripup_costs * n`
    /// (`batch-orchestration.md` §4, row 44). `-mp` on the command line.
    ///
    /// Must be `>= 1`: upstream's `maxPasses <= 0` re-enables the router with
    /// **no pass cap at all** (conformance appendix §1.2), and this port
    /// refuses to reach that state by accident — `fr-cli` rejects a negative
    /// `-mp` with an error, and [`MazeOptions::with_passes`] clamps.
    pub max_passes: i32,
    /// Run the SMD pin-escape pre-pass first.
    ///
    /// **Off by default, unlike upstream** (`router.fanout.enabled` defaults `true`,
    /// `DefaultSettings.java:113`): the escape via is placed by the same M2a stand-in locator as
    /// every other connection, and a fanout that escapes into a bad via placement is much harder
    /// to undo than a connection that simply fails. It is opt-in until the insert wave lands.
    pub fanout: bool,
    /// Spend the per-connection budget in effort-based virtual time instead of wall time, so no
    /// routing decision can depend on machine load (`fr_router::batch::budget`, gate G2.2).
    /// `--deterministic` on the command line.
    pub deterministic: bool,
    /// Run the post-routing optimization stage.
    ///
    /// **On by default, as the release is.** `getRunOptimizer()` reads
    /// `optimizer.enabled` fail-closed, but every job's settings are merged onto
    /// `DefaultSettings`, which sets it to `true` (conformance #6, #7), and the
    /// headless entry point runs the stage right after the router
    /// (`RoutingJobSchedulerActionThread:179-189`). `--no-optimizer` on the
    /// command line is the only way to reach the off state, exactly as
    /// `--router.optimizer.enabled=false` is upstream.
    pub optimize: bool,
    /// The worker budget the caller asked for: `None` = "not stated",
    /// `Some(0)` = "pick for me", `Some(n)` = exactly `n`. `--threads` on the
    /// command line.
    ///
    /// Kept as the *request* rather than as a resolved [`Threads`] so that
    /// [`MazeOptions::threads`] can apply the `--deterministic` rule whatever
    /// order the builders were called in.
    pub threads_requested: Option<usize>,
    /// Route passes to race against one another on board clones, keeping the
    /// best referee score (`fr_router::batch::portfolio`). `1` — the default —
    /// is the plain single run and clones nothing. `--portfolio` on the command
    /// line.
    pub portfolio: usize,
}

impl Default for MazeOptions {
    fn default() -> Self {
        MazeOptions {
            max_passes: 1,
            fanout: false,
            deterministic: false,
            optimize: true,
            threads_requested: None,
            portfolio: 1,
        }
    }
}

impl MazeOptions {
    /// `max_passes` passes, no fanout.
    pub fn with_passes(max_passes: i32) -> MazeOptions {
        MazeOptions {
            max_passes: max_passes.max(1),
            ..MazeOptions::default()
        }
    }

    /// Turns the fanout pre-pass on or off.
    pub fn with_fanout(mut self, fanout: bool) -> MazeOptions {
        self.fanout = fanout;
        self
    }

    /// Turns the effort-based virtual budget clock on or off.
    pub fn with_deterministic(mut self, deterministic: bool) -> MazeOptions {
        self.deterministic = deterministic;
        self
    }

    /// Turns the post-routing optimization stage on or off. On by default.
    pub fn with_optimizer(mut self, optimize: bool) -> MazeOptions {
        self.optimize = optimize;
        self
    }

    /// Records the caller's `--threads` request. See
    /// [`MazeOptions::threads_requested`].
    pub fn with_threads(mut self, requested: Option<usize>) -> MazeOptions {
        self.threads_requested = requested;
        self
    }

    /// Sets the portfolio arm count (`--portfolio`). Floored at one.
    pub fn with_portfolio(mut self, arms: usize) -> MazeOptions {
        self.portfolio = arms.max(1);
        self
    }

    /// The worker budget actually in force.
    ///
    /// # An absent `--threads` is one worker, not the machine's core count
    ///
    /// The T1 optimizer is *deterministic* but it is not the *same heuristic* as
    /// the sequential one — it trusts a worker verdict that a promotion inside
    /// the same window may have staled, which can hide an improvement
    /// (`fr_router::optimizer::parallel`). Defaulting it on would therefore move
    /// the quality gate as a side effect of a performance change, and would move
    /// it by a different amount on every machine, because the default would be
    /// `available_parallelism`. So an absent flag means one worker. `--threads
    /// 0` is how a caller says "pick from the machine" and gets
    /// [`Threads::automatic`].
    ///
    /// # `--deterministic` and an unstated thread count
    ///
    /// `--deterministic` exists so a run is byte-reproducible
    /// (`ARCHITECTURE.md`, "Budgets"). T1 *is* deterministic — the promoter
    /// applies winners in sweep order regardless of completion order, and
    /// workers are assigned items by a fixed round-robin — but it is
    /// deterministic **at a fixed worker count**, and two thread counts are two
    /// configurations of the heuristic, not two runs of one. So
    /// `--deterministic` does not force one thread: it refuses a thread count
    /// that came from the *machine*, because that number cannot be reproduced
    /// elsewhere. `--deterministic --threads 4` is honoured and reproduces
    /// byte-for-byte anywhere; `--deterministic --threads 0` falls back to one.
    pub fn threads(&self) -> Threads {
        match (self.deterministic, self.threads_requested) {
            (_, None) => Threads::single(),
            (true, Some(0)) => Threads::single(),
            (_, requested) => Threads::resolve(requested),
        }
    }
}

/// What the optimization stage did, `None` when it did not run.
///
/// `fr_router::optimizer::OptimizerOutcome` flattened to the numbers a summary
/// line and a harness read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OptimizerReport {
    /// Passes actually run — `BatchOptimizer.runBatchLoop`'s pass list.
    pub passes: usize,
    /// `totalItemsOptimized`: items the sweep handed out and the optimizer
    /// attempted, over every pass.
    pub items_optimized: usize,
    /// Of those, the reroutes that strictly improved the board and were kept.
    pub items_improved: usize,
    /// `getNormalizedScore` before the first pass.
    pub score_before: f64,
    /// `getNormalizedScore` after the last one.
    pub score_after: f64,
    /// Ratsnest incompletes when the optimizer stopped.
    pub incomplete_count: usize,
    /// Why the loop stopped, as a short label.
    pub exit: &'static str,
}

/// What a `--portfolio N` race did, `None` when only one arm ran.
///
/// The numbers the experiment `ARCHITECTURE.md` asks for is answerable from:
/// *"does a diverse portfolio find more connections within the same wall
/// budget?"* — [`PortfolioReport::arm_incompletes`] is that question's answer,
/// per arm.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PortfolioReport {
    /// Arms raced.
    pub arms: usize,
    /// Index of the arm whose board was adopted; `0` is the default item order.
    pub winner: usize,
    /// Whether a shuffled arm beat the default one. `false` means the race cost
    /// cores and bought nothing on this board.
    pub shuffle_won: bool,
    /// Ratsnest incompletes per arm, in arm order.
    pub arm_incompletes: Vec<usize>,
    /// `getNormalizedScore` per arm, in arm order.
    pub arm_scores: Vec<f64>,
}

/// What the router stage did, `None` when it did not run.
///
/// Not [`Eq`]: the optimizer reports scores, and a score is an `f64`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RouterReport {
    /// Passes actually run.
    pub passes: usize,
    /// Connections routed, summed over the passes.
    pub routed: usize,
    /// Connections the maze could not make, in the last pass.
    pub not_routed: usize,
    /// Items ripped up, summed over the passes.
    pub ripped: usize,
    /// SMD pins escaped by the fanout pre-pass.
    pub fanouted: usize,
    /// Ratsnest incompletes when the **stage** stopped, as the referee counts them —
    /// after the optimizer when it ran, after the batch loop when it did not. The
    /// batch loop's stop rules read the referee's score, so this is the same number
    /// [`JobReport::stats`] carries and not a second opinion.
    pub incomplete_count: usize,
    /// Why the batch loop stopped (`fr_router::StopReason`), as a short label
    /// for the summary line.
    pub stop_reason: &'static str,
    /// Board restores the pass ≥ 8 check performed.
    pub restores: usize,
    /// Whether the run ended by restoring the best board it ever held.
    pub best_board_restored: bool,
    /// Connections that reached their per-connection budget, summed over the
    /// passes — ARCHITECTURE "Budgets"'s `connections_hitting_budget`.
    pub connections_hitting_budget: usize,
    /// The post-routing optimization stage; `None` when it was switched off or
    /// skipped.
    pub optimizer: Option<OptimizerReport>,
    /// The `--portfolio` race; `None` when a single arm ran.
    pub portfolio: Option<PortfolioReport>,
}

/// Millimetres to board units for this design.
///
/// The importer stores `Unit::scale(1, unit, UM) / resolution` micrometres per board unit
/// (`board_geometry_stats`); the fanout annulus wants the inverse, in millimetres.
fn board_units_per_mm(design: &ImportedDesign) -> f64 {
    let um_per_board_unit =
        Unit::scale(1.0, design.unit, Unit::Um) / f64::from(design.resolution.max(1));
    if um_per_board_unit > 0.0 {
        1000.0 / um_per_board_unit
    } else {
        1.0
    }
}

/// The router settings this design routes under: the shipped defaults, sized to the board, then
/// the file's own `(autoroute_settings …)` scope, then
/// [`RouterSettings::apply_board_specific_optimizations`] — without which every layer costs
/// 1.0/1.0 and the maze has no preferred direction.
///
/// # The order of the last two is upstream's, and it is what decides which DSN fields survive
///
/// Upstream merges the scope into the job's settings from inside the DSN read
/// (`Structure.read_scope:979-987`) and calls `applyBoardSpecificOptimizations` immediately
/// afterwards, on the loaded board (`HeadlessBoardManager:628`, and again in
/// `RoutingJobScheduler:108` after the settings merger). That function fills in only the per-layer
/// entries that are still unset, so the file's `(active …)` and `(preferred_direction …)` survive
/// it — while it recomputes *both* per-layer trace-cost arrays outright, which is why the file's
/// `preferred_direction_trace_costs` never reach the maze.
/// [`fr_io::specctra::autoroute_settings`] carries the evidence and does not apply them.
fn settings_for(design: &ImportedDesign, options: MazeOptions) -> RouterSettings {
    let mut settings = RouterSettings::default();
    settings.set_layer_count(design.rules.layer_structure.len());
    if let Some(dsn) = &design.autoroute_settings {
        specctra::apply_autoroute_settings(&mut settings, dsn);
    }
    settings.apply_board_specific_optimizations(
        BoardAspectRatio::from_bounding_box(
            f64::from(design.bounds.width()),
            f64::from(design.bounds.height()),
        ),
        &design.rules.layer_structure,
    );
    settings.max_passes = options.max_passes.max(1);
    settings
}

/// Ratsnest incompletes on the board as it stands — the referee's own count, without the
/// clearance scan.
///
/// Only the `(autoroute off)` path needs it: every other path gets the number from the stage that
/// last touched the board.
fn incomplete_count_of(design: &ImportedDesign) -> usize {
    let board = &design.board;
    let shapes = shape_store(board);
    let view = StoreView::new(
        board.store(),
        &shapes,
        &design.rules.clearance_matrix,
        board.layers().len(),
    );
    board_connectivity(&view).incomplete_count
}

/// Runs [`RouterStage::Maze`] over `design` in place.
fn run_maze_stage(design: &mut ImportedDesign, options: MazeOptions) -> RouterReport {
    let settings = settings_for(design, options);
    let scale = board_units_per_mm(design);
    let mut report = RouterReport::default();

    if options.fanout {
        let config = FanoutConfig::from_settings(&settings, scale);
        // Disjoint field borrows: the router needs `&mut board` and `&rules` at once.
        let outcome = fanout_board(&mut design.board, &design.rules, &settings, &config);
        report.fanouted = outcome.total_items_fanouted;
    }

    // The batch loop needs three things the design knows and the board does not: whether the
    // fanout stage actually ran (it gates the one-time fanout-tail recovery, `:1073`), the unit
    // conversion of the score's trace-length term, and the budget clock.
    let budget = if options.deterministic {
        BudgetPolicy::deterministic()
    } else {
        BudgetPolicy::default()
    };
    let threads = options.threads();
    let mut router = BatchRouter::new(&design.rules, &settings)
        .with_fanout(options.fanout)
        .with_board_units_per_mm(scale)
        .with_budget(budget);
    // T1b. A portfolio of one is the plain run and clones nothing, so the
    // default path through here is byte-identical to the pre-portfolio one; the
    // `total_items_routed` item-cap check below reads the router either way,
    // and a portfolio arm's copy of it is discarded with the losing arms.
    //
    // `(autoroute off)` in the file's `autoroute_settings` scope skips the batch
    // loop and nothing else: upstream never constructs the `BatchAutorouter`
    // (`RoutingJobSchedulerActionThread:96`), falls through to its fanout-only
    // branch, and still runs the optimizer at `:179` if `postroute` allows it.
    // `describe_stop(None)` — "not started" — is already the label for it.
    let outcome = if !settings.run_router() {
        fr_router::batch::RunOutcome {
            // The report's incomplete count has to be the one the referee will
            // re-derive a few lines later, and a run that routed nothing still
            // owes that number honestly rather than the `0` a default claims.
            incomplete_count: incomplete_count_of(design),
            ..fr_router::batch::RunOutcome::default()
        }
    } else if options.portfolio > 1 {
        let race = portfolio::race(
            &mut design.board,
            &design.rules,
            &settings,
            PortfolioConfig {
                arms: options.portfolio,
                max_passes: options.max_passes,
                budget,
                fanout: options.fanout,
                board_units_per_mm: scale,
            },
        );
        report.portfolio = Some(PortfolioReport {
            arms: race.arms.len(),
            winner: race.winner,
            shuffle_won: race.shuffle_won(),
            arm_incompletes: race
                .arms
                .iter()
                .map(|arm| arm.score.incomplete_count)
                .collect(),
            arm_scores: race.arms.iter().map(|arm| arm.score.normalized).collect(),
        });
        race.outcome()
    } else {
        router.run(&mut design.board, options.max_passes)
    };
    report.passes = outcome.passes.len();
    report.routed = outcome.passes.iter().map(|pass| pass.routed).sum();
    report.ripped = outcome
        .passes
        .iter()
        .map(|pass| pass.ripped_item_count)
        .sum();
    report.not_routed = outcome.passes.last().map_or(0, |pass| pass.not_routed);
    report.incomplete_count = outcome.incomplete_count;
    report.stop_reason = describe_stop(outcome.stop_reason);
    report.restores = outcome.restores;
    report.best_board_restored = outcome.best_board_restored;
    report.connections_hitting_budget = outcome
        .passes
        .iter()
        .map(|pass| pass.connections_hitting_budget)
        .sum();

    // `BatchAutorouter.autoroute_pass:496-505` — a router that stopped on
    // `maxItems` calls `requestStop()` over *all* stages, so hitting the item
    // cap also skips the optimizer (conformance #42). Nothing else about the
    // routing result gates it.
    let hit_the_item_cap =
        settings.max_items > 0 && router.total_items_routed() >= settings.max_items;
    // A spent job budget skips the stage outright. The optimizer's own
    // per-item router polls the deadline too, so entering it would terminate —
    // but only after a full sweep of every routed item, each one ripped,
    // re-attempted against a zero allowance and rolled back. The budget exists
    // to leave time for the referee and the session writer, and that sweep is
    // exactly the work that would spend it.
    let budget_spent = fr_router::job_budget_spent();
    if options.optimize && !hit_the_item_cap && !budget_spent {
        report.optimizer = run_optimization_stage_with_threads(
            &mut design.board,
            &design.rules,
            &settings,
            scale,
            threads,
        )
        .map(|outcome| OptimizerReport {
            passes: outcome.passes.len(),
            items_optimized: outcome.total_items_optimized.max(0) as usize,
            items_improved: outcome.items_improved(),
            score_before: outcome.score_before.normalized,
            score_after: outcome.score_after.normalized,
            incomplete_count: outcome.score_after.incomplete_count,
            exit: describe_optimizer_exit(outcome.exit),
        });
    }
    // The stage the board actually left is the one the incomplete count has to
    // describe; the referee re-derives it below and the two must agree.
    if let Some(optimizer) = &report.optimizer {
        report.incomplete_count = optimizer.incomplete_count;
    }
    report
}

/// A short label for the optimizer loop's exit.
fn describe_optimizer_exit(exit: LoopExit) -> &'static str {
    match exit {
        LoopExit::MaxPasses => "pass limit",
        LoopExit::MaxItems => "item limit",
        LoopExit::NearMaximumScore => "near-maximum score",
        LoopExit::BelowImprovementThreshold => "below improvement threshold",
        LoopExit::JobDeadline => "job deadline",
    }
}

/// A short label for the batch loop's stop reason.
fn describe_stop(reason: Option<StopReason>) -> &'static str {
    match reason {
        None => "not started",
        Some(StopReason::BoardComplete) => "board complete",
        Some(StopReason::NoProgress) => "no progress",
        Some(StopReason::MaxPasses) => "pass limit",
        Some(StopReason::ItemLimit) => "item limit",
        Some(StopReason::NoBetterBoard) => "no better board",
        Some(StopReason::RankLimit) => "board rank limit",
        Some(StopReason::PassLocalStagnation) => "stagnation (pass-local)",
        Some(StopReason::GlobalStagnation) => "stagnation (global)",
        Some(StopReason::JobDeadline) => "job deadline",
        Some(StopReason::Interrupted) => "interrupt",
    }
}

/// Arms the process's job budget: after `budget` the router stops the way a
/// stop rule stops it, and the pipeline still judges and writes the board.
///
/// One-shot — a second call is ignored and returns `false`. Everything
/// downstream reads it ambiently ([`fr_router::JobDeadline::ambient`]), so this
/// is the *only* thing a caller has to do; the batch loop, the pass's
/// between-connections poll and the optimizer's own per-item router all see it.
///
/// `Duration::ZERO` arms a budget that is already spent, which routes nothing
/// and writes the imported board — the cheapest end-to-end check that the path
/// works.
pub fn arm_job_deadline(budget: std::time::Duration) -> bool {
    fr_router::arm_job_deadline(budget)
}

/// Asks the run to stop at its next poll — the signal handler's entry point.
///
/// **Async-signal-safe**: a single relaxed atomic store, so `fr-cli` may call
/// it from its `SIGTERM` / `SIGINT` handler.
pub fn request_job_stop() {
    fr_router::request_job_stop();
}

/// Whether the job budget is gone: the armed deadline passed, or
/// [`request_job_stop`] was called.
///
/// This, and not the router's stop reason, is what `fr-cli` turns into its
/// deadline exit code: the budget can also run out during the import, the
/// optimizer or the referee, and a caller that asked for a budget wants to know
/// it was spent however the run ended.
pub fn job_budget_spent() -> bool {
    fr_router::job_budget_spent()
}

/// A stage of [`RoutingJob::run_observed`], announced as it completes.
///
/// The order is the contract: [`JobPhase::SessionWritten`] always precedes
/// [`JobPhase::Judged`], because a run that is killed between them still leaves
/// the caller a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobPhase {
    /// The DSN is a board; the tail reserve has been taken.
    Imported,
    /// The router stage is over — or there was none.
    Routed,
    /// The session text exists, and is on disk if a path was configured.
    SessionWritten,
    /// The referee has judged the board.
    Judged,
}

/// What one run is expected to spend after its work deadline: leaving the item
/// sweep, building the session, writing it, and judging the board.
///
/// **Measured, not guessed** (`nice -n 10`, one run at a time, release build,
/// `-mp 20`):
///
/// | board | items | drain | referee | session |
/// |---|---|---|---|---|
/// | `Issue209-split05` | 2 687 | 0.33 s | 0.014 s | 0.002 s |
/// | `Issue209-split10` | 2 755 | 1.36 s | 0.015 s | 0.011 s |
/// | `Issue103-Board-Routed` | 4 617 | 2.11 s | 0.163 s | 0.013 s |
/// | `Issue190-processor.Z80` | 10 892 | 1.04 s | 0.191 s | 0.035 s |
///
/// The referee and the writer are milliseconds even on the biggest board in the
/// suite; the drain — the optimizer sweep noticing and leaving — is what costs,
/// and it tracks item count loosely enough that the estimate is deliberately
/// generous. `0.25 s + 0.5 ms per item` covers every row above with margin to
/// spare on the two the estimate is *loosest* on (`Issue209-split05` 0.35 s
/// measured against 1.59 s budgeted, `Issue190` 1.27 s against 5.70 s) and by
/// the smallest margin on the two it tracks best: `Issue209-split10` 1.39 s
/// against 1.63 s and `Issue103` 2.29 s against 2.56 s, i.e. **1.12x on the
/// tightest board, not 2x** — the constant is sized to cover the worst case,
/// not to double it. Over-reserving costs routing time on a board that was
/// going to be cut short anyway; under-reserving costs the whole session to a
/// `SIGKILL`, so the asymmetry is priced in. [`fr_router::reserve_job_tail`]
/// then caps it at a quarter of the budget, so this can never eat a short run.
///
/// Re-measured on the M3 wave-3 gate (serial, `--job-timeout` sweeps in
/// `/tmp/gate3/tail-*.json`), the unused part of the reserve — i.e. how early
/// the run actually finished — was 1.29-1.34 s on `Issue209-split10`, 2.00 s on
/// `Issue103` and 4.40-4.61 s on `Issue190`. Every one of those is inside the
/// budget, which is the property that matters; none is negative, which is the
/// property that would cost a session.
fn tail_reserve_for(design: &ImportedDesign) -> std::time::Duration {
    const BASE: std::time::Duration = std::time::Duration::from_millis(250);
    const PER_ITEM_US: u64 = 500;
    let items = u64::try_from(design.board.store().len()).unwrap_or(u64::MAX);
    BASE + std::time::Duration::from_micros(items.saturating_mul(PER_ITEM_US))
}

/// Writes the session where the caller asked for it, atomically.
///
/// A plain `fs::write` truncates first, so a kill in the middle of it leaves a
/// *shorter* session that still parses — the one failure a harness cannot
/// detect. Writing a sibling temporary and renaming it makes the file either
/// the old one or the whole new one; `rename(2)` is atomic within a directory,
/// which is why the temporary is a sibling rather than one in `/tmp`.
fn write_session_file(path: &Path, text: &str) -> std::io::Result<()> {
    let temporary = match path.file_name() {
        Some(name) => {
            let mut scratch = name.to_os_string();
            scratch.push(format!(".{}.tmp", std::process::id()));
            path.with_file_name(scratch)
        }
        // A path with no file name cannot be renamed onto; let the direct write
        // produce the real error.
        None => return std::fs::write(path, text),
    };
    if let Err(error) = std::fs::write(&temporary, text) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(())
}

/// Why a job could not produce a report.
#[derive(Debug)]
pub enum JobError {
    /// The design file could not be read.
    Read(PathBuf, std::io::Error),
    /// The importer rejected it.
    Import(ImportError),
    /// The session file could not be written. Only a job given a
    /// [`RoutingJob::with_session_path`] can produce this; a caller that writes
    /// the report's `ses` itself handles its own IO.
    Write(PathBuf, std::io::Error),
}

impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobError::Read(path, error) => write!(f, "read {}: {error}", path.display()),
            JobError::Import(error) => write!(f, "{error}"),
            JobError::Write(path, error) => write!(f, "write {}: {error}", path.display()),
        }
    }
}

impl std::error::Error for JobError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            JobError::Read(_, error) => Some(error),
            JobError::Import(error) => Some(error),
            JobError::Write(_, error) => Some(error),
        }
    }
}

/// A headless job: one design, one configuration, one report.
///
/// Deliberately neither `Clone` nor `#[derive(Debug)]`: `source` is the whole design file, tens
/// of megabytes on the larger fixtures, and both would copy or print it. Use
/// [`RoutingJob::import_with`] where a second configuration is needed.
pub struct RoutingJob {
    source: Vec<u8>,
    design_name: String,
    import_options: ImportOptions,
    weights: ScoreWeights,
    stage: RouterStage,
    /// The referee's **T2** worker budget. `Threads::single()` unless a caller
    /// says otherwise; [`RoutingJob::with_stage`] adopts the routing stage's own
    /// `--threads`, so the router and the referee never disagree about how much
    /// of the machine this job may use.
    referee_threads: Threads,
    /// Where [`RoutingJob::run`] puts the session, before it judges the board.
    /// `None` leaves the caller to write [`JobReport::ses`] itself, which then
    /// necessarily happens *after* the referee.
    session_path: Option<PathBuf>,
}

impl std::fmt::Debug for RoutingJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoutingJob")
            .field("design_name", &self.design_name)
            .field("source_bytes", &self.source.len())
            .field("import_options", &self.import_options)
            .field("weights", &self.weights)
            .field("stage", &self.stage)
            .field("referee_threads", &self.referee_threads)
            .field("session_path", &self.session_path)
            .finish()
    }
}

impl RoutingJob {
    /// Builds a job over DSN bytes.
    ///
    /// `design_name` is the `(base_design …)` token of the session file. Upstream passes the
    /// *file* name there, not the `(pcb <name>)` token, which is why it is a parameter and not
    /// read off the design.
    pub fn new(source: impl Into<Vec<u8>>, design_name: impl Into<String>) -> RoutingJob {
        RoutingJob {
            source: source.into(),
            design_name: design_name.into(),
            import_options: ImportOptions::default(),
            weights: ScoreWeights::V2,
            stage: RouterStage::default(),
            referee_threads: Threads::single(),
            session_path: None,
        }
    }

    /// Builds a job from a path, taking the file name as the design name.
    pub fn from_path(path: impl AsRef<Path>) -> Result<RoutingJob, JobError> {
        let path = path.as_ref();
        let bytes =
            std::fs::read(path).map_err(|error| JobError::Read(path.to_path_buf(), error))?;
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(RoutingJob::new(bytes, name))
    }

    /// The post-load overrides the import runs. Defaults to the shipped ones; `java -drc` runs
    /// them off (see [`crate::drc_report::DRC_IMPORT_OPTIONS`]).
    pub fn with_import_options(mut self, options: ImportOptions) -> RoutingJob {
        self.import_options = options;
        self
    }

    /// The scoring weights. Defaults to [`ScoreWeights::V2`], the 2.3.0 numbers.
    pub fn with_weights(mut self, weights: ScoreWeights) -> RoutingJob {
        self.weights = weights;
        self
    }

    /// Selects the router stage. Only [`RouterStage::ImportOnly`] exists in M1.
    pub fn with_stage(mut self, stage: RouterStage) -> RoutingJob {
        // A routing stage states the machine budget for the whole job, referee
        // included: `--threads` is one number, not two.
        if let RouterStage::Maze(options) = stage {
            self.referee_threads = options.threads();
        }
        self.stage = stage;
        self
    }

    /// Overrides the referee's worker budget, for a job with no routing stage
    /// (`--import-only`, the `-drc` and `-do` oracles) that still wants T2.
    pub fn with_referee_threads(mut self, threads: Threads) -> RoutingJob {
        self.referee_threads = threads;
        self
    }

    /// Asks [`RoutingJob::run`] to write the session to `path` itself — *before*
    /// it judges the board (see the module docs).
    ///
    /// This is the only way to get the ordering guarantee. A caller that writes
    /// [`JobReport::ses`] after `run` returns necessarily writes it after the
    /// referee, and a kill in between costs it the file.
    pub fn with_session_path(mut self, path: impl Into<PathBuf>) -> RoutingJob {
        self.session_path = Some(path.into());
        self
    }

    /// The `(base_design …)` token this job will write.
    pub fn design_name(&self) -> &str {
        &self.design_name
    }

    /// Stage 1 on its own: DSN bytes → board + rules.
    ///
    /// Exposed because the two gate emitters ([`fr_io::golden_dump`] and
    /// [`crate::drc_report`]) need the design and nothing downstream of it.
    pub fn import(&self) -> Result<ImportedDesign, JobError> {
        self.import_with(self.import_options)
    }

    /// [`RoutingJob::import`] with a one-off override of the post-load options.
    ///
    /// The `-drc` and `-do` oracles were generated under *different* override settings
    /// ([`crate::drc_report::DRC_IMPORT_OPTIONS`]), so a caller emitting both documents has to
    /// import twice. Taking the options here rather than through
    /// [`RoutingJob::with_import_options`] is what keeps that from cloning the whole design
    /// file, which on the larger fixtures is tens of megabytes.
    pub fn import_with(&self, options: ImportOptions) -> Result<ImportedDesign, JobError> {
        specctra::import_with_options(&self.source, options).map_err(JobError::Import)
    }

    /// The whole pipeline.
    pub fn run(&self) -> Result<JobReport, JobError> {
        self.run_observed(&mut |_| {})
    }

    /// [`RoutingJob::run`], announcing each phase as it completes.
    ///
    /// The observer exists so that the *order* of the phases is testable from
    /// outside: "the session is on disk before the referee starts" is a
    /// promise about a run that gets killed, and the only honest way to check
    /// it without killing anything is to look at the world between the phases.
    /// It is not a progress API — the callback runs on the job's own thread and
    /// blocks it.
    pub fn run_observed(
        &self,
        observe: &mut dyn FnMut(JobPhase),
    ) -> Result<JobReport, JobError> {
        let import_started = std::time::Instant::now();
        let mut design = self.import()?;
        let import_duration = import_started.elapsed();
        // Taken here, before anything can move copper. `census` below is the same count over the
        // *finished* board, and the two differ by exactly what the router added — which is the
        // whole reason both exist.
        let import_census = BoardCensus::of(&design.board);
        observe(JobPhase::Imported);
        // The board is known now, so the job's wall budget can finally reserve what its own
        // shutdown will cost (`fr_router::reserve_job_tail`). Before the import there is
        // nothing to size it from, and after the router stage it is too late — the stage is
        // what has to stop early.
        let reserved = fr_router::reserve_job_tail(tail_reserve_for(&design));
        // The router stage. `ImportOnly` is a no-op *by construction*: its arm does not receive
        // `&mut design`, so no future edit to it can start mutating the board by accident.
        let router = match self.stage {
            RouterStage::ImportOnly => None,
            RouterStage::Maze(options) => Some(run_maze_stage(&mut design, options)),
        };
        let routed = router.is_some();
        observe(JobPhase::Routed);
        // Session first, referee second — the module docs argue the order. Everything below
        // this line is a description of a board that is already on disk.
        let session = write_session(&design, &self.design_name);
        if let Some(path) = &self.session_path {
            write_session_file(path, &session.text)
                .map_err(|error| JobError::Write(path.clone(), error))?;
        }
        observe(JobPhase::SessionWritten);
        let referee = Referee::judge_with_threads(&design, &self.weights, self.referee_threads);
        observe(JobPhase::Judged);
        Ok(JobReport {
            session_path: self.session_path.clone(),
            tail_reserve: reserved,
            design_name: self.design_name.clone(),
            pcb_name: design.design_name.clone(),
            layer_count: design.board.layers().len(),
            import_census,
            census: BoardCensus::of(&design.board),
            stats: referee.stats,
            score: referee.score,
            violations: referee.violations,
            connectivity: referee.connectivity,
            ses: session.text,
            warnings: design
                .warnings
                .iter()
                .cloned()
                .chain(session.warnings)
                .collect(),
            routed,
            router,
            import_duration,
            design,
        })
    }
}

/// Everything one job run produced.
pub struct JobReport {
    /// The `(base_design …)` token, i.e. the input file name.
    pub design_name: String,
    /// The `(pcb <name>)` token.
    pub pcb_name: String,
    /// Layers in the board stackup.
    pub layer_count: usize,
    /// Item counts by kind **as the DSN was imported**, before any router stage ran.
    ///
    /// Kept apart from [`JobReport::census`] for the same reason
    /// [`JobReport::import_duration`] is kept apart from the caller's wall clock: `fr-cli`'s
    /// summary opens with a line that says "Imported …", and it was printing the post-routing
    /// count there, so on a routed board it reported items the importer never saw. A line that
    /// names a stage has to carry that stage's numbers.
    pub import_census: BoardCensus,
    /// Item counts by kind on the **finished** board — after the router stage when one ran, and
    /// therefore equal to [`JobReport::import_census`] only under
    /// [`RouterStage::ImportOnly`].
    pub census: BoardCensus,
    /// The five referee quantities the score is computed from.
    pub stats: BoardStats,
    /// The v2 score.
    pub score: Score,
    /// The deduplicated clearance violations.
    pub violations: Vec<Violation>,
    /// Per-net connectivity, incompletes and airlines.
    pub connectivity: BoardConnectivity,
    /// The session file text.
    pub ses: String,
    /// Importer and writer warnings, in that order.
    pub warnings: Vec<String>,
    /// True when a router stage ran, i.e. when the SES carries our work rather than a
    /// passthrough of the imported wiring.
    pub routed: bool,
    /// What the router stage did; `None` under [`RouterStage::ImportOnly`].
    pub router: Option<RouterReport>,
    /// How long stage 1 — DSN bytes to board — took, and *only* that.
    ///
    /// Kept apart from the caller's own wall clock because the two are easy to
    /// confuse and the difference is the whole router: `fr-cli` printed its
    /// end-to-end elapsed time on a line that says "Imported … in", which made
    /// every summary claim the import had cost what the run did.
    pub import_duration: std::time::Duration,
    /// Where the session was written, when the job wrote it itself
    /// ([`RoutingJob::with_session_path`]). `None` means the caller still has
    /// to put `ses` somewhere.
    pub session_path: Option<PathBuf>,
    /// What the job took out of its wall budget to pay for its own shutdown
    /// ([`fr_router::reserve_job_tail`]); zero when no budget was armed.
    pub tail_reserve: std::time::Duration,
    /// The imported design, kept so a caller can re-read the session against it or emit a
    /// golden dump without importing twice.
    pub design: ImportedDesign,
}

impl JobReport {
    /// No violations and nothing left unconnected.
    ///
    /// On an M1 run this describes the *input* board, not our work: a fully pre-routed fixture
    /// is clean and an unrouted one is not, and neither says anything about a router that has
    /// not been written yet.
    pub fn is_clean(&self) -> bool {
        self.stats.clearance_violation_count == 0 && self.stats.incomplete_count == 0
    }
}

/// The referee's verdict on one board.
pub struct Referee {
    /// `check_clearance`, deduplicated per `(min_id, max_id, layer)`.
    pub violations: Vec<Violation>,
    /// `board_connectivity`.
    pub connectivity: BoardConnectivity,
    /// The score inputs, assembled from both plus the board's own geometry.
    pub stats: BoardStats,
    /// `score(stats, weights)`.
    pub score: Score,
}

impl Referee {
    /// Runs the clearance DRC and the ratsnest, then scores the result.
    pub fn judge(design: &ImportedDesign, weights: &ScoreWeights) -> Referee {
        Referee::judge_with_threads(design, weights, Threads::single())
    }

    /// [`Referee::judge`] with **T2**: the violation scan fanned out over
    /// `threads` workers.
    ///
    /// Safe to turn on by default, and turned on by default, because
    /// `fr_check::check_clearance_parallel` returns *the same `Vec<Violation>`*
    /// as the sequential scan — not an equivalent multiset, the same records in
    /// the same order — which
    /// `fr-check/tests/property_test.rs::the_parallel_scan_returns_exactly_the_sequential_result`
    /// pins against the sequential scan on 25 random boards at three thread
    /// counts. A referee that could disagree with itself would be worthless: it
    /// judges both tools' output in the bench.
    ///
    /// The ratsnest is **not** parallelised. `ARCHITECTURE.md` already predicted
    /// why — "incremental connectivity deletes much of what T2 would
    /// parallelize" — and the profile agrees: `board_connectivity` is a
    /// union-find over an anchor index, linear and cheap next to the
    /// `O(candidates)` shape-intersection scan next to it.
    pub fn judge_with_threads(
        design: &ImportedDesign,
        weights: &ScoreWeights,
        threads: Threads,
    ) -> Referee {
        let board = &design.board;
        let shapes = shape_store(board);
        let view = StoreView::new(
            board.store(),
            &shapes,
            &design.rules.clearance_matrix,
            board.layers().len(),
        );
        let anchors = AnchorIndex::build(&view);
        let violations = check_clearance_parallel(&view, &anchors, threads.get());
        let connectivity = board_connectivity(&view);
        let mut stats = board_geometry_stats(design);
        stats.max_connections = connectivity.max_connections as u64;
        stats.incomplete_count = connectivity.incomplete_count as u64;
        stats.clearance_violation_count = violations.len() as u64;
        let score = score(&stats, weights);
        Referee {
            violations,
            connectivity,
            stats,
            score,
        }
    }
}

/// The three geometry-derived score inputs: bends, trace length, vias.
///
/// `BoardStatistics.java:150-336`, transcribed:
///
/// - **bends** = `Σ over traces with cornerCount >= 3 of (cornerCount - 2)` (`:252-268`). A
///   two-corner trace is straight and contributes nothing.
/// - **trace length** = `Σ PolylineTrace.get_length()` over *every* trace regardless of fixed
///   state (`:171-176`), converted with `Unit.scale(1, boardUnit, MM) / max(resolution, 1)`
///   (`:194-203`).
/// - **vias** = `board.get_vias().size()` (`:308`).
///
/// # Two things that look like bugs and are the contract
///
/// 1. The unit conversion divides by the DSN **resolution**, not by the board **scale factor**.
///    They differ exactly when `Structure.createBoard`'s integer-division reduction fired
///    (`transform.rs`, §3.2), and on such a board upstream's millimetres are off by the
///    reduction ratio. Reproduced, because the score is a comparison target.
/// 2. `plane_via_count` is left at zero and every via is charged at `via_costs`. Upstream's
///    score line is `this.vias.totalCount * scoringSettings.viaCosts` (`:566`) — `planeViaCosts`
///    is a *router* cost, never a *scoring* one (`batch-orchestration.md` §6.1, row 76).
fn board_geometry_stats(design: &ImportedDesign) -> BoardStats {
    // `Unit.scale(1.0, unit, UM) / max(resolution, 1)` — micrometres per board unit.
    let resolution = f64::from(design.resolution.max(1));
    let board_unit_to_um = Unit::scale(1.0, design.unit, Unit::Um) / resolution;

    let mut bend_count = 0u64;
    let mut trace_length_board = 0.0f64;
    let mut via_count = 0u64;
    for (_, item) in design.board.store().iter() {
        match &item.kind {
            ItemKind::Trace(trace) => {
                let corners = trace.corner_count();
                if corners >= 3 {
                    bend_count += (corners - 2) as u64;
                }
                trace_length_board += trace.polyline.length_total();
            }
            ItemKind::Via(_) => via_count += 1,
            _ => {}
        }
    }

    BoardStats {
        bend_count,
        // `f64 -> u64` saturates at 0 for negatives and at u64::MAX for overflow, neither of
        // which a length can reach; a NaN would become 0, and a NaN length is a geometry bug
        // that the board consistency check catches first.
        trace_length_um: (trace_length_board * board_unit_to_um).round() as u64,
        via_count,
        plane_via_count: 0,
        ..BoardStats::default()
    }
}

/// Item counts by kind — the cheap summary a CLI prints and a test asserts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BoardCensus {
    /// Component pads.
    pub pins: usize,
    /// Vias.
    pub vias: usize,
    /// Traces.
    pub traces: usize,
    /// Keepouts of all three flavours.
    pub obstacle_areas: usize,
    /// Copper pours and power planes.
    pub conduction_areas: usize,
    /// Board outline items.
    pub board_outlines: usize,
    /// Component metadata geometry.
    pub component_outlines: usize,
}

impl BoardCensus {
    /// Counts every item in the store.
    pub fn of(board: &Board) -> BoardCensus {
        let mut census = BoardCensus::default();
        for (_, item) in board.store().iter() {
            match &item.kind {
                ItemKind::Pin(_) => census.pins += 1,
                ItemKind::Via(_) => census.vias += 1,
                ItemKind::Trace(_) => census.traces += 1,
                ItemKind::ObstacleArea(_) => census.obstacle_areas += 1,
                ItemKind::ConductionArea(_) => census.conduction_areas += 1,
                ItemKind::BoardOutline(_) => census.board_outlines += 1,
                ItemKind::ComponentOutline(_) => census.component_outlines += 1,
            }
        }
        census
    }

    /// Every item, of every kind.
    pub fn total(&self) -> usize {
        self.pins
            + self.vias
            + self.traces
            + self.obstacle_areas
            + self.conduction_areas
            + self.board_outlines
            + self.component_outlines
    }
}

/// Stage 4 on its own: the v2 session writer, with endpoint snapping.
///
/// Separate from [`RoutingJob::run`] so that a caller holding a board a router has just edited
/// can write it without rebuilding the job.
pub fn write_session(design: &ImportedDesign, design_name: &str) -> specctra::SesOutput {
    specctra::write_session_verbose(&SesInput::from_design(design, design_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two pins on one net, one straight `(type route)` wire between them, on a 10 mm board.
    ///
    /// Every number the assertions below use is derived from this text: `(resolution um 10)`
    /// with a 10 000-unit outline keeps the scale factor at 10 (`transform.rs`), so a board unit
    /// is 0.1 µm and the wire's 5000 DSN µm are 50 000 board units.
    const BOARD: &str = r#"(pcb pipeline_smoke
  (parser (string_quote "))
  (resolution um 10)
  (unit um)
  (structure
    (layer F.Cu (type signal))
    (layer B.Cu (type signal))
    (boundary (path signal 0 0 0 10000 0 10000 10000 0 10000 0 0))
    (rule (width 250) (clearance 200))
  )
  (placement
    (component pad2
      (place P1 1000 1000 front 0)
      (place P2 6000 1000 front 0)
    )
  )
  (library
    (image pad2 (pin round_pad 1 0 0))
    (padstack round_pad (shape (circle F.Cu 400)) (shape (circle B.Cu 400)) (attach off))
  )
  (network
    (net SIG (pins P1-1 P2-1))
    (class default SIG (rule (width 250) (clearance 200)))
  )
  (wiring
    (wire (path F.Cu 250 1000 1000 6000 1000) (net SIG) (type route))
  )
)"#;

    fn job() -> RoutingJob {
        RoutingJob::new(BOARD.as_bytes(), "pipeline_smoke.dsn")
    }

    #[test]
    fn the_pipeline_imports_judges_and_writes() {
        let report = job().run().expect("the board imports");
        assert_eq!(report.pcb_name, "pipeline_smoke");
        assert_eq!(report.layer_count, 2);
        assert_eq!(report.census.pins, 2);
        assert_eq!(report.census.traces, 1);
        // The session is the v2 writer's, not the pre-rewrite one's: `(base_design …)` carries
        // the *file* name we passed, and the session name is that with `.dsn` -> `.ses`. Both
        // are quoted, because `_` is in `SES_RESERVED_CHARS` (v2 widened the set so KiCad net
        // names survive a round trip).
        assert!(
            report.ses.starts_with("(session \"pipeline_smoke.ses\""),
            "{}",
            &report.ses[..report.ses.len().min(60)]
        );
        assert!(report.ses.contains("(base_design \"pipeline_smoke.dsn\")"));
        assert!(report.ses.contains("(routes"));
    }

    #[test]
    fn nothing_is_routed_and_the_report_says_so() {
        let report = job().run().expect("the board imports");
        assert!(!report.routed, "M1 must not route");
        // The one wire in the fixture is the one wire in the session: a passthrough, not a route.
        assert_eq!(report.census.traces, 1);
    }

    /// The wire spans 5000 µm between the two pad centres, so the net is complete and the board
    /// is clean — which makes the score the maximum, 1000.
    #[test]
    fn a_complete_two_pin_net_scores_a_thousand() {
        let report = job().run().expect("the board imports");
        assert_eq!(report.stats.max_connections, 1);
        assert_eq!(report.stats.incomplete_count, 0);
        assert_eq!(report.stats.clearance_violation_count, 0);
        assert!(report.is_clean());
        // maximum 5e6, penalties 0, costs = 5000 µm * 1.0/mm + 0 vias = 5.0.
        assert_eq!(report.score.maximum, 5_000_000.0);
        assert_eq!(report.score.penalties, 0.0);
        assert!(
            (report.score.normalized - 999.999).abs() < 0.01,
            "{}",
            report.score.normalized
        );
    }

    /// A straight two-corner wire has no bends, and its length converts through the DSN
    /// resolution: 50 000 board units * (1 µm / 10) = 5000 µm.
    #[test]
    fn geometry_stats_are_converted_through_the_resolution() {
        let design = job().import().expect("the board imports");
        let stats = board_geometry_stats(&design);
        assert_eq!(stats.bend_count, 0);
        assert_eq!(stats.trace_length_um, 5_000);
        assert_eq!(stats.via_count, 0);
        assert_eq!(stats.plane_via_count, 0);
    }

    /// The stage sequencing and the default the release ships: router, then
    /// optimizer, and the optimizer is **on** unless asked otherwise.
    #[test]
    fn the_optimizer_stage_runs_after_the_router_and_is_on_by_default() {
        assert!(MazeOptions::default().optimize);
        assert!(MazeOptions::with_passes(3).optimize);

        let on = job()
            .with_stage(RouterStage::Maze(MazeOptions::with_passes(1)))
            .run()
            .expect("the board imports");
        let router = on.router.clone().expect("the stage reports");
        let optimizer = router.optimizer.expect("the optimizer ran");
        // The fixture arrives already wired, so the router finds nothing and
        // the optimizer's near-perfect early exit fires before its first pass
        // (`BatchOptimizer.java:141-147`) — which is still the stage running.
        assert_eq!(optimizer.exit, "near-maximum score", "{optimizer:?}");
        assert_eq!(optimizer.items_optimized, 0, "{optimizer:?}");
        assert!(optimizer.score_before > 999.0, "{optimizer:?}");
        assert_eq!(optimizer.score_after, optimizer.score_before);
        assert_eq!(optimizer.incomplete_count, 0);

        let off = job()
            .with_stage(RouterStage::Maze(
                MazeOptions::with_passes(1).with_optimizer(false),
            ))
            .run()
            .expect("the board imports");
        assert!(off
            .router
            .clone()
            .expect("the stage reports")
            .optimizer
            .is_none());
        // Switching a stage that had nothing to do off changes no copper.
        assert_eq!(on.census, off.census);
    }

    /// The import clock is the *import's*, not the run's — the whole point of
    /// [`JobReport::import_duration`].
    #[test]
    fn the_import_duration_excludes_the_router() {
        let import_only = job().run().expect("the board imports");
        let routed = job()
            .with_stage(RouterStage::Maze(MazeOptions::with_passes(1)))
            .run()
            .expect("the board imports");
        // Not a timing assertion — a same-order-of-magnitude one. Both jobs
        // import the same bytes; if the field were the run's elapsed time the
        // routed one would carry the router's cost too.
        assert!(routed.import_duration < std::time::Duration::from_secs(1));
        assert!(import_only.import_duration < std::time::Duration::from_secs(1));
    }

    #[test]
    fn a_non_dsn_input_is_a_typed_error_not_a_panic() {
        let job = RoutingJob::new(b"not a board".to_vec(), "junk.dsn");
        match job.run() {
            Err(JobError::Import(ImportError::NotADsnFile)) => {}
            Err(other) => panic!("wrong error: {other}"),
            Ok(_) => panic!("junk must not import"),
        }
    }

    #[test]
    fn a_missing_file_is_a_read_error() {
        match RoutingJob::from_path("/nonexistent/board.dsn") {
            Err(JobError::Read(path, _)) => assert!(path.ends_with("board.dsn")),
            _ => panic!("a missing file must be a read error"),
        }
    }
}
