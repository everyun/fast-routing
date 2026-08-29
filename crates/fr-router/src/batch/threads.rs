//! The thread budget — one type, shared by every parallel stage in this crate.
//!
//! `ARCHITECTURE.md` "Parallelism (restated honestly)" splits the work into
//! three tiers, and this module is the knob all of them read:
//!
//! * **T1** — [`crate::optimizer::parallel`], clone-eval-promote over the
//!   optimizer's item sweep;
//! * **T1b** — [`crate::batch::portfolio`], racing whole route passes on board
//!   clones;
//! * **T2** — the referee's violation scan, which lives in `fr-check` behind its
//!   own `parallel` feature and is configured by the same number.
//!
//! **T3** (concurrent connection routing) is not implemented and this type says
//! nothing about it; a `Threads` of 8 still routes one connection at a time.
//!
//! # Why the automatic value is capped
//!
//! Every parallel stage here buys its independence with a **board clone per
//! worker**, so the memory the stage holds is linear in the thread count and the
//! marginal worker gets less valuable as the clone traffic grows. On the bench
//! suite the optimizer's wall time stops falling somewhere between 4 and 8
//! workers (`bench/analysis/`), and the machine the suite runs on is also the
//! machine the user is working on. [`MAX_AUTOMATIC_THREADS`] is that observation
//! written down; an explicit `--threads N` is never capped, because a caller who
//! typed a number meant it.

use std::num::NonZeroUsize;

/// The ceiling [`Threads::automatic`] clamps `available_parallelism` to.
///
/// Not a hardware limit — a diminishing-returns limit. See the module docs.
pub const MAX_AUTOMATIC_THREADS: usize = 8;

/// How many worker threads a parallel stage may use. Always `>= 1`.
///
/// `Threads(1)` is not "parallel with one worker": every stage in this crate
/// tests [`Threads::is_parallel`] and takes the plain sequential path when it is
/// false, so a single-threaded run executes exactly the code it executed before
/// any of this existed — no clones, no scoped threads, no windowing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Threads(usize);

impl Default for Threads {
    /// [`Threads::single`].
    ///
    /// The library default is sequential, deliberately. The parallel optimizer
    /// is deterministic *at a fixed thread count* but is not bit-identical to
    /// the sequential one (it trusts a stale negative verdict inside a window —
    /// see [`crate::optimizer::parallel`]), so making it the default would move
    /// the quality gate as a side effect of a performance change. The choice is
    /// pushed up to whoever configures a run.
    fn default() -> Threads {
        Threads::single()
    }
}

impl Threads {
    /// One worker: the sequential path.
    pub const fn single() -> Threads {
        Threads(1)
    }

    /// `available_parallelism` clamped to `[1, MAX_AUTOMATIC_THREADS]`.
    ///
    /// `available_parallelism` already accounts for cgroup/affinity limits, so
    /// this is the right question to ask rather than a raw core count.
    pub fn automatic() -> Threads {
        let cpus = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
        Threads(cpus.clamp(1, MAX_AUTOMATIC_THREADS))
    }

    /// Exactly `count` workers, floored at one. Never capped.
    pub fn new(count: usize) -> Threads {
        Threads(count.max(1))
    }

    /// Resolves a caller's request: `None` or `Some(0)` mean
    /// [`Threads::automatic`], anything else is [`Threads::new`].
    ///
    /// `0` is "pick for me" rather than "do nothing", which is the convention
    /// `make -j0`-style flags and rayon's `num_threads(0)` both use.
    pub fn resolve(requested: Option<usize>) -> Threads {
        match requested {
            None | Some(0) => Threads::automatic(),
            Some(count) => Threads::new(count),
        }
    }

    /// The worker count.
    #[inline]
    pub const fn get(self) -> usize {
        self.0
    }

    /// Whether a stage should take its parallel path at all.
    #[inline]
    pub const fn is_parallel(self) -> bool {
        self.0 > 1
    }

    /// This budget, lowered to the amount of work there is. Spawning a worker
    /// per item when there are three items costs more than it saves.
    #[inline]
    pub fn capped_to(self, work_items: usize) -> Threads {
        Threads(self.0.min(work_items.max(1)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_library_default_is_sequential() {
        assert_eq!(Threads::default(), Threads::single());
        assert!(!Threads::default().is_parallel());
        assert_eq!(Threads::default().get(), 1);
    }

    #[test]
    fn zero_and_absent_both_mean_automatic() {
        assert_eq!(Threads::resolve(None), Threads::automatic());
        assert_eq!(Threads::resolve(Some(0)), Threads::automatic());
        assert_eq!(Threads::resolve(Some(3)), Threads::new(3));
    }

    #[test]
    fn the_automatic_value_is_capped_but_an_explicit_one_is_not() {
        assert!(Threads::automatic().get() <= MAX_AUTOMATIC_THREADS);
        assert!(Threads::automatic().get() >= 1);
        assert_eq!(Threads::new(64).get(), 64, "an explicit number is obeyed");
    }

    #[test]
    fn a_thread_count_never_falls_below_one() {
        assert_eq!(Threads::new(0).get(), 1);
        assert_eq!(Threads::new(4).capped_to(0).get(), 1);
        assert_eq!(Threads::new(4).capped_to(2).get(), 2);
        assert_eq!(Threads::new(4).capped_to(9).get(), 4);
    }
}
