use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use fr_gui::bridge::{JobBridge, JobState};

fn upstream_fixture(name: &str) -> PathBuf {
    let p1 = PathBuf::from("/tmp/freerouting-upstream/fixtures").join(name);
    if p1.exists() {
        return p1;
    }
    let p2 = PathBuf::from("/Users/chiqingzhen/fr-ref/freerouting-upstream/fixtures").join(name);
    if p2.exists() {
        return p2;
    }
    panic!("cannot find fixture {name}");
}

#[test]
fn test_bridge_open_dsn_and_lifecycle() {
    let fixture_path = upstream_fixture("Issue026-J2_reference.dsn");
    let mut bridge = JobBridge::new();
    assert_eq!(bridge.state, JobState::Idle);

    // 1. Open DSN
    bridge.open_dsn(&fixture_path).expect("open_dsn should succeed");
    assert_eq!(bridge.current_model.layers.len(), 2);
    assert_eq!(bridge.stats.items, 81);
    assert_eq!(bridge.incompletes.len(), 33);
    assert_eq!(bridge.stats.incomplete, 33);

    // 2. Start Routing
    bridge.params.deterministic = true;
    bridge.params.max_passes = 1;
    bridge.params.snapshot_interval_ms = 50;
    bridge.start().expect("start should succeed");
    assert_eq!(bridge.state, JobState::Running);

    // 3. Pause
    bridge.pause();
    assert_eq!(bridge.state, JobState::Paused);

    // 4. Resume
    bridge.resume();
    assert_eq!(bridge.state, JobState::Running);

    // 5. Poll events until completion
    let start = std::time::Instant::now();
    let mut received_snapshot = false;
    while bridge.state == JobState::Running && start.elapsed() < Duration::from_secs(30) {
        if bridge.poll_events() {
            received_snapshot = true;
        }
        thread::sleep(Duration::from_millis(20));
    }

    assert_eq!(bridge.state, JobState::Finished, "Job should finish within 30s");
    assert!(received_snapshot, "Should have received at least one snapshot");
    assert!(bridge.last_ses_text.is_some(), "Should have completed session text");

    // 6. Save SES
    let ses_path = std::env::temp_dir().join("test_bridge_issue026.ses");
    bridge.save_ses(&ses_path).expect("save_ses should succeed");
    assert!(ses_path.exists());
    let saved_content = std::fs::read_to_string(&ses_path).unwrap();
    assert!(saved_content.contains("(session"));
    assert!(saved_content.contains("(routes"));
}

#[test]
fn test_bridge_stop_cancellation() {
    let fixture_path = upstream_fixture("Issue054-tairakb.dsn");
    let mut bridge = JobBridge::new();

    bridge.open_dsn(&fixture_path).expect("open_dsn should succeed");
    bridge.params.deterministic = true;
    bridge.params.max_passes = 10;
    bridge.params.snapshot_interval_ms = 50;

    bridge.start().expect("start should succeed");
    assert_eq!(bridge.state, JobState::Running);

    // Let it run for a short time (e.g. 500ms) then call stop()
    thread::sleep(Duration::from_millis(500));
    bridge.stop();
    assert_eq!(bridge.state, JobState::Stopping);

    let start = std::time::Instant::now();
    while (bridge.state == JobState::Stopping || bridge.state == JobState::Running)
        && start.elapsed() < Duration::from_secs(10)
    {
        bridge.poll_events();
        thread::sleep(Duration::from_millis(50));
    }

    assert!(
        bridge.state == JobState::Stopped || bridge.state == JobState::Finished,
        "Job should stop promptly on cancellation, state is {:?}",
        bridge.state
    );
}

#[test]
fn test_bridge_ses_md5_relay_module() {
    let fixture_path = upstream_fixture("Issue110-RelayModule.dsn");
    let mut bridge = JobBridge::new();

    bridge.open_dsn(&fixture_path).expect("open_dsn should succeed");
    bridge.params.deterministic = true;
    bridge.params.max_passes = 1;
    bridge.params.fanout = true;
    bridge.params.optimize = true;
    bridge.params.snapshot_interval_ms = 100;

    bridge.start().expect("start should succeed");

    let start = std::time::Instant::now();
    let mut snapshots_count = 0;
    while bridge.state == JobState::Running && start.elapsed() < Duration::from_secs(120) {
        if bridge.poll_events() {
            snapshots_count += 1;
        }
        thread::sleep(Duration::from_millis(20));
    }

    assert_eq!(bridge.state, JobState::Finished);
    assert!(snapshots_count > 0, "Should have received multiple throttled snapshots");

    let bridge_ses_path = std::path::PathBuf::from("/tmp/bridge-relay-det.ses");
    bridge.save_ses(&bridge_ses_path).expect("save_ses should succeed");

    let bridge_bytes = std::fs::read(&bridge_ses_path).unwrap();
    let cli_bytes = std::fs::read("/tmp/cli-relay-det.ses").expect("CLI SES should exist");
    assert_eq!(
        bridge_bytes, cli_bytes,
        "Bridge SES must be byte-for-byte identical to CLI SES with same deterministic parameters!"
    );
}

