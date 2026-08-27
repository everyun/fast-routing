//! Per-thread counter blocks and the process-wide registry that sums them.
//!
//! Only compiled when the `metrics` feature is on — with the feature off nothing
//! in this module is reachable and the crate has no global state at all.
//!
//! Design (perf-forensics §8.2: "plain `u64` fields on a per-thread struct;
//! `#[cold]` reporting"):
//!
//! - Every thread owns one [`ThreadBlock`]. The hot path is a *relaxed* load /
//!   add / store on a cell that only the owning thread writes, so it lowers to
//!   the same three instructions a plain `u64` field would — no lock, no RMW,
//!   no contention. The cells are `AtomicU64` purely so that [`report`] can read
//!   them from another thread without UB.
//! - Blocks are `Arc`-registered in [`REGISTRY`] while their thread lives. When
//!   the thread dies its counts are folded into [`retired_block`] and the block
//!   is dropped, so a worker's counts survive into the final report without the
//!   registry growing without bound in a long-lived process.
//! - The registry mutex is taken exactly twice per thread (birth, death) plus
//!   once per report; never on an increment.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use crate::counters::COUNTER_COUNT;
use crate::timers::STAGE_COUNT;

/// One thread's raw accumulators.
pub(crate) struct ThreadBlock {
    pub(crate) counters: [AtomicU64; COUNTER_COUNT],
    pub(crate) stage_nanos: [AtomicU64; STAGE_COUNT],
    pub(crate) stage_calls: [AtomicU64; STAGE_COUNT],
}

