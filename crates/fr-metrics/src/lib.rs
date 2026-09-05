//! Measurement infrastructure for the Freerouting Rust port.
//!
//! This crate is the executable form of `docs/upstream-study/perf-forensics.md`
//! §8, *"A measurement plan for the Rust port"*. It is deliberately **not** a
//! port of upstream's `PerformanceProfiler` (§1.1), whose two design flaws — a
//! nesting stack that silently drops mismatched samples, and integer-millisecond
//! averages that report `0` for every section in the maze loop — make it unable
//! to answer the only question that matters: *why*.
//!
//! Four pieces, in the order §8 prescribes them:
//!
//! 1. [`alloc_stats`] — a counting global allocator behind the off-by-default
//!    `alloc-stats` feature. Target: **zero heap allocations per maze expansion
//!    and per spatial query** in steady state (§8.1).
//! 2. [`counters`] — `u64` counters, not timers, in the inner loop, kept in
//!    per-thread blocks and summed only when reporting (§8.2).
//! 3. [`timers`] — [`StageTimer`]s at stage boundaries only, mirroring the eight
//!    `profile*Nanos` accumulators of §1.3, plus upstream's `(pass, unrouted,
//!    duration_ms, ripup_cost)` history (§8.3, §8.4).
//! 4. [`settle_trace`] — the settle sequence our M2 differential harness diffs
//!    against the lab jar's `FRLAB settle` lines (§8.5).
//!
//! [`report`] turns all of that into `FRMETRIC …` JSON one-liners for
//! `bench/tools/`.
//!
//! # Cost
//!
//! The `metrics` feature is **on by default**. With it on, a counter bump is a
//! thread-local lookup plus a relaxed load/add/store on a cell nobody else
//! writes — no lock, no atomic RMW, no allocation, nothing that can appear in a
//! hot-loop allocation profile.
//!
//! With `--no-default-features` every entry point in this crate becomes an empty
//! `const fn` and [`StageTimer`] becomes a `Drop`-less zero-sized type, so call
//! sites vanish completely. `tests/zero_overhead.rs` proves this three ways:
//! the disabled calls are const-evaluable, the guard is zero-sized, and the
//! generated assembly of an instrumented loop contains no calls at all.
//!
//! # Example
//!
//! ```
//! use fr_metrics::{counter, stage_timer, Stage};
//!
//! fr_metrics::set_pass(1);
//! {
//!     let _timer = stage_timer(Stage::MazeSearch);
//!     for _ in 0..100 {
//!         counter!(expansions);
//!     }
//! }
//! fr_metrics::record_pass(1, 2, 1020, 100);
//!
//! let report = fr_metrics::report();
//! for line in report.to_frmetric_lines() {
//!     println!("{line}");
//! }
//! ```

pub mod alloc_stats;
pub mod counters;
#[cfg(feature = "metrics")]
mod registry;
pub mod report;
pub mod settle_trace;
pub mod timers;

pub use alloc_stats::AllocStats;
pub use counters::{Counter, ALL_COUNTERS, COUNTER_COUNT, COUNTER_NAMES};
pub use report::{MetricsReport, FRMETRIC_PREFIX};
pub use timers::{
    current_pass, record_pass, set_pass, stage_timer, PassRecord, Stage, StageRecord, StageTimer,
    ALL_STAGES, STAGE_COUNT, STAGE_NAMES,
};

/// Collects the process's metrics. See [`MetricsReport`].
#[must_use]
pub fn report() -> MetricsReport {
    MetricsReport::collect()
}

/// Clears every counter, timer and history row, plus the cumulative allocation
/// totals.
///
/// Intended for a bench harness that runs several fixtures in one process; the
/// router itself never needs it.
pub fn reset() {
    timers::reset();
    alloc_stats::reset();
}

/// True when this build actually measures anything (the `metrics` feature).
#[must_use]
pub const fn enabled() -> bool {
    cfg!(feature = "metrics")
}

/// True when the counting allocator is compiled in (the `alloc-stats` feature).
#[must_use]
pub const fn alloc_stats_enabled() -> bool {
    cfg!(feature = "alloc-stats")
}

