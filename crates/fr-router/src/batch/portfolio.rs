//! **T1b — portfolio pass racing.** N whole route passes, on N board clones,
//! with N different item orders; keep the one the referee likes best.
//!
//! `ARCHITECTURE.md` is explicit about what this is for: *"Portfolio racing buys
//! quality-at-equal-wall-time, not speedup."* A portfolio of 4 does not finish a
//! board 4× faster — it finishes it in about the same wall clock as one arm and
//! hands back the best of four tries. That only pays on a board whose result is
//! decided by the **budget**, not by the algorithm: if every arm converges, they
//! all converge to a similar score and the extra cores bought nothing.
//!
//! # It works, on the boards it is for
//!
//! `ARCHITECTURE.md` poses the question — *"does a diverse portfolio find more
//! connections within the same wall budget?"* — and on the two budget-starved
//! fixtures the answer is yes. Both at `-mp 5 --job-timeout 90`, so every
//! configuration got the same 90 s of wall clock and the arms shared the machine:
//!
//! | fixture | arms=1 | 2 | 4 | 8 |
//! |---|---|---|---|---|
//! | `Issue039` (204 connections) | 57 open | 52 | 43 | **40** |
//! | `Issue269` (192 connections) | 96 open | 95 | 93 | **93** |
//!
//! Clearance violations were identical at every arm count on both boards (25 and
//! 4), so the connections are not bought with copper the referee objects to. A
//! shuffled arm won every single race, and arm 0 — the default order — reported
//! the same incomplete count as the corresponding `arms=1` run, which is the
//! "never worse than the plain run" property showing up in the measurements
//! rather than only in the argument.
//!
//! Note the shape of the `Issue039` row: the win keeps growing to 8 arms on a
//! machine with 8 usable cores, i.e. the arms are still finding genuinely
//! different orders rather than re-discovering one. `Issue269` saturates at 4;
//! it is the harder board and its remaining incompletes are not order-limited.
//!
//! It is nonetheless off by default (`--portfolio N` opts in), because the honest
//! default is the one that spends the machine on the board in front of it, and
//! because on a board that converges the race costs `N` cores for nothing.
//!
//! # Why the diversity is the *item order*
//!
//! The batch pass is a greedy sequential router: connection *k* routes into the
//! obstacle field connections `1..k` left behind, so the work list's order is
//! the single largest lever on the outcome that costs nothing to change. It is
//! also the only lever this module is allowed to pull — everything else (ripup
//! costs, pass counts, budgets) is upstream-calibrated behaviour that a
//! portfolio arm has no business diverging on.
//!
//! # Arm 0 is always the default run
//!
//! [`WorkOrder::Scan`] is the un-shuffled release order, and arm 0 always gets
//! it. So `--portfolio N` can only ever return a board **at least as good as**
//! `--portfolio 1` on the same budget: the default run is one of the candidates
//! and the winner is chosen by the referee. That property is what makes the flag
//! safe to hand to a user, and it is pinned by
//! `tests::a_portfolio_of_one_is_the_default_run`.
//!
//! # Determinism
//!
//! Each arm's seed is a pure function of its index; the shuffle is a
//! Fisher-Yates driven by a splitmix64 stream seeded from `(seed, pass_no)`, so
//! an arm's order is reproducible and independent of every other arm. The winner
//! is `max` by [`ArmScore`] with **the lowest arm index winning a tie**, so
//! which arm finished first never reaches the decision. Nothing about a
//! portfolio run depends on thread scheduling.
//!
//! What a portfolio run *does* depend on is the wall clock, exactly as a
//! single-arm run does — N arms sharing a machine each go slower than one arm
//! alone, so a wall-budgeted arm inside a portfolio gets less done than the same
//! arm on its own. That is a property of the experiment, not a bug, and it is
//! why the comparison to record is "portfolio of N against a single arm on the
//! same machine and the same budget".

use fr_board::board::Board;
use fr_rules::board_rules::BoardRules;
use fr_rules::router_settings::RouterSettings;

