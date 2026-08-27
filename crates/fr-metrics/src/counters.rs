//! The router's inner-loop counters.
//!
//! perf-forensics §8.2 — *"Counters, not timers, in the inner loop … plain `u64`
//! fields on a per-thread struct; `#[cold]` reporting. These reproduce what
//! `PerformanceProfiler` should have measured."*
//!
//! Use the [`counter!`](crate::counter) macro at call sites:
//!
//! ```
//! use fr_metrics::counter;
//!
//! counter!(expansions);            // += 1
//! counter!(tree_nodes_visited, 7); // += 7
//! ```
//!
//! The macro forwards to the inline functions in [`bump`], which is also the
//! module to call directly from generic code. With the `metrics` feature off
//! every one of them is an empty `const fn`.

macro_rules! define_counters {
    ($( $variant:ident, $snake:ident, $doc:literal; )+) => {
        /// A router counter, in report order.
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
        #[repr(usize)]
        pub enum Counter {
            $(
                #[doc = $doc]
                $variant,
            )+
        }

        /// Number of distinct counters.
        pub const COUNTER_COUNT: usize = [$( stringify!($snake), )+].len();

        /// Counter names as emitted in the `FRMETRIC` JSON, indexed by
        /// `Counter as usize`.
        pub const COUNTER_NAMES: [&str; COUNTER_COUNT] = [$( stringify!($snake), )+];

        /// All counters, in report order.
        pub const ALL_COUNTERS: [Counter; COUNTER_COUNT] = [$( Counter::$variant, )+];

        /// One inline entry point per counter; what [`counter!`](crate::counter)
        /// expands to.
        pub mod bump {
            use super::Counter;

            $(
                #[doc = $doc]
                #[cfg(feature = "metrics")]
                #[inline(always)]
                pub fn $snake(delta: u64) {
                    super::add(Counter::$variant, delta);
                }

                #[doc = $doc]
                #[cfg(not(feature = "metrics"))]
                #[inline(always)]
                pub const fn $snake(delta: u64) {
                    super::add(Counter::$variant, delta);
                }
            )+
        }
    };
}

define_counters! {
    Expansions,           expansions,             "Maze elements settled: popped off the frontier and expanded (`MazeSearchAlgo.occupy_next_element`).";
    QueuePushes,          queue_pushes,           "Elements inserted into the maze frontier.";
    QueuePops,            queue_pops,             "Elements taken off the frontier, including the ones discarded as stale.";
    StalePops,            stale_pops,             "Popped elements whose door section was already occupied (lazy deletion).";
    TreeQueries,          tree_queries,           "Spatial search-tree overlap queries (attack surface #1).";
    TreeNodesVisited,     tree_nodes_visited,     "Search-tree nodes touched while traversing, the cost proxy for a query.";
    RoomCompletions,      room_completions,       "`complete_expansion_room` calls (attack surface #6).";
    DoorShapeComputations, door_shape_computations, "Door-shape intersections computed (attack surface #5 — must stay 0 once cached).";
    ExactIntersections,   exact_intersections,    "Geometry decisions that fell through to the exact `i128` path.";
    ShoveChecks,          shove_checks,           "Shove feasibility checks (`ShoveTraceAlgo` / `ForcedViaInserter`).";
    RipupChecks,          ripup_checks,           "Ripup feasibility checks in the maze loop.";
    PullTightIterations,  pull_tight_iterations,  "Pull-tight optimisation iterations over a changed area.";
}

impl Counter {
    /// The counter's name as it appears in the `FRMETRIC` JSON.
    #[inline]
    #[must_use]
    pub const fn name(self) -> &'static str {
        COUNTER_NAMES[self as usize]
    }
}

/// Adds `delta` to `counter` on the calling thread.
#[cfg(feature = "metrics")]
#[inline(always)]
pub fn add(counter: Counter, delta: u64) {
    crate::registry::with_block(|block| {
        crate::registry::ThreadBlock::bump(&block.counters[counter as usize], delta);
    });
}

/// Adds `delta` to `counter` on the calling thread.
///
/// With the `metrics` feature off this is an empty `const fn`: it can be
/// evaluated at compile time, which is a proof that it touches no thread-local,
/// no atomic and no clock (see `tests/zero_overhead.rs`).
#[cfg(not(feature = "metrics"))]
#[inline(always)]
pub const fn add(_counter: Counter, _delta: u64) {}

/// Reads one counter summed over every thread.
#[must_use]
pub fn value(counter: Counter) -> u64 {
    snapshot()[counter as usize]
}

/// Reads all counters summed over every thread, indexed by `Counter as usize`.
#[cfg(feature = "metrics")]
#[must_use]
pub fn snapshot() -> [u64; COUNTER_COUNT] {
    crate::registry::sum_counters()
}

/// Reads all counters summed over every thread, indexed by `Counter as usize`.
#[cfg(not(feature = "metrics"))]
#[must_use]
pub const fn snapshot() -> [u64; COUNTER_COUNT] {
    [0; COUNTER_COUNT]
}

/// Reads all counters of the *calling thread only*.
///
/// Useful in tests: unlike [`snapshot`] it cannot be perturbed by another thread
/// (`cargo test` runs test functions concurrently).
#[cfg(feature = "metrics")]
#[must_use]
pub fn thread_snapshot() -> [u64; COUNTER_COUNT] {
    use std::sync::atomic::Ordering::Relaxed;
    crate::registry::with_block(|block| std::array::from_fn(|i| block.counters[i].load(Relaxed)))
}

/// Reads all counters of the *calling thread only*.
#[cfg(not(feature = "metrics"))]
#[must_use]
pub const fn thread_snapshot() -> [u64; COUNTER_COUNT] {
    [0; COUNTER_COUNT]
}

/// Increments a counter by 1, or by an explicit delta.
///
/// ```
/// use fr_metrics::counter;
///
/// counter!(expansions);
/// counter!(expansions, 4);
/// ```
#[macro_export]
macro_rules! counter {
    ($name:ident) => {
        $crate::counters::bump::$name(1)
    };
    ($name:ident, $delta:expr) => {
        $crate::counters::bump::$name($delta)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_json_safe_identifiers() {
        for name in COUNTER_NAMES {
            assert!(!name.is_empty());
            assert!(
                name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "counter name {name:?} needs JSON escaping"
            );
        }
    }

    #[test]
    fn indices_match_report_order() {
        for (index, counter) in ALL_COUNTERS.into_iter().enumerate() {
            assert_eq!(counter as usize, index);
            assert_eq!(counter.name(), COUNTER_NAMES[index]);
        }
    }
}
