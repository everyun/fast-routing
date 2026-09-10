use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use fr_core::{MazeOptions, RouterStage, RoutingJob, RouterEvent};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from("/Users/chiqingzhen/fr-ref/freerouting-upstream/fixtures").join(name)
}

#[test]
fn test_observer_zero_divergence_issue054() {
    let dsn_path = fixture("Issue054-tairakb.dsn");
    if !dsn_path.exists() {
        eprintln!("Fixture Issue054-tairakb.dsn not found, skipping");
        return;
    }

    // 1. Run without observer
    let job_no_obs = RoutingJob::from_path(&dsn_path)
        .unwrap()
        .with_stage(RouterStage::Maze(
            MazeOptions::with_passes(1).with_deterministic(true),
        ));
    let rep_no_obs = job_no_obs.run().unwrap();

    // 2. Run with observer
    let (tx, rx) = mpsc::channel();
    let job_with_obs = RoutingJob::from_path(&dsn_path)
        .unwrap()
        .with_stage(RouterStage::Maze(
            MazeOptions::with_passes(1).with_deterministic(true),
        ))
        .with_observer(Box::new(tx))
        .with_snapshot_interval_ms(100);
    let rep_with_obs = job_with_obs.run().unwrap();

    // Collect events
    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    assert!(!events.is_empty(), "Observer should have received events");

    // Verify byte-for-byte identical session text
    assert_eq!(
        rep_no_obs.ses, rep_with_obs.ses,
        "Observer caused session text divergence!"
    );
    assert_eq!(
        rep_no_obs.violations.len(),
        rep_with_obs.violations.len()
    );
    assert_eq!(
        rep_no_obs.stats.incomplete_count,
        rep_with_obs.stats.incomplete_count
    );
}

#[test]
fn test_observer_zero_divergence_relay_module() {
    let dsn_path = fixture("Issue110-RelayModule.dsn");
    if !dsn_path.exists() {
        return;
    }

    let job_no_obs = RoutingJob::from_path(&dsn_path)
        .unwrap()
        .with_stage(RouterStage::Maze(
            MazeOptions::with_passes(1).with_deterministic(true),
        ));
    let rep_no_obs = job_no_obs.run().unwrap();

    let (tx, rx) = mpsc::channel();
    let job_with_obs = RoutingJob::from_path(&dsn_path)
        .unwrap()
        .with_stage(RouterStage::Maze(
            MazeOptions::with_passes(1).with_deterministic(true),
        ))
        .with_observer(Box::new(tx))
        .with_snapshot_interval_ms(100);
    let rep_with_obs = job_with_obs.run().unwrap();

    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    assert!(!events.is_empty(), "Observer should have received events");

    assert_eq!(
        rep_no_obs.ses, rep_with_obs.ses,
        "Observer caused session text divergence on RelayModule!"
    );
}
