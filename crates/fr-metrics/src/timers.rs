//! Stage timers and the upstream pass history.
//!
//! perf-forensics §8.3 — *"Timers only at stage boundaries — mirror the eight
//! `profile*Nanos` counters of §1.3 so the stage-level split is directly
//! comparable to `-Dfreerouting.benchmark.profile=true` runs"* — and §8.4,
//! *"keep upstream's pass history (`passNo`, `unroutedItems`, `durationMs`,
//! `ripupCost`)"*.
//!
//! Timing lives at stage boundaries only. Never put a [`StageTimer`] inside the
//! maze loop: `Instant::now()` is a real clock read, which is exactly the cost
//! perf-forensics §8.2 tells us to avoid there — use a [`counter!`](crate::counter)
//! instead.
//!
//! ```
//! use fr_metrics::{Stage, stage_timer};
//!
//! fr_metrics::set_pass(1);
//! {
//!     let _t = stage_timer(Stage::Fanout);
//!     // … fanout stage …
//! } // elapsed time is attributed to (pass 1, fanout) here
//! ```
//!
//! Accumulation is per thread and per stage; the *per pass* split is produced by
//! [`set_pass`], which folds the deltas accumulated since the previous call into
//! a [`StageRecord`]. That keeps the timer's stop path an array store instead of
//! a map lookup, and makes the MT optimizer's threads fold in for free.

macro_rules! define_stages {
    ($( $variant:ident, $snake:ident, $doc:literal; )+) => {
        /// A timed stage, in report order.
        ///
        /// The first three are the batch stages the bench harness already parses
        /// out of Java's log; the rest mirror the `profile*Nanos` accumulators of
        /// perf-forensics §1.3 one-for-one.
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
        #[repr(usize)]
        pub enum Stage {
            $(
                #[doc = $doc]
                $variant,
            )+
        }

        /// Number of distinct stages.
        pub const STAGE_COUNT: usize = [$( stringify!($snake), )+].len();

        /// Stage names as emitted in the `FRMETRIC` JSON, indexed by
        /// `Stage as usize`.
        pub const STAGE_NAMES: [&str; STAGE_COUNT] = [$( stringify!($snake), )+];

        /// All stages, in report order.
        pub const ALL_STAGES: [Stage; STAGE_COUNT] = [$( Stage::$variant, )+];
    };
}

define_stages! {
    Fanout,                  fanout,                   "Batch fanout stage (27 % of suite stage time; sole stage on 6 of the 14 Java timeouts).";
    Route,                   route,                    "Batch auto-route stage, all passes.";
    Optimize,                optimize,                 "Batch optimiser stage.";
    ItemSelection,           item_selection,           "`getAutorouteItems` — upstream `profileItemSelectionNanos`.";
    BoardStatistics,         board_statistics,         "Board statistics snapshot — upstream `profileBoardStatisticsNanos`.";
    IntermediateStatistics,  intermediate_statistics,  "DRC incompletes between items — upstream `profileIntermediateStatisticsNanos`.";
    AutorouteItem,           autoroute_item,           "One connection end to end — upstream `profileAutorouteItemNanos`.";
    MazeSearch,              maze_search,              "`autoroute_connection` maze search — upstream `profileMazeSearchNanos`.";
    OptChangedArea,          opt_changed_area,         "Pull-tight over the changed area — upstream `profileOptChangedAreaNanos`.";
    TailRemoval,             tail_removal,             "`removeTails` — upstream `profileTailRemovalNanos`.";
    IncompleteDrc,           incomplete_drc,           "`calculateIncompleteCount` — upstream `profileIncompleteDrcNanos`.";
}

impl Stage {
    /// The stage's name as it appears in the `FRMETRIC` JSON.
    #[inline]
    #[must_use]
    pub const fn name(self) -> &'static str {
        STAGE_NAMES[self as usize]
    }
}

/// Total time and call count charged to one `(pass, stage)` pair.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct StageRecord {
    /// Router pass this time was charged to; 0 for work outside any pass.
    pub pass: u32,
    /// The stage.
    pub stage: Stage,
    /// Wall-clock nanoseconds, summed over threads.
    pub nanos: u64,
    /// Number of timed regions, summed over threads.
    pub calls: u64,
}

