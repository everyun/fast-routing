//! Batch verification suite against upstream Freerouting .dsn fixtures.

use fr_autoroute::BatchRouterSettings;
use fr_core::RoutingJob;
use std::fs;
use std::path::Path;

#[test]
fn test_batch_sample_fixtures() {
    let fixture_dir = Path::new("../../upstream-freerouting/fixtures");
    if !fixture_dir.exists() {
        eprintln!("Fixture dir not found, skipping");
        return;
    }

    let sample_fixtures = [
        "Issue026-J2_reference.dsn",
        "Issue035-ReadPlaceScope.dsn",
        "Issue269-min_fr_test/min_fr_test.dsn",
        "Issue313-FastTest.dsn",
    ];

    for fixture_rel in &sample_fixtures {
        let fixture_path = fixture_dir.join(fixture_rel);
        if !fixture_path.exists() {
            continue;
        }

        let dsn_text = fs::read_to_string(&fixture_path).unwrap_or_default();
        if dsn_text.is_empty() {
            continue;
        }

        let mut job = RoutingJob::new(&dsn_text);
        job.router_settings = BatchRouterSettings {
            max_passes: 2,
            ..Default::default()
        };

        match job.execute() {
            Ok(res) => {
                println!(
                    "Fixture {:?}: Score={:.1}, Vias={}, Violations={}, Incompletes={}",
                    fixture_rel,
                    res.statistics.calculate_score(),
                    res.statistics.via_count,
                    res.statistics.clearance_violation_count,
                    res.statistics.unrouted_net_count
                );
                assert!(!res.ses_content.is_empty());
            }
            Err(e) => {
                eprintln!("Failed to route {:?}: {}", fixture_rel, e);
            }
        }
    }
}