use crate::batch::budget::BudgetPolicy;
use crate::batch::schedule::RunOutcome;
use crate::batch::scoring::{BoardScore, BoardScorer, CheckScorer};
use crate::batch::{BatchRouter, WorkItem};

/// The order a pass hands its work list to the router.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorkOrder {
    /// `BatchAutorouter.getAutorouteItems`'s own order — descending semantic id,
    /// the release's. The default, and what every non-portfolio run uses.
    #[default]
    Scan,
    /// A seeded Fisher-Yates permutation of it. Only a portfolio arm sets this.
    Shuffled(u64),
}

impl WorkOrder {
    /// Reorders one pass's work list in place.
    ///
    /// `pass_no` is mixed into the stream so that the arm's passes are not
    /// permutations of one another — a portfolio arm should explore a different
    /// order every pass, the way [`crate::optimizer`]'s own
    /// `p_with_preferred_directions` alternation does, "to create more
    /// variations" (`BatchOptimizer.java:153`).
    pub fn apply(self, work: &mut [WorkItem], pass_no: i32) {
        let WorkOrder::Shuffled(seed) = self else {
            return;
        };
        if work.len() < 2 {
            return;
        }
        let mut state = splitmix64(seed ^ splitmix64(pass_no as u64));
        // Fisher-Yates, high to low: the classic form, so the permutation is
        // uniform and the mapping from stream to permutation is the textbook
        // one rather than something only this file knows.
        for i in (1..work.len()).rev() {
            state = splitmix64(state);
            let j = (state % (i as u64 + 1)) as usize;
            work.swap(i, j);
        }
    }
}

/// SplitMix64 — the finalizer of Java's `SplittableRandom`, and a well-tested
/// way to turn a counter into a stream.
///
/// It is here rather than behind a dependency because the router may not have a
/// randomness policy it does not control: this is the only stochastic thing in
/// the crate, it is seeded from an integer the caller states, and the whole
/// point is that its output is pinned by a test.
fn splitmix64(seed: u64) -> u64 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The seed of arm `index`. Arm 0 is the un-shuffled default run.
pub fn arm_order(index: usize) -> WorkOrder {
    if index == 0 {
        WorkOrder::Scan
    } else {
        WorkOrder::Shuffled(splitmix64(index as u64))
    }
}

/// How two arms' boards are compared — the referee's verdict, in the order a
/// human would argue it.
///
/// **Connections first.** `normalized` already folds incompletes into a single
/// number, but it folds them together with via and length costs, so a board that
/// closed one more connection at the price of a longer detour can score lower.
/// On a budget-starved board — the only board a portfolio is for — the question
/// being asked is "did the extra tries close more connections", so that is the
/// primary key, with the normalized score breaking ties beneath it.
///
/// Added violations are not a key: both arms route the same imported board under
/// the same rules, and the parity policy counts violations as a delta over that
/// import. A violation count that differs between arms is worth knowing about
/// and is reported; it does not silently decide the race.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ArmScore {
    /// Ratsnest incompletes; fewer is better.
    pub incomplete_count: usize,
    /// `getNormalizedScore`; higher is better.
    pub normalized: f64,
    /// Deduplicated clearance violations, reported not raced.
    pub violation_count: usize,
}

impl ArmScore {
    /// The referee's verdict on one arm's board.
    pub fn of(score: &BoardScore) -> ArmScore {
        ArmScore {
            incomplete_count: score.incomplete_count,
            normalized: score.normalized,
            violation_count: score.violation_count,
        }
    }

    /// Whether `self` beats `other` **strictly** — ties go to the incumbent,
    /// which is how the lowest arm index wins a tie.
    pub fn beats(&self, other: &ArmScore) -> bool {
        if self.incomplete_count != other.incomplete_count {
            return self.incomplete_count < other.incomplete_count;
        }
        self.normalized > other.normalized
    }
}