/// One row of upstream's `PerformanceProfiler` pass history (perf-forensics §1.1).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct PassRecord {
    /// 1-based pass number.
    pub pass: u32,
    /// Items still unrouted at the end of the pass.
    pub unrouted: u32,
    /// Pass wall-clock duration in milliseconds.
    pub duration_ms: u64,
    /// Ripup cost in force for the pass (`start_ripup_costs * pass`).
    pub ripup_cost: u32,
}

// ---------------------------------------------------------------------------
// metrics ON
// ---------------------------------------------------------------------------

#[cfg(feature = "metrics")]
mod imp {
    use super::{PassRecord, Stage, StageRecord, ALL_STAGES, STAGE_COUNT};
    use crate::registry;
    use std::sync::Mutex;
    use std::time::Instant;

    /// Everything that is not per-thread: the current pass, the deltas already
    /// folded, and the two histories. Touched at stage/pass boundaries only.
    struct PassState {
        current_pass: u32,
        /// Cumulative `(nanos, calls)` per stage at the last fold.
        folded: [(u64, u64); STAGE_COUNT],
        stages: Vec<StageRecord>,
        passes: Vec<PassRecord>,
    }

    impl PassState {
        const fn new() -> Self {
            Self {
                current_pass: 0,
                folded: [(0, 0); STAGE_COUNT],
                stages: Vec::new(),
                passes: Vec::new(),
            }
        }

        /// Charges everything accumulated since the last fold to `current_pass`.
        fn fold(&mut self) {
            let totals = registry::sum_stages();
            for (index, stage) in ALL_STAGES.into_iter().enumerate() {
                let nanos = totals[index].0.wrapping_sub(self.folded[index].0);
                let calls = totals[index].1.wrapping_sub(self.folded[index].1);
                if nanos == 0 && calls == 0 {
                    continue;
                }
                let pass = self.current_pass;
                match self
                    .stages
                    .iter_mut()
                    .find(|record| record.pass == pass && record.stage == stage)
                {
                    Some(record) => {
                        record.nanos += nanos;
                        record.calls += calls;
                    }
                    None => self.stages.push(StageRecord {
                        pass,
                        stage,
                        nanos,
                        calls,
                    }),
                }
            }
            self.folded = totals;
        }
    }

    static STATE: Mutex<PassState> = Mutex::new(PassState::new());

    fn with_state<R>(f: impl FnOnce(&mut PassState) -> R) -> R {
        // Poisoning carries no meaning for a pile of counters.
        let mut guard = match STATE.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        f(&mut guard)
    }

    /// RAII guard: charges its lifetime to `stage` when dropped.
    #[must_use = "the stage is only timed while the guard is alive"]
    pub struct StageTimer {
        stage: Stage,
        start: Instant,
    }

    impl StageTimer {
        #[inline]
        pub(super) fn start(stage: Stage) -> Self {
            Self {
                stage,
                start: Instant::now(),
            }
        }

        /// Nanoseconds elapsed so far, without stopping the timer.
        #[must_use]
        pub fn elapsed_nanos(&self) -> u64 {
            u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(u64::MAX)
        }
    }

    impl Drop for StageTimer {
        #[inline]
        fn drop(&mut self) {
            let nanos = self.elapsed_nanos();
            let index = self.stage as usize;
            registry::with_block(|block| {
                registry::ThreadBlock::bump(&block.stage_nanos[index], nanos);
                registry::ThreadBlock::bump(&block.stage_calls[index], 1);
            });
        }
    }

    #[cold]
    pub fn set_pass(pass: u32) {
        with_state(|state| {
            state.fold();
            state.current_pass = pass;
        });
    }

    #[must_use]
    pub fn current_pass() -> u32 {
        with_state(|state| state.current_pass)
    }

    #[cold]
    pub fn record_pass(pass: u32, unrouted: u32, duration_ms: u64, ripup_cost: u32) {
        with_state(|state| {
            state.passes.push(PassRecord {
                pass,
                unrouted,
                duration_ms,
                ripup_cost,
            });
        });
    }

