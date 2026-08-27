//! Exercises the counting global allocator for real.
//!
//! `#[global_allocator]` is per-binary, so this has to be an integration test:
//! the crate's own unit tests run inside a binary that does not install it.
//!
//! Runs only with `--features alloc-stats`; without it the file compiles to a
//! single trivially-passing test.

#[cfg(feature = "alloc-stats")]
#[global_allocator]
static ALLOC: fr_metrics::alloc_stats::CountingAllocator =
    fr_metrics::alloc_stats::CountingAllocator::system();

#[test]
#[cfg(feature = "alloc-stats")]
fn counts_bytes_and_tracks_the_peak() {
    let before = fr_metrics::alloc_stats::stats().expect("alloc-stats is on");

    // 4 MiB live, then released.
    let block: Vec<u64> = Vec::with_capacity(512 * 1024);
    let after_alloc = fr_metrics::alloc_stats::stats().expect("alloc-stats is on");
    let requested = 512 * 1024 * std::mem::size_of::<u64>() as u64;

    assert!(
        after_alloc.total_bytes - before.total_bytes >= requested,
        "total_bytes did not grow by the requested {requested} bytes"
    );
    assert!(after_alloc.allocations > before.allocations);
    assert!(
        after_alloc.peak_bytes >= after_alloc.live_bytes,
        "peak must dominate live"
    );
    let peak_at_alloc = after_alloc.peak_bytes;

    drop(block);
    let after_drop = fr_metrics::alloc_stats::stats().expect("alloc-stats is on");

    assert!(after_drop.deallocations > after_alloc.deallocations);
    assert!(
        after_drop.live_bytes < after_alloc.live_bytes,
        "live_bytes must fall when the block is freed"
    );
    assert_eq!(
        after_drop.peak_bytes, peak_at_alloc,
        "peak_bytes is a high-water mark and must not fall"
    );
    assert!(
        after_drop.total_bytes >= after_alloc.total_bytes,
        "total_bytes never decreases"
    );
}

#[test]
#[cfg(feature = "alloc-stats")]
fn appears_in_the_report() {
    let report = fr_metrics::report();
    let alloc = report.alloc.expect("alloc-stats is on");
    assert!(alloc.total_bytes > 0, "the test harness itself allocates");
    assert!(report
        .to_frmetric_lines()
        .iter()
        .any(|line| line.contains("\"kind\":\"alloc\"")));
}

#[test]
#[cfg(not(feature = "alloc-stats"))]
fn absent_without_the_feature() {
    assert!(!fr_metrics::alloc_stats_enabled());
    assert_eq!(fr_metrics::alloc_stats::stats(), None);
    assert!(fr_metrics::report().alloc.is_none());
}
