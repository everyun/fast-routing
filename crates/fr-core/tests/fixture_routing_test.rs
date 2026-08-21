//! End-to-end routing integration tests on real DSN PCB fixtures from Freerouting.

use fr_autoroute::BatchRouterSettings;
use fr_core::RoutingJob;
use std::fs;
use std::path::Path;

#[test]
fn test_route_tutorial_board_example() {
    let fixture_path = Path::new("../../upstream-freerouting/examples/tutorial_board/tutorial_board.dsn");
    if !fixture_path.exists() {
        eprintln!("Skipping fixture test: file not found at {:?}", fixture_path);
        return;
    }

    let dsn_text = fs::read_to_string(fixture_path).expect("failed to read tutorial_board.dsn");
    let mut job = RoutingJob::new(&dsn_text);
    job.router_settings = BatchRouterSettings {
        max_passes: 5,
        ..Default::default()
    };

    let result = job.execute().expect("routing job failed");
    assert!(!result.ses_content.is_empty());
    assert!(result.ses_content.contains("(session"));
    println!(
        "Tutorial board routed: score={:.2}, vias={}, violations={}",
        result.statistics.calculate_score(),
        result.statistics.via_count,
        result.statistics.clearance_violation_count
    );
}

#[test]
fn test_route_fast_test_fixture() {
    let fixture_path = Path::new("../../upstream-freerouting/fixtures/Issue313-FastTest.dsn");
    if !fixture_path.exists() {
        eprintln!("Skipping fixture test: file not found at {:?}", fixture_path);
        return;
    }

    let dsn_text = fs::read_to_string(fixture_path).expect("failed to read Issue313-FastTest.dsn");
    let mut job = RoutingJob::new(&dsn_text);
    job.router_settings = BatchRouterSettings {
        max_passes: 3,
        ..Default::default()
    };

    let result = job.execute().expect("routing job failed");
    assert!(!result.ses_content.is_empty());
    assert!(result.ses_content.contains("(session"));
}