impl ThreadBlock {
    fn new() -> Self {
        Self {
            counters: std::array::from_fn(|_| AtomicU64::new(0)),
            stage_nanos: std::array::from_fn(|_| AtomicU64::new(0)),
            stage_calls: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    fn clear(&self) {
        for cell in &self.counters {
            cell.store(0, Relaxed);
        }
        for cell in &self.stage_nanos {
            cell.store(0, Relaxed);
        }
        for cell in &self.stage_calls {
            cell.store(0, Relaxed);
        }
    }

    /// Single-writer add. Not an atomic RMW: only the owning thread writes this
    /// cell, so a relaxed load/add/store is both sound and cheap.
    #[inline(always)]
    pub(crate) fn bump(cell: &AtomicU64, delta: u64) {
        cell.store(cell.load(Relaxed).wrapping_add(delta), Relaxed);
    }
}

/// The blocks of the threads that are currently alive. A `Vec`, never a hash map
/// — ARCHITECTURE: no default-hasher iteration where order matters.
static REGISTRY: Mutex<Vec<Arc<ThreadBlock>>> = Mutex::new(Vec::new());

/// Where a dying thread's counts go, so they still appear in the report.
fn retired_block() -> &'static ThreadBlock {
    static RETIRED: OnceLock<ThreadBlock> = OnceLock::new();
    RETIRED.get_or_init(ThreadBlock::new)
}

fn lock_registry() -> MutexGuard<'static, Vec<Arc<ThreadBlock>>> {
    // Poisoning carries no meaning here: the data is a pile of independent
    // counters, so a panic mid-update cannot leave it inconsistent.
    match REGISTRY.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Owns one thread's block; unregisters and folds it away on thread exit.
struct ThreadHandle(Arc<ThreadBlock>);

impl ThreadHandle {
    fn new() -> Self {
        let block = Arc::new(ThreadBlock::new());
        lock_registry().push(Arc::clone(&block));
        Self(block)
    }
}

impl Drop for ThreadHandle {
    fn drop(&mut self) {
        // Unregistering and folding happen under one lock, so a concurrent
        // reader sees this block's counts exactly once: either still live, or
        // already retired — never twice and never not at all. That is what
        // keeps `timers`' cumulative-minus-last-snapshot deltas monotonic.
        let mut registry = lock_registry();
        if let Some(position) = registry
            .iter()
            .position(|other| Arc::ptr_eq(other, &self.0))
        {
            registry.swap_remove(position);
        }
        let retired = retired_block();
        for (index, cell) in self.0.counters.iter().enumerate() {
            retired.counters[index].fetch_add(cell.load(Relaxed), Relaxed);
        }
        for (index, cell) in self.0.stage_nanos.iter().enumerate() {
            retired.stage_nanos[index].fetch_add(cell.load(Relaxed), Relaxed);
        }
        for (index, cell) in self.0.stage_calls.iter().enumerate() {
            retired.stage_calls[index].fetch_add(cell.load(Relaxed), Relaxed);
        }
    }
}

thread_local! {
    static LOCAL: ThreadHandle = ThreadHandle::new();
}

/// Runs `f` against the calling thread's block.
///
/// Yields `R::default()` — dropping the sample — if the thread-local has already
/// been destroyed, which can only happen in a `Drop` impl running after TLS
/// teardown. Losing a count there is preferable to panicking in a destructor.
#[inline(always)]
pub(crate) fn with_block<R: Default>(f: impl FnOnce(&ThreadBlock) -> R) -> R {
    LOCAL.try_with(|handle| f(&handle.0)).unwrap_or_default()
}

/// Sums `counters` across every registered block. `#[cold]`: reporting is rare.
#[cold]
pub(crate) fn sum_counters() -> [u64; COUNTER_COUNT] {
    let mut total = [0u64; COUNTER_COUNT];
    for_each_block(|block| {
        for (slot, cell) in total.iter_mut().zip(block.counters.iter()) {
            *slot = slot.wrapping_add(cell.load(Relaxed));
        }
    });
    total
}

/// Sums the stage accumulators across every registered block as
/// `[(nanos, calls); STAGE_COUNT]`.
#[cold]
pub(crate) fn sum_stages() -> [(u64, u64); STAGE_COUNT] {
    let mut total = [(0u64, 0u64); STAGE_COUNT];
    for_each_block(|block| {
        for (index, slot) in total.iter_mut().enumerate() {
            slot.0 = slot.0.wrapping_add(block.stage_nanos[index].load(Relaxed));
            slot.1 = slot.1.wrapping_add(block.stage_calls[index].load(Relaxed));
        }
    });
    total
}

/// Zeroes every registered block.
#[cold]
pub(crate) fn clear_all() {
    for_each_block(ThreadBlock::clear);
}

/// Visits every live block plus the retired accumulator, under the registry
/// lock so that a thread cannot retire mid-scan.
fn for_each_block(mut f: impl FnMut(&ThreadBlock)) {
    let registry = lock_registry();
    for block in registry.iter() {
        f(block);
    }
    f(retired_block());
}

/// Number of blocks belonging to live threads. Test hook for the leak check.
#[cfg(test)]
pub(crate) fn live_block_count() -> usize {
    lock_registry().len()
}

/// `cargo test` runs test functions on concurrent threads and every counter in
/// this crate is process-wide, so any test that resets or sums has to serialise
/// against the others.
#[cfg(test)]
pub(crate) fn test_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::counters::Counter;

    #[test]
    fn dead_threads_are_unregistered_but_their_counts_survive() {
        let _guard = test_lock();
        let before_counts = sum_counters()[Counter::RoomCompletions as usize];
        let before_blocks = live_block_count();

        for _ in 0..16 {
            std::thread::spawn(|| {
                with_block(|block| {
                    ThreadBlock::bump(&block.counters[Counter::RoomCompletions as usize], 3);
                });
            })
            .join()
            .expect("worker thread");
        }

        assert_eq!(
            sum_counters()[Counter::RoomCompletions as usize] - before_counts,
            48,
            "counts of dead threads must be folded into the retired block"
        );
        assert!(
            live_block_count() <= before_blocks,
            "the registry grew: {before_blocks} -> {}",
            live_block_count()
        );
    }
}
