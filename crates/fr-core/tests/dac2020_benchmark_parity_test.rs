//! DAC2020 PCB Benchmark Suite parity verification tests.
//! Compiled only with the default-on `legacy` feature; `fr_core::legacy` explains why the
//! pre-rewrite pipeline is still here and when it goes.
#![cfg(feature = "legacy")]

use fr_autoroute::BatchRouterSettings;
use fr_core::legacy::RoutingJob;
use rayon::prelude::*;
use std::fs;

mod support;

#[test]
fn test_dac2020_benchmarks_all_pass() {
    let benchmarks = [
        "Issue508-DAC2020/DAC2020_bm01/DAC2020_bm01.unrouted.dsn",
        "Issue508-DAC2020/DAC2020_bm02/DAC2020_bm02.unrouted.dsn",
        "Issue508-DAC2020/DAC2020_bm04/DAC2020_bm04.unrouted.dsn",
        "Issue508-DAC2020/DAC2020_bm05/DAC2020_bm05.unrouted.dsn",
        "Issue508-DAC2020/DAC2020_bm06/DAC2020_bm06.unrouted.dsn",
        "Issue508-DAC2020/DAC2020_bm07/DAC2020_bm07.unrouted.dsn",
        "Issue508-DAC2020/DAC2020_bm08/DAC2020_bm08.unrouted.dsn",
        "Issue508-DAC2020/DAC2020_bm09/DAC2020_bm09.unrouted.dsn",
        "Issue508-DAC2020/DAC2020_bm10/DAC2020_bm10.unrouted.dsn",
        "Issue508-DAC2020/DAC2020_bm11/DAC2020_bm11.unrouted.dsn",
    ];

    // The DAC2020 boards are not part of `bench/fetch_fixtures.sh`'s corpus; when they are
    // absent every row below skips and this test is a no-op, which is why it must never be
    // quoted as evidence of anything.
    let Some(base_dir) = support::fixtures_dir() else {
        return;
    };

    println!("\n=== DAC2020 PCB Benchmark Parity Results ===");
    benchmarks.par_iter().for_each(|bm_rel| {
        let bm_path = base_dir.join(bm_rel);
        if !bm_path.exists() {
            return;
        }

        let dsn_text = fs::read_to_string(&bm_path).expect("failed to read benchmark file");
        let mut job = RoutingJob::new(&dsn_text);
        job.router_settings = BatchRouterSettings {
            max_passes: 2,
            ..Default::default()
        };

        let result = job.execute().expect("routing failed");
        assert!(result.statistics.total_net_count > 0);
        assert!(result.ses_content.contains("(session "));
    });
}