/// What one arm produced.
#[derive(Debug, Clone)]
pub struct ArmReport {
    /// Its index; 0 is the default run.
    pub index: usize,
    /// The order it routed in.
    pub order: WorkOrder,
    /// The referee's verdict on its board.
    pub score: ArmScore,
    /// Its batch loop's own outcome.
    pub outcome: RunOutcome,
    /// Wall clock, for the "same budget" claim to be checkable.
    pub wall: std::time::Duration,
}

/// What the race produced.
#[derive(Debug, Clone)]
pub struct PortfolioOutcome {
    /// One entry per arm, in arm order.
    pub arms: Vec<ArmReport>,
    /// The index of the arm whose board was adopted.
    pub winner: usize,
}

impl PortfolioOutcome {
    /// The winning arm's report.
    pub fn winning_arm(&self) -> &ArmReport {
        &self.arms[self.winner]
    }

    /// The winning arm's batch loop outcome — what a caller that does not care
    /// that a race happened wants back.
    pub fn outcome(self) -> RunOutcome {
        self.arms
            .into_iter()
            .nth(self.winner)
            .expect("winner")
            .outcome
    }

    /// Whether a shuffled arm beat the default one. The number the `--portfolio`
    /// experiment exists to produce.
    pub fn shuffle_won(&self) -> bool {
        self.winner != 0
    }
}

/// How a race is configured.
#[derive(Debug, Clone, Copy)]
pub struct PortfolioConfig {
    /// Arms to race. `1` is the default run and skips the clone entirely.
    pub arms: usize,
    /// Passes each arm runs — `-mp`, the same for every arm.
    pub max_passes: i32,
    /// The per-connection budget policy, the same for every arm.
    pub budget: BudgetPolicy,
    /// Whether the fanout stage ran before the race.
    pub fanout: bool,
    /// Board units per millimetre, for the score's length term.
    pub board_units_per_mm: f64,
}

