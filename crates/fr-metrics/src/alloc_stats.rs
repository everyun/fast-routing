//! Counting global allocator (feature `alloc-stats`, default **off**).
//!
//! perf-forensics §8.1 — *"Allocation counting first. Wrap the global allocator
//! in a counting shim behind a `--features alloc-stats` build. Target: zero heap
//! allocations per maze expansion and zero per spatial query in steady state. If
//! a benchmark shows non-zero, that is a bug, not a tuning opportunity."*
//!
//! The shim is off by default because it costs two atomic RMWs per allocation.
//! Enable it on the measurement build only:
//!
//! ```sh
//! cargo build --release -p fr-cli --features fr-metrics/alloc-stats
//! ```
//!
//! and install it in the binary crate:
//!
//! ```ignore
//! #[global_allocator]
//! static ALLOC: fr_metrics::alloc_stats::CountingAllocator =
//!     fr_metrics::alloc_stats::CountingAllocator::system();
//! ```
//!
//! `total_bytes` is the analogue of the JVM's `getThreadAllocatedBytes` — the
//! number the 3 TB figure comes from (perf-forensics §1.2) — except that it is
//! process-wide and exact. `peak_bytes` is the analogue of "peak heap usage",
//! but of live *requested* bytes, so it is smaller than RSS and ignores
//! allocator overhead.

/// Allocation totals for the process.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct AllocStats {
    /// Bytes ever requested (never decreases).
    pub total_bytes: u64,
    /// High-water mark of `live_bytes`.
    pub peak_bytes: u64,
    /// Bytes currently requested and not yet freed.
    pub live_bytes: u64,
    /// Number of allocation calls (a `realloc` counts as one).
    pub allocations: u64,
    /// Number of deallocation calls (a `realloc` counts as one).
    pub deallocations: u64,
}

#[cfg(feature = "alloc-stats")]
mod imp {
    use super::AllocStats;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    static TOTAL_BYTES: AtomicU64 = AtomicU64::new(0);
    static PEAK_BYTES: AtomicU64 = AtomicU64::new(0);
    static LIVE_BYTES: AtomicU64 = AtomicU64::new(0);
    static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
    static DEALLOCATIONS: AtomicU64 = AtomicU64::new(0);

    /// Records `size` bytes handed out and updates the peak.
    ///
    /// Only plain statics are touched — the shim must never allocate, so it can
    /// use neither thread-locals with lazy init nor any `Vec`.
    #[inline]
    fn on_alloc(size: u64) {
        TOTAL_BYTES.fetch_add(size, Relaxed);
        ALLOCATIONS.fetch_add(1, Relaxed);
        let live = LIVE_BYTES.fetch_add(size, Relaxed) + size;
        PEAK_BYTES.fetch_max(live, Relaxed);
    }

    #[inline]
    fn on_dealloc(size: u64) {
        DEALLOCATIONS.fetch_add(1, Relaxed);
        LIVE_BYTES.fetch_sub(size, Relaxed);
    }

    /// A [`GlobalAlloc`] wrapper that counts bytes and calls.
    pub struct CountingAllocator<A: GlobalAlloc = System> {
        inner: A,
    }

    impl CountingAllocator<System> {
        /// Wraps the platform allocator.
        #[must_use]
        pub const fn system() -> Self {
            Self { inner: System }
        }
    }

    impl<A: GlobalAlloc> CountingAllocator<A> {
        /// Wraps an arbitrary allocator.
        pub const fn wrap(inner: A) -> Self {
            Self { inner }
        }
    }

    // SAFETY: every method forwards to `inner`, which upholds the `GlobalAlloc`
    // contract; the added bookkeeping only touches `static` atomics and never
    // allocates, so it cannot re-enter.
    unsafe impl<A: GlobalAlloc> GlobalAlloc for CountingAllocator<A> {
        #[inline]
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let ptr = self.inner.alloc(layout);
            if !ptr.is_null() {
                on_alloc(layout.size() as u64);
            }
            ptr
        }

        #[inline]
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let ptr = self.inner.alloc_zeroed(layout);
            if !ptr.is_null() {
                on_alloc(layout.size() as u64);
            }
            ptr
        }

        #[inline]
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            on_dealloc(layout.size() as u64);
            self.inner.dealloc(ptr, layout);
        }

        #[inline]
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            let new_ptr = self.inner.realloc(ptr, layout, new_size);
            if !new_ptr.is_null() {
                // Counted as one free of the old block plus one fresh
                // allocation, so `total_bytes` matches the "bytes handed out"
                // semantics of the JVM counter we compare against.
                on_dealloc(layout.size() as u64);
                on_alloc(new_size as u64);
            }
            new_ptr
        }
    }

    #[must_use]
    pub fn stats() -> Option<AllocStats> {
        Some(AllocStats {
            total_bytes: TOTAL_BYTES.load(Relaxed),
            peak_bytes: PEAK_BYTES.load(Relaxed),
            live_bytes: LIVE_BYTES.load(Relaxed),
            allocations: ALLOCATIONS.load(Relaxed),
            deallocations: DEALLOCATIONS.load(Relaxed),
        })
    }

    /// Resets the cumulative totals; `live_bytes` is preserved (the memory is
    /// still out there) and becomes the new peak baseline.
    pub fn reset() {
        TOTAL_BYTES.store(0, Relaxed);
        ALLOCATIONS.store(0, Relaxed);
        DEALLOCATIONS.store(0, Relaxed);
        PEAK_BYTES.store(LIVE_BYTES.load(Relaxed), Relaxed);
    }
}

#[cfg(not(feature = "alloc-stats"))]
mod imp {
    use super::AllocStats;

    /// Allocation statistics are not compiled in.
    #[must_use]
    pub const fn stats() -> Option<AllocStats> {
        None
    }

    pub const fn reset() {}
}

#[cfg(feature = "alloc-stats")]
pub use imp::CountingAllocator;

/// The process's allocation statistics, or `None` without the `alloc-stats`
/// feature.
#[must_use]
pub fn stats() -> Option<AllocStats> {
    imp::stats()
}

/// Resets the cumulative allocation totals. No-op without `alloc-stats`.
pub fn reset() {
    imp::reset();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(feature = "alloc-stats"))]
    fn absent_without_the_feature() {
        assert_eq!(stats(), None);
    }

    #[test]
    #[cfg(feature = "alloc-stats")]
    fn present_with_the_feature() {
        // The allocator is only installed by the binary crate, so the counts may
        // legitimately be zero here; the contract under test is that the
        // accessor exists and reports a consistent triple.
        let stats = stats().expect("alloc-stats is on");
        assert!(stats.peak_bytes >= stats.live_bytes);
        assert!(stats.total_bytes >= stats.live_bytes);
    }
}