/// Instrumented loop used by `tests/zero_overhead.rs` to inspect the generated
/// assembly. Not part of the public API.
#[doc(hidden)]
#[inline(never)]
#[no_mangle]
pub extern "C" fn fr_metrics_zero_overhead_probe(iterations: u64) -> u64 {
    let mut accumulator: u64 = 0;
    let mut index: u64 = 0;
    while index < iterations {
        counter!(expansions);
        counter!(tree_queries, 2);
        accumulator = accumulator.wrapping_add(index);
        index += 1;
    }
    accumulator
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialises the tests that touch the process-wide counters. Shared with
    /// `registry`'s own tests, which race with these otherwise.
    #[cfg(feature = "metrics")]
    use crate::registry::test_lock as exclusive;

    #[test]
    fn feature_predicates_agree_with_cfg() {
        assert_eq!(enabled(), cfg!(feature = "metrics"));
        assert_eq!(alloc_stats_enabled(), cfg!(feature = "alloc-stats"));
    }

    #[test]
    #[cfg(feature = "metrics")]
    fn counters_accumulate_on_the_calling_thread() {
        let before = counters::thread_snapshot();
        counter!(expansions);
        counter!(expansions, 9);
        counter!(shove_checks, 3);
        let after = counters::thread_snapshot();

        assert_eq!(
            after[Counter::Expansions as usize] - before[Counter::Expansions as usize],
            10
        );
        assert_eq!(
            after[Counter::ShoveChecks as usize] - before[Counter::ShoveChecks as usize],
            3
        );
        assert_eq!(
            after[Counter::TreeQueries as usize],
            before[Counter::TreeQueries as usize]
        );
    }

    #[test]
    #[cfg(feature = "metrics")]
    fn worker_thread_counts_survive_into_the_report() {
        let _guard = exclusive();
        reset();

        std::thread::spawn(|| {
            counter!(room_completions, 7);
        })
        .join()
        .expect("worker thread");

        counter!(room_completions, 5);
        assert_eq!(report().counter(Counter::RoomCompletions), 12);
    }

    #[test]
    #[cfg(feature = "metrics")]
    fn stage_time_is_charged_to_the_current_pass() {
        let _guard = exclusive();
        reset();

        set_pass(1);
        drop(stage_timer(Stage::Fanout));
        set_pass(2);
        drop(stage_timer(Stage::Route));
        drop(stage_timer(Stage::Route));
        record_pass(2, 0, 17, 200);

        let report = report();
        assert_eq!(report.stages.len(), 2, "{:?}", report.stages);
        assert_eq!(report.stages[0].pass, 1);
        assert_eq!(report.stages[0].stage, Stage::Fanout);
        assert_eq!(report.stages[0].calls, 1);
        assert_eq!(report.stages[1].pass, 2);
        assert_eq!(report.stages[1].stage, Stage::Route);
        assert_eq!(report.stages[1].calls, 2);
        assert_eq!(
            report.passes,
            vec![PassRecord {
                pass: 2,
                unrouted: 0,
                duration_ms: 17,
                ripup_cost: 200
            }]
        );
    }

    #[test]
    #[cfg(feature = "metrics")]
    fn report_is_idempotent() {
        let _guard = exclusive();
        reset();

        set_pass(3);
        drop(stage_timer(Stage::Optimize));
        let first = report();
        let second = report();
        assert_eq!(first, second);
    }

    #[test]
    #[cfg(feature = "metrics")]
    fn reset_clears_everything() {
        let _guard = exclusive();
        reset();

        counter!(expansions, 1234);
        set_pass(1);
        drop(stage_timer(Stage::Route));
        record_pass(1, 5, 10, 100);
        assert!(!report().is_empty());

        reset();
        let report = report();
        assert!(report.is_empty(), "{report:?}");
        assert_eq!(current_pass(), 0);
    }

    #[test]
    fn probe_computes_the_gauss_sum_regardless_of_instrumentation() {
        assert_eq!(fr_metrics_zero_overhead_probe(0), 0);
        assert_eq!(fr_metrics_zero_overhead_probe(5), 10);
    }
}