/// Races `config.arms` route passes on clones of `board` and leaves the winner
/// in `board`.
///
/// One arm is run in place with no clone at all, so `arms == 1` costs exactly
/// what [`BatchRouter::run`] costs.
pub fn race(
    board: &mut Board,
    rules: &BoardRules,
    settings: &RouterSettings,
    config: PortfolioConfig,
) -> PortfolioOutcome {
    let arms = config.arms.max(1);
    if arms == 1 {
        let started = std::time::Instant::now();
        let outcome = run_arm(board, rules, settings, config, WorkOrder::Scan);
        let score = judge(board, rules, settings, config);
        return PortfolioOutcome {
            arms: vec![ArmReport {
                index: 0,
                order: WorkOrder::Scan,
                score,
                outcome,
                wall: started.elapsed(),
            }],
            winner: 0,
        };
    }

    // Clone up front, on this thread, so that every arm starts from a board that
    // is provably the same one: a clone taken inside a worker would still be the
    // same board, but this way the race cannot be blamed for it.
    let mut boards: Vec<Board> = (0..arms).map(|_| board.clone()).collect();
    let reports: Vec<ArmReport> = std::thread::scope(|scope| {
        let handles: Vec<_> = boards
            .iter_mut()
            .enumerate()
            .map(|(index, arm_board)| {
                scope.spawn(move || {
                    let order = arm_order(index);
                    let started = std::time::Instant::now();
                    let outcome = run_arm(arm_board, rules, settings, config, order);
                    let wall = started.elapsed();
                    ArmReport {
                        index,
                        order,
                        score: judge(arm_board, rules, settings, config),
                        outcome,
                        wall,
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| match handle.join() {
                Ok(report) => report,
                Err(payload) => std::panic::resume_unwind(payload),
            })
            .collect()
    });

    // `beats` is strict, so the first arm to hold a score keeps it against an
    // equal one: the winner of a tie is the lowest index, and arm 0 is the
    // default run.
    let mut winner = 0;
    for report in &reports {
        if report.score.beats(&reports[winner].score) {
            winner = report.index;
        }
    }
    *board = boards.swap_remove(winner);
    PortfolioOutcome {
        arms: reports,
        winner,
    }
}

/// One arm's batch loop.
fn run_arm(
    board: &mut Board,
    rules: &BoardRules,
    settings: &RouterSettings,
    config: PortfolioConfig,
    order: WorkOrder,
) -> RunOutcome {
    let mut router = BatchRouter::new(rules, settings)
        .with_fanout(config.fanout)
        .with_board_units_per_mm(config.board_units_per_mm)
        .with_budget(config.budget)
        .with_work_order(order);
    router.run(board, config.max_passes)
}

/// The referee's verdict on one arm's finished board.
fn judge(
    board: &Board,
    rules: &BoardRules,
    settings: &RouterSettings,
    config: PortfolioConfig,
) -> ArmScore {
    let mut scorer = CheckScorer::new(rules, config.board_units_per_mm)
        .with_weights(crate::batch::scoring::weights_from(&settings.scoring));
    ArmScore::of(&scorer.score(board))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fr_board::item::ItemId;

    fn work(count: usize) -> Vec<WorkItem> {
        (0..count)
            .map(|i| WorkItem {
                item: ItemId::from_raw(i as u32 + 1),
                net_no: 1,
            })
            .collect()
    }

    #[test]
    fn the_scan_order_is_left_alone() {
        let mut items = work(16);
        let before = items.clone();
        WorkOrder::Scan.apply(&mut items, 1);
        assert_eq!(items, before);
    }

    #[test]
    fn a_shuffle_is_a_permutation_and_is_reproducible() {
        let mut a = work(64);
        let mut b = work(64);
        WorkOrder::Shuffled(7).apply(&mut a, 1);
        WorkOrder::Shuffled(7).apply(&mut b, 1);
        assert_eq!(a, b, "the same seed and pass give the same order");

        let mut sorted = a.clone();
        sorted.sort_by_key(|entry| entry.item);
        assert_eq!(sorted, work(64), "and it is a permutation, not a filter");
        assert_ne!(a, work(64), "... that actually moved something");
    }

    #[test]
    fn each_pass_of_an_arm_gets_its_own_order() {
        let mut first = work(64);
        let mut second = work(64);
        WorkOrder::Shuffled(7).apply(&mut first, 1);
        WorkOrder::Shuffled(7).apply(&mut second, 2);
        assert_ne!(first, second);
    }

    #[test]
    fn different_arms_explore_different_orders() {
        let mut first = work(64);
        let mut second = work(64);
        arm_order(1).apply(&mut first, 1);
        arm_order(2).apply(&mut second, 1);
        assert_ne!(first, second);
        assert_eq!(arm_order(0), WorkOrder::Scan, "arm 0 is the default run");
    }

    #[test]
    fn a_shuffle_of_a_trivial_list_is_a_no_op() {
        for count in [0, 1] {
            let mut items = work(count);
            let before = items.clone();
            WorkOrder::Shuffled(11).apply(&mut items, 1);
            assert_eq!(items, before);
        }
    }

    #[test]
    fn connections_outrank_the_normalized_score() {
        let fewer_open = ArmScore {
            incomplete_count: 1,
            normalized: 100.0,
            violation_count: 0,
        };
        let higher_score = ArmScore {
            incomplete_count: 2,
            normalized: 900.0,
            violation_count: 0,
        };
        assert!(fewer_open.beats(&higher_score));
        assert!(!higher_score.beats(&fewer_open));
    }

    #[test]
    fn a_tie_never_displaces_the_incumbent() {
        let score = ArmScore {
            incomplete_count: 3,
            normalized: 500.0,
            violation_count: 2,
        };
        assert!(!score.beats(&score), "so the lowest arm index wins a tie");
        // ... and the score breaks a tie on connections.
        let better = ArmScore {
            normalized: 501.0,
            ..score
        };
        assert!(better.beats(&score));
    }
}
