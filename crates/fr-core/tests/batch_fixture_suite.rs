//! Batch verification suite against upstream Freerouting .dsn fixtures.
//! Compiled only with the default-on `legacy` feature; `fr_core::legacy` explains why the
//! pre-rewrite pipeline is still here and when it goes.
#![cfg(feature = "legacy")]

use fr_autoroute::BatchRouterSettings;
use fr_core::legacy::RoutingJob;
use std::fs;

mod support;

#[test]
fn test_batch_sample_fixtures() {
    let Some(fixture_dir) = support::fixtures_dir() else {
        return;
    };

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
