//! **T1 — the parallel optimizer.** Clone-eval-promote over the item sweep.
//!
//! `ARCHITECTURE.md` "Parallelism": *clone-eval-promote is upstream-proven
//! sound*. The variant upstream ships and its own authors warn about
//! (`BatchOptimizerMultiThreaded`, "multi-threaded route optimization is
//! broken") is the **greedy shared-master** one — N threads rip and re-route on
//! *the same* board under a lock, so a thread's `boardStatisticsBefore` is a
//! measurement of a board that no longer exists by the time it decides, and its
//! `undo()` rolls back whatever landed in between. That design is not copied
//! here and must never be.
//!
//! # The shape
//!
//! ```text
//! per window of W items drawn from the sweep against the live board:
//!     evaluate   N workers, each on its own Board clone, each item ripped,
//!                re-routed and **always rolled back** — a worker produces a
//!                verdict, never copper
//!     promote    one thread, walking the window in sweep order, re-running
//!                every Improved candidate on the live board for real
//! ```
//!
//! ## Soundness
//!
//! The live board is mutated **only** by
//! [`BatchOptimizer::opt_route_item`](super::BatchOptimizer::opt_route_item)
//! calls made by the promoter, on the promoter's thread, against the live board
//! itself. Worker clones are separate `Board`s that no one else can see and that
//! are rolled back before they are reused. So every mutation the parallel
//! optimizer performs is a mutation the *sequential* optimizer could have
//! performed at that moment, with the sequential optimizer's own
//! before/after measurement and its own commit-iff-improved rule. The board
//! therefore keeps the release's monotonicity property — `(incompletes, vias,
//! weighted length)` never rises — with no extra argument needed.
//!
//! What parallelism changes is only **which items get attempted**:
//!
//! * a `Rejected` verdict is trusted for the rest of the window, even though a
//!   promotion earlier in that same window may have changed the candidate's
//!   context. That can hide an improvement (a *false negative*); it can never
//!   invent one, because nothing is committed on a verdict — only on a live
//!   re-run. Window size bounds how stale a trusted verdict can be, and the next
//!   optimizer pass revisits every item anyway.
//! * `min_cumulative_trace_length` moves down as the promoter keeps items, while
//!   workers evaluate against the value at window start. Under both
//!   [`LengthMetric`](super::LengthMetric)s the `before` side is the floor an
//!   improvement must beat, so a *larger* floor is *more permissive*: a worker's
//!   `Rejected` therefore implies the promoter (with the same or a smaller
//!   floor) would also reject on the length branch, and a worker's `Improved` is
//!   re-checked for real. The drift is one-directional and in the safe
//!   direction.
//!
//! ## Determinism
//!
//! **The promotion order is the sweep order, regardless of completion order** —
//! the promoter walks `window` by index, and the verdict vector is filled by
//! index, so nothing about which worker finished first reaches a decision.
//!
//! That leaves one thing to pin: the verdict vector itself. A worker's clone
//! carries state that an evaluation advances and a rollback does **not** rewind
//! — chiefly `ItemStore`'s monotone id counter, which the journal cannot reuse.
//! Two evaluations of the same item on the same worker board therefore start
//! from different `next_id`s, and item id order is a parity-relevant iteration
//! order. So *which items a given worker sees, in which order* has to be a
//! function of the configuration and not of the clock. It is:
//! [`assign`] is a fixed round-robin `item index % worker count` — there is no
//! work stealing. The verdict for item `i` is then a pure function of
//! `(baseline board, window, thread count, i)`, and the whole stage is
//! reproducible at a fixed `--threads N`.
//!
//! It is **not** reproducible *across* thread counts, and it is not claimed to
//! be: `--threads 4` and `--threads 8` are two different configurations of a
//! heuristic, exactly as `-mp 3` and `-mp 4` are. `--deterministic` therefore
//! only accepts a thread count that was stated explicitly (see
//! `fr_core::pipeline`), because `available_parallelism` is a property of the
//! machine.
//!
//! Round-robin costs load balance — item costs vary by orders of magnitude and a
//! stealing scheduler would fill the gaps. That trade is deliberate and is the
//! reason the assignment is *interleaved* rather than blocked: a run of
//! expensive neighbouring items is spread across all workers instead of landing
//! on one.
//!
//! ## What a worker is not, and where that costs
//!
//! A worker never promotes, never writes to the live board, and never returns a
//! board. Every kept item is therefore routed **twice** — once by a worker to
//! get the verdict, once by the promoter to get the copper — and the second one
//! is on the critical path.
//!
//! That is this stage's Amdahl term, and it is not small on every board. Writing
//! `N` for the items a pass attempts, `W` for the ones it keeps, `T` for the
//! worker count and `c(i)` for an item's reroute cost:
//!
//! ```text
//! sequential  =  Σ_N c(i)
//! parallel    ≈  (Σ_N c(i)) / T   +   Σ_W c(i)
//! ```
//!
//! so the ceiling is `1 / (1/T + Σ_W c / Σ_N c)`. It is a *speedup* only when the
//! kept items are not where the time is. On `Issue034` they are exactly where
//! the time is — four kept items collapse a 290-connection backlog and the other
//! 85 attempts run against the cheap board that leaves behind — so `Σ_W c / Σ_N
//! c` is close to 1 and the measured stage went 23.0 s sequential → 26.1 s at
//! `--threads 4` → 24.4 s at `--threads 8`, with an identical board and score.
//! On a board whose optimizer time goes into *failed* attempts the same
//! arithmetic predicts a real win; `bench/` is where that gets settled per board,
//! and it is why `--threads` is opt-in.
//!
//! The obvious way to delete the second routing is to adopt the winner's store
//! outright. It is sound for the **first** promotion of a window and only that
//! one: until the promoter commits something, the live board is still
//! bit-identical to the baseline the workers cloned. It is deliberately not done
//! yet, because it makes the promoter's correctness depend on "nothing has landed
//! yet" — precisely the invariant the greedy shared-master variant gets wrong —
//! and that is a claim to make with a test that can fail, not in passing.