    /// Folds the pending deltas and returns both histories.
    ///
    /// Idempotent: a second call with no intervening work returns the same data.
    #[cold]
    pub fn histories() -> (Vec<StageRecord>, Vec<PassRecord>) {
        with_state(|state| {
            state.fold();
            (state.stages.clone(), state.passes.clone())
        })
    }

    #[cold]
    pub fn reset() {
        with_state(|state| {
            registry::clear_all();
            *state = PassState::new();
        });
    }
}

// ---------------------------------------------------------------------------
// metrics OFF — everything below is an empty `const fn` and a zero-sized guard.
// ---------------------------------------------------------------------------

#[cfg(not(feature = "metrics"))]
mod imp {
    use super::{PassRecord, Stage, StageRecord};

    /// Zero-sized, `Drop`-less guard: the whole timing region compiles away.
    #[must_use = "the stage is only timed while the guard is alive"]
    pub struct StageTimer;

    impl StageTimer {
        #[inline(always)]
        pub(super) const fn start(_stage: Stage) -> Self {
            Self
        }

        /// Always 0 without the `metrics` feature.
        #[must_use]
        pub const fn elapsed_nanos(&self) -> u64 {
            0
        }
    }

    #[inline(always)]
    pub const fn set_pass(_pass: u32) {}

    #[must_use]
    pub const fn current_pass() -> u32 {
        0
    }

    #[inline(always)]
    pub const fn record_pass(_pass: u32, _unrouted: u32, _duration_ms: u64, _ripup_cost: u32) {}

    pub fn histories() -> (Vec<StageRecord>, Vec<PassRecord>) {
        (Vec::new(), Vec::new())
    }

    pub const fn reset() {}
}

pub use imp::StageTimer;

/// Starts timing `stage`; the elapsed time is charged when the guard drops.
#[inline]
#[must_use = "the stage is only timed while the guard is alive"]
pub fn stage_timer(stage: Stage) -> StageTimer {
    StageTimer::start(stage)
}

/// Sets the pass that subsequent stage time is charged to.
///
/// Everything accumulated since the previous call is folded into the record of
/// the *previous* pass first, so callers may call this at pass boundaries only.
/// Pass 0 is the conventional bucket for work outside any router pass.
#[inline]
pub fn set_pass(pass: u32) {
    imp::set_pass(pass);
}

/// The pass that stage time is currently charged to.
#[must_use]
pub fn current_pass() -> u32 {
    imp::current_pass()
}

/// Appends a row to the upstream-shaped pass history (perf-forensics §8.4).
#[inline]
pub fn record_pass(pass: u32, unrouted: u32, duration_ms: u64, ripup_cost: u32) {
    imp::record_pass(pass, unrouted, duration_ms, ripup_cost);
}

/// The stage and pass histories, deltas folded, sorted for a stable report.
#[must_use]
pub fn histories() -> (Vec<StageRecord>, Vec<PassRecord>) {
    let (mut stages, mut passes) = imp::histories();
    // Deterministic report order regardless of the order stages ran in.
    stages.sort_unstable_by_key(|record| (record.pass, record.stage as usize));
    passes.sort_unstable_by_key(|record| record.pass);
    (stages, passes)
}

/// Clears every counter, stage timer and history row in the process.
pub fn reset() {
    imp::reset();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_json_safe_identifiers() {
        for name in STAGE_NAMES {
            assert!(!name.is_empty());
            assert!(
                name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "stage name {name:?} needs JSON escaping"
            );
        }
    }

    #[test]
    fn indices_match_report_order() {
        for (index, stage) in ALL_STAGES.into_iter().enumerate() {
            assert_eq!(stage as usize, index);
            assert_eq!(stage.name(), STAGE_NAMES[index]);
        }
    }

    #[test]
    fn upstream_profile_nanos_counters_are_all_covered() {
        // perf-forensics §1.3 — the eight `profile*Nanos` accumulators.
        for name in [
            "item_selection",
            "board_statistics",
            "intermediate_statistics",
            "autoroute_item",
            "maze_search",
            "opt_changed_area",
            "tail_removal",
            "incomplete_drc",
        ] {
            assert!(STAGE_NAMES.contains(&name), "missing stage {name}");
        }
    }
}