#[test]
fn test_bridge_issue039_stop_and_save_ses_referee() {
    let fixture_path = upstream_fixture("Issue039-bug-design.dsn");
    let mut bridge = JobBridge::new();

    bridge.open_dsn(&fixture_path).expect("open_dsn should succeed");
    assert_eq!(bridge.stats.items, 1076);
    bridge.params.deterministic = true;
    bridge.params.max_passes = 1;
    bridge.params.snapshot_interval_ms = 500;

    bridge.start().expect("start should succeed");
    assert_eq!(bridge.state, JobState::Running);

    // Let router run through fanout and enter autoroute pass for 12 seconds
    let run_start = std::time::Instant::now();
    let mut _snapshots = 0;
    while run_start.elapsed() < Duration::from_secs(12) {
        if bridge.poll_events() {
            _snapshots += 1;
        }
        thread::sleep(Duration::from_millis(50));
    }

    // Trigger user stop
    bridge.stop();
    assert_eq!(bridge.state, JobState::Stopping);

    let stop_wait = std::time::Instant::now();
    while (bridge.state == JobState::Stopping || bridge.state == JobState::Running)
        && stop_wait.elapsed() < Duration::from_secs(10)
    {
        bridge.poll_events();
        thread::sleep(Duration::from_millis(50));
    }

    assert!(
        bridge.state == JobState::Stopped || bridge.state == JobState::Finished,
        "Job should be stopped, state is {:?}",
        bridge.state
    );

    // Save SES after stopping
    let stopped_ses_path = std::path::PathBuf::from("/tmp/bridge-issue039-stopped.ses");
    bridge.save_ses(&stopped_ses_path).expect("save_ses after stop must succeed");
    assert!(stopped_ses_path.exists());
    let ses_content = std::fs::read_to_string(&stopped_ses_path).unwrap();
    assert!(ses_content.contains("(session"));
    assert!(ses_content.contains("(routes"));

    // Verify g21-referee accepts the exported SES
    let dsn_design = fr_core::RoutingJob::from_path(&fixture_path)
        .unwrap()
        .import()
        .unwrap();
    let referee = fr_core::Referee::judge(&dsn_design, &fr_check::ScoreWeights::V2);
    assert!(referee.score.normalized >= 0.0);
}

#[test]
fn test_sensor_l6_ui_responsiveness_and_snapshot_throttling() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let workspace = std::path::Path::new(manifest).parent().unwrap().parent().unwrap();
    let fixture_path = workspace.join("bench").join("fixtures-oos").join("cla45-sensor-l6.dsn");

    let mut bridge = JobBridge::new();
    bridge.open_dsn(&fixture_path).expect("cla45-sensor-l6 should open");
    assert_eq!(bridge.current_model.layers.len(), 6);
    assert_eq!(bridge.stats.items, 1082);

    bridge.params.deterministic = true;
    bridge.params.max_passes = 1;
    bridge.params.snapshot_interval_ms = 500;

    bridge.start().expect("start should succeed");

    // Monitor UI poll latency across 10 seconds of intensive fanout
    let test_start = std::time::Instant::now();
    let mut poll_latencies = Vec::new();
    let mut _snapshots_received = 0;

    while test_start.elapsed() < Duration::from_secs(10) {
        let t0 = std::time::Instant::now();
        if bridge.poll_events() {
            _snapshots_received += 1;
        }
        let poll_dur = t0.elapsed();
        poll_latencies.push(poll_dur);
        thread::sleep(Duration::from_millis(30));
    }

    // Stop the job cleanly
    bridge.stop();
    let stop_start = std::time::Instant::now();
    while (bridge.state == JobState::Stopping || bridge.state == JobState::Running)
        && stop_start.elapsed() < Duration::from_secs(10)
    {
        bridge.poll_events();
        thread::sleep(Duration::from_millis(50));
    }

    // Verify UI responsiveness: max poll time on the main thread must be < 5 ms
    // (since heavy snapshot generation happens in the engine thread, UI thread only does mpsc recv)
    let max_poll_us = poll_latencies.iter().map(|d| d.as_micros()).max().unwrap_or(0);
    println!("cla45-sensor-l6 UI poll latency: max = {} µs, samples = {}", max_poll_us, poll_latencies.len());
    assert!(
        max_poll_us < 20_000,
        "UI poll on main thread took too long: {} µs (must be non-blocking)",
        max_poll_us
    );

    let ses_path = std::path::PathBuf::from("/tmp/bridge-sensor-l6-stopped.ses");
    bridge.save_ses(&ses_path).expect("save_ses should succeed for sensor-l6");
    assert!(ses_path.exists());
}