use std::sync::atomic::{AtomicBool, Ordering};

use fr_board::board::Board;
use fr_board::item::ItemId;

use crate::batch::threads::Threads;
use crate::optimizer::attempt::{attempt_item, AttemptConfig, AttemptScratch, Disposition};
use crate::optimizer::MazeRerouter;

/// The largest window, in items per worker.
pub const MAX_WINDOW_ITEMS_PER_WORKER: usize = 8;

/// The largest window a thread budget may reach.
pub fn max_window_size(threads: Threads) -> usize {
    threads
        .get()
        .saturating_mul(MAX_WINDOW_ITEMS_PER_WORKER)
        .max(1)
}

/// The window size, which adapts to whether the board is still moving.
///
/// # The measurement that produced this
///
/// A fixed window of `threads × 8` made the T1 optimizer **2.4× slower than
/// sequential** on `Issue034` at `--threads 4` (116 s against 48 s, for the
/// identical board and score). The reason is specific and worth writing down,
/// because it is the whole design constraint of this stage:
///
/// > *The cost of an item's reroute is a function of how much of the board is
/// > still unrouted.* `MazeRerouter` escalates over up to six
/// > `autoroute_pass`es and every one of them re-attempts the board's whole
/// > backlog. On `Issue034` the optimizer starts on a board with 290 failed
/// > connections and closes almost all of them in its first handful of kept
/// > items — the score goes 795.91 → 999.98 on four improvements. Sequentially,
/// > items 5..89 are therefore *cheap*, because the backlog they re-attempt is
/// > gone. A worker evaluating item 30 against the **window-start** board pays
/// > the full 290-connection price for it.
///
/// So a wide window does not merely risk a stale verdict, it risks paying a
/// stale *price*, and on a board whose early items collapse the backlog the
/// speculation loses more than the parallelism wins.
///
/// The window therefore starts at one item per worker — the narrowest window
/// that still uses every thread — and **doubles only after a window in which
/// the promoter kept nothing**, up to [`max_window_size`]. A window that kept
/// something drops it straight back to the floor. Long failure runs are exactly
/// where the sequential optimizer's cost per item is flat and where speculation
/// is free, and `optimizer.maxConsecutiveFailures = 50` says upstream expects
/// those runs to be the common case.
///
/// It stays deterministic because the schedule is a function of the *promotion
/// history*, which is itself deterministic (see the module docs): the same board
/// and the same thread count produce the same sequence of window sizes.
pub fn next_window_size(current: usize, threads: Threads, promoted_any: bool) -> usize {
    let floor = threads.get().max(1);
    if promoted_any {
        floor
    } else {
        current
            .max(floor)
            .saturating_mul(2)
            .min(max_window_size(threads))
    }
}

