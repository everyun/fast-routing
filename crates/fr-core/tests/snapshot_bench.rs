use fr_core::pipeline::RoutingJob;
use fr_core::snapshot::{snapshot, Stats};
use std::path::PathBuf;
use std::time::Instant;

fn find_cla45_stage2() -> Option<PathBuf> {
    let candidates = [
        PathBuf::from("/Users/chiqingzhen/fr-work/cutout-keepout/bench/fixtures-oos/cla45-compute-hdi6cs-stage2.dsn"),
        PathBuf::from("bench/fixtures-oos/cla45-compute-hdi6cs-stage2.dsn"),
        PathBuf::from("../cutout-keepout/bench/fixtures-oos/cla45-compute-hdi6cs-stage2.dsn"),
        PathBuf::from("bench/fixtures-oos/cla45-compute-hdi6cs.dsn"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

#[test]
fn bench_cla45_stage2_snapshot() {
    let Some(path) = find_cla45_stage2() else {
        eprintln!("cla45 stage2 not found, skipping");
        return;
    };
    println!("Loading fixture: {}", path.display());
    let report = RoutingJob::from_path(&path)
        .expect("cla45 stage2 reads")
        .run()
        .expect("cla45 stage2 imports");

    let total_items = report.census.total();
    println!("Total items in board: {total_items}");

    // Warm up
    let _ = snapshot(&report.design.board, Stats::default());

    // Measure snapshot() over 10 iterations
    let iterations = 10;
    let start = Instant::now();
    for _ in 0..iterations {
        let model = snapshot(&report.design.board, Stats::default());
        std::hint::black_box(model);
    }
    let elapsed = start.elapsed();
    let per_iter = elapsed / iterations;
    println!(
        "snapshot() on {} ({} items): total {:?}, per iteration: {:?}",
        path.file_name().unwrap().to_string_lossy(),
        total_items,
        elapsed,
        per_iter
    );

    // Requirement: snapshot() < 50 ms in release
    #[cfg(not(debug_assertions))]
    assert!(
        per_iter.as_millis() < 50,
        "snapshot() must be < 50 ms on cla45 stage2 (15k items), got {:?}",
        per_iter
    );

    // Also assert model has items
    let model = snapshot(&report.design.board, Stats::default());
    println!(
        "RenderModel: {} layers, {} traces, {} vias, {} pads, {} areas, {} ratsnest",
        model.layers.len(),
        model.traces.len(),
        model.vias.len(),
        model.pads.len(),
        model.areas.len(),
        model.ratsnest.len()
    );
}
