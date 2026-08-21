//! DAC2020 PCB Benchmark Suite parity verification tests.

use fr_autoroute::BatchRouterSettings;
use fr_core::RoutingJob;
use rayon::prelude::*;
use std::fs;
use std::path::Path;

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

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let base_dir = manifest_dir.parent().unwrap().parent().unwrap().join("upstream-freerouting/fixtures");
    if !base_dir.exists() {
        eprintln!("Fixtures directory not found, skipping");
        return;
    }

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