/// The window a pass starts with: one item per worker.
pub fn initial_window_size(threads: Threads) -> usize {
    threads.get().max(1)
}

/// What a worker found out about one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// No worker got to it — the window was cut short by the job deadline. The
    /// promoter runs it itself rather than guessing.
    Unevaluated,
    /// The reroute improved the board on the clone. The promoter re-runs it on
    /// the live board; that re-run's verdict, not this one, decides.
    Improved,
    /// The reroute did not improve the board on the clone, or the item could not
    /// be attempted at all (`ItemRouteResult::not_attempted`, which the
    /// sequential loop also counts as a failure). The promoter skips it.
    Rejected,
}

/// Which worker evaluates `index` under a budget of `workers` threads.
///
/// Fixed round-robin, no stealing — see the module docs on determinism.
#[inline]
pub fn assign(index: usize, workers: usize) -> usize {
    index % workers.max(1)
}

/// The worker boards, kept across windows.
///
/// A pool rather than a fresh clone per window because the reset is
/// `ARCHITECTURE.md` state mechanism 2: `Board::adopt_store` reconciles the
/// trees and the connectivity index from the *difference* between the worker's
/// store and the baseline's, which on a window that moved a handful of traces is
/// a handful of shape rebuilds instead of a whole board.
#[derive(Debug, Default)]
pub struct WorkerPool {
    boards: Vec<Board>,
}

impl WorkerPool {
    /// An empty pool; the first [`WorkerPool::sync_to`] fills it.
    pub fn new() -> WorkerPool {
        WorkerPool::default()
    }

    /// Worker boards currently held.
    pub fn len(&self) -> usize {
        self.boards.len()
    }

    /// Whether the pool holds no boards yet.
    pub fn is_empty(&self) -> bool {
        self.boards.is_empty()
    }

    /// Brings `workers` boards up to `baseline`.
    ///
    /// Normally an [`Board::adopt_store`] of a clone of the baseline's items.
    /// A board whose §11.6 normalize-suppression latch has drifted from the
    /// baseline's is thrown away and re-cloned instead: `adopt_store` restores
    /// items, trees and connectivity but not that latch, and a worker carrying a
    /// suppressed net would quietly answer every later question about it
    /// differently from the live board. It is a 2000-iteration oscillation
    /// pathology, so this is a guard, not a hot path.
    fn sync_to(&mut self, baseline: &Board, workers: usize) {
        for board in self.boards.iter_mut() {
            if latch_drifted(baseline, board) {
                *board = baseline.clone();
            } else {
                board.adopt_store(baseline.store().clone());
            }
        }
        while self.boards.len() < workers {
            self.boards.push(baseline.clone());
        }
    }

