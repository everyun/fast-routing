//! Integration tests for router observer events and cancellation.
//!
//! Validates event sequence determinism on Issue026-J2_reference and cancellation stop paths.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use fr_core::{MazeOptions, RouterStage, RoutingJob};
use fr_router::observer::{RouterEvent, RouterObserver};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("two levels below root")
        .to_path_buf()
}

fn fixture_bytes(name: &str) -> Vec<u8> {
    let candidate = PathBuf::from("/tmp/freerouting-upstream/fixtures").join(name);
    if let Ok(bytes) = std::fs::read(&candidate) {
        return bytes;
    }
    let candidate2 = repo_root().join("bench/fixtures").join(name);
    if let Ok(bytes) = std::fs::read(&candidate2) {
        return bytes;
    }
    let candidate3 = dirs_home()
        .join("fr-ref/freerouting-upstream/fixtures")
        .join(name);
    if let Ok(bytes) = std::fs::read(&candidate3) {
        return bytes;
    }
    panic!("cannot find fixture {name}");
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Simplified representation of deterministic event properties for assertions.
#[derive(Debug, Clone, PartialEq)]
enum EventSummary {
    JobStarted {
        design: String,
        layers: u16,
        connections: u32,
    },
    FanoutPass {
        pass: u32,
        routed: u32,
        escapes: u32,
    },
    PassStarted {
        pass: u32,
        incomplete: u32,
    },
    ConnectionDone {
        pass: u32,
        net: i32,
        ok: bool,
        ripped: u32,
    },
    PassFinished {
        pass: u32,
        incomplete: u32,
        violations: u32,
        score: f64,
    },
    Snapshot {
        incomplete: u32,
        violations: u32,
        pass: u32,
        items: u32,
    },
    Stopped {
        reason: String,
    },
    Finished {
        incomplete: u32,
        violations: u32,
    },
}

impl EventSummary {
    fn from_event(ev: &RouterEvent) -> Self {
        match ev {
            RouterEvent::JobStarted {
                design,
                layers,
                connections,
            } => EventSummary::JobStarted {
                design: design.clone(),
                layers: *layers,
                connections: *connections,
            },
            RouterEvent::FanoutPass {
                pass,
                routed,
                escapes,
                ..
            } => EventSummary::FanoutPass {
                pass: *pass,
                routed: *routed,
                escapes: *escapes,
            },
            RouterEvent::PassStarted { pass, incomplete } => EventSummary::PassStarted {
                pass: *pass,
                incomplete: *incomplete,
            },
            RouterEvent::ConnectionDone {
                pass,
                net,
                ok,
                ripped,
                ..
            } => EventSummary::ConnectionDone {
                pass: *pass,
                net: *net,
                ok: *ok,
                ripped: *ripped,
            },
            RouterEvent::PassFinished {
                pass,
                incomplete,
                violations,
                score,
                ..
            } => EventSummary::PassFinished {
                pass: *pass,
                incomplete: *incomplete,
                violations: *violations,
                score: *score,
            },
            RouterEvent::Snapshot(model) => EventSummary::Snapshot {
                incomplete: model.stats.incomplete,
                violations: model.stats.violations,
                pass: model.stats.pass,
                items: model.stats.items,
            },
            RouterEvent::Stopped { reason } => EventSummary::Stopped {
                reason: reason.clone(),
            },
            RouterEvent::Finished {
                incomplete,
                violations,
                ..
            } => EventSummary::Finished {
                incomplete: *incomplete,
                violations: *violations,
            },
        }
    }
}

#[derive(Clone, Default)]
struct RecordingObserver {
    events: Arc<Mutex<Vec<EventSummary>>>,
}

impl RouterObserver for RecordingObserver {
    fn on_event(&mut self, ev: RouterEvent) {
        self.events
            .lock()
            .unwrap()
            .push(EventSummary::from_event(&ev));
    }
}

#[test]
fn test_issue026_event_sequence_deterministic() {
    let dsn_bytes = fixture_bytes("Issue026-J2_reference.dsn");

    let run_once = || {
        let observer = RecordingObserver::default();
        let events_handle = observer.events.clone();
        let job = RoutingJob::new(dsn_bytes.clone(), "Issue026-J2_reference.dsn")
            .with_stage(RouterStage::Maze(
                MazeOptions::with_passes(1)
                    .with_fanout(true)
                    .with_deterministic(true)
                    .with_optimizer(false),
            ))
            .with_observer(Box::new(observer));

        let report = job.run().expect("routing job succeeds");
        let events = events_handle.lock().unwrap().clone();
        (report, events)
    };

    let (report1, events1) = run_once();
    let (report2, events2) = run_once();

    assert_eq!(
        report1.ses, report2.ses,
        "SES output must be byte-identical"
    );
    assert_eq!(
        events1.len(),
        events2.len(),
        "Total event counts must match"
    );
    assert_eq!(
        events1, events2,
        "Event sequence must be 100% deterministic between runs"
    );

    // Verify key structure of the event sequence
    assert!(events1.len() > 10, "Should have emitted multiple events");
    match &events1[0] {
        EventSummary::JobStarted {
            design,
            layers,
            connections,
        } => {
            assert_eq!(design, "Issue026-J2_reference.dsn");
            assert_eq!(*layers, 2);
            assert_eq!(*connections, 33);
        }
        other => panic!("First event should be JobStarted, got: {other:?}"),
    }

    match events1.last().unwrap() {
        EventSummary::Finished {
            incomplete,
            violations,
        } => {
            assert_eq!(*incomplete, 0);
            assert_eq!(*violations, 0);
        }
        other => panic!("Last event should be Finished, got: {other:?}"),
    }
}

#[test]
fn test_cancellation_stops_routing() {
    let dsn_bytes = fixture_bytes("Issue026-J2_reference.dsn");
    let cancel = Arc::new(AtomicBool::new(true)); // Already cancelled before start

    let observer = RecordingObserver::default();
    let events_handle = observer.events.clone();

    let job = RoutingJob::new(dsn_bytes, "Issue026-J2_reference.dsn")
        .with_stage(RouterStage::Maze(
            MazeOptions::with_passes(5)
                .with_fanout(true)
                .with_deterministic(true),
        ))
        .with_cancel(cancel)
        .with_observer(Box::new(observer));

    let report = job.run().expect("job completes even when cancelled early");
    let events = events_handle.lock().unwrap().clone();

    // Must have stopped with reason "cancelled"
    let stopped = events
        .iter()
        .any(|ev| matches!(ev, EventSummary::Stopped { reason } if reason == "cancelled"));
    assert!(
        stopped,
        "Must emit Stopped {{ reason: 'cancelled' }} event, got events: {events:?}"
    );
    assert_eq!(report.router.as_ref().unwrap().stop_reason, "cancelled");
}

#[test]
fn test_observer_none_vs_some_zero_divergence() {
    let dsn_bytes = fixture_bytes("Issue026-J2_reference.dsn");

    // Run with observer None
    let job_none = RoutingJob::new(dsn_bytes.clone(), "Issue026-J2_reference.dsn").with_stage(
        RouterStage::Maze(
            MazeOptions::with_passes(1)
                .with_fanout(true)
                .with_deterministic(true)
                .with_optimizer(true),
        ),
    );
    let report_none = job_none.run().expect("job succeeds with observer None");

    // Run with observer Some
    let observer = RecordingObserver::default();
    let job_some = RoutingJob::new(dsn_bytes, "Issue026-J2_reference.dsn")
        .with_stage(RouterStage::Maze(
            MazeOptions::with_passes(1)
                .with_fanout(true)
                .with_deterministic(true)
                .with_optimizer(true),
        ))
        .with_observer(Box::new(observer));
    let report_some = job_some.run().expect("job succeeds with observer Some");

    assert_eq!(
        report_none.ses, report_some.ses,
        "SES output must be byte-for-byte identical with observer None vs Some"
    );
    assert_eq!(report_none.stats, report_some.stats);
}