    /// Evaluates every item of `window` against `baseline`, in parallel.
    ///
    /// `baseline` is read-only for the whole call; the returned verdicts are
    /// indexed by `window`, not by completion order.
    pub fn evaluate(
        &mut self,
        baseline: &Board,
        cfg: &AttemptConfig<'_>,
        window: &[ItemId],
        threads: Threads,
    ) -> Vec<Verdict> {
        let mut verdicts = vec![Verdict::Unevaluated; window.len()];
        if window.is_empty() {
            return verdicts;
        }
        let workers = threads.capped_to(window.len()).get();
        if workers <= 1 {
            // Not a parallel run: evaluate in place with one scratch board, so
            // that `--threads 1` never pays for a pool it does not use.
            self.sync_to(baseline, 1);
            let board = &mut self.boards[0];
            let mut scratch = AttemptScratch::new();
            let cancelled = AtomicBool::new(false);
            for (index, verdict) in verdicts.iter_mut().enumerate() {
                *verdict = evaluate_one(board, cfg, window[index], &mut scratch, &cancelled);
            }
            return verdicts;
        }

        self.sync_to(baseline, workers);
        // Shared only to stop every worker at once when the job budget goes; it
        // can never change a verdict, only turn one into `Unevaluated`.
        let cancelled = AtomicBool::new(false);
        let boards = &mut self.boards[..workers];
        let results: Vec<Vec<(usize, Verdict)>> = std::thread::scope(|scope| {
            let handles: Vec<_> = boards
                .iter_mut()
                .enumerate()
                .map(|(worker, board)| {
                    let cancelled = &cancelled;
                    scope.spawn(move || {
                        let mut scratch = AttemptScratch::new();
                        let mut out = Vec::new();
                        for index in (0..window.len()).filter(|i| assign(*i, workers) == worker) {
                            out.push((
                                index,
                                evaluate_one(board, cfg, window[index], &mut scratch, cancelled),
                            ));
                        }
                        out
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| match handle.join() {
                    Ok(out) => out,
                    // A worker panic is a bug in the router, not a routing
                    // outcome. Re-raise it on the promoter's thread rather than
                    // silently returning a board that skipped part of a pass.
                    Err(payload) => std::panic::resume_unwind(payload),
                })
                .collect()
        });
        for chunk in results {
            for (index, verdict) in chunk {
                verdicts[index] = verdict;
            }
        }
        verdicts
    }
}

/// One item on one worker board: rip, re-route, measure, **always** roll back.
fn evaluate_one(
    board: &mut Board,
    cfg: &AttemptConfig<'_>,
    item: ItemId,
    scratch: &mut AttemptScratch,
    cancelled: &AtomicBool,
) -> Verdict {
    if cancelled.load(Ordering::Relaxed) {
        return Verdict::Unevaluated;
    }
    // The job's wall budget. Polled here, between items, for the same reason the
    // pass polls it between connections: it is the only point at which the
    // worker board is in a consistent state.
    if crate::batch::job_budget_spent() {
        cancelled.store(true, Ordering::Relaxed);
        return Verdict::Unevaluated;
    }
    let mut rerouter = MazeRerouter::default();
    let result = attempt_item(
        board,
        item,
        cfg,
        &mut rerouter,
        scratch,
        Disposition::Discard,
    );
    if result.improved() {
        Verdict::Improved
    } else {
        Verdict::Rejected
    }
}

/// Whether `clone` has latched a normalize-suppressed net the `baseline` has
/// not (or vice versa).
fn latch_drifted(baseline: &Board, clone: &Board) -> bool {
    clone
        .store()
        .nets()
        .map(|(net_no, _)| net_no)
        .chain(baseline.store().nets().map(|(net_no, _)| net_no))
        .any(|net_no| {
            clone.is_normalize_suppressed(net_no) != baseline.is_normalize_suppressed(net_no)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_assignment_is_interleaved_round_robin() {
        let workers = 3;
        let owners: Vec<usize> = (0..7).map(|i| assign(i, workers)).collect();
        assert_eq!(owners, vec![0, 1, 2, 0, 1, 2, 0]);
        // Every index has exactly one owner and every worker gets a share.
        for worker in 0..workers {
            assert!((0..7).any(|i| assign(i, workers) == worker));
        }
    }

    #[test]
    fn the_assignment_survives_a_zero_worker_budget() {
        assert_eq!(assign(5, 0), 0);
    }

    #[test]
    fn a_pass_starts_with_one_item_per_worker() {
        assert_eq!(initial_window_size(Threads::new(4)), 4);
        assert_eq!(initial_window_size(Threads::single()), 1);
        assert!(initial_window_size(Threads::new(0)) >= 1);
    }

    #[test]
    fn the_window_doubles_while_nothing_is_kept_and_collapses_when_something_is() {
        let threads = Threads::new(4);
        let mut window = initial_window_size(threads);
        let mut widths = vec![window];
        for _ in 0..6 {
            window = next_window_size(window, threads, false);
            widths.push(window);
        }
        assert_eq!(widths, vec![4, 8, 16, 32, 32, 32, 32], "doubles, then caps");
        assert_eq!(*widths.last().unwrap(), max_window_size(threads));

        // One kept item and the speculation window is back to the floor.
        assert_eq!(next_window_size(32, threads, true), 4);
        assert_eq!(next_window_size(4, threads, true), 4);
    }

    #[test]
    fn the_window_never_falls_below_the_worker_count() {
        for threads in [Threads::single(), Threads::new(3), Threads::new(8)] {
            assert!(initial_window_size(threads) >= threads.get());
            assert!(next_window_size(0, threads, false) >= threads.get());
            assert!(next_window_size(0, threads, true) >= threads.get());
            assert!(max_window_size(threads) >= initial_window_size(threads));
        }
    }
}
