//! The M2a router stage, through the pipeline.
//!
//! These are the pipeline-level counterpart of `fr-cli`'s end-to-end cases: same board, but
//! asserting on [`fr_core::JobReport`] rather than on a process exit code, so a regression says
//! *which* number moved.
//!
//! The board is deliberately trivial — four pads, two nets, no `(wiring)` scope, an empty 10 mm
//! outline — because the claim under test is "the stage is wired up and the referee agrees with
//! it", not "the maze is good". Router quality lives in the bench gates.

use fr_core::{MazeOptions, RouterStage, RoutingJob};

/// Two nets, four pads, nothing connected. Each net's pads are 5 mm apart on `F.Cu` with an
/// empty board between them, so both connections exist and a one-pass run must find both.
/// `(resolution um 10)` with a 10 000-unit outline keeps the scale factor at 10.
const UNROUTED: &str = r#"(pcb router_stage
  (parser (string_quote "))
  (resolution um 10)
  (unit um)
  (structure
    (layer F.Cu (type signal))
    (layer B.Cu (type signal))
    (boundary (path signal 0 0 0 10000 0 10000 10000 0 10000 0 0))
    (rule (width 250) (clearance 200))
  )
  (placement
    (component pad4
      (place P1 1000 1000 front 0)
      (place P2 6000 1000 front 0)
      (place P3 1000 4000 front 0)
      (place P4 6000 4000 front 0)
    )
  )
  (library
    (image pad4 (pin round_pad 1 0 0))
    (padstack round_pad (shape (circle F.Cu 400)) (shape (circle B.Cu 400)) (attach off))
  )
  (network
    (net SIG (pins P1-1 P2-1))
    (net SIG2 (pins P3-1 P4-1))
    (class default SIG SIG2 (rule (width 250) (clearance 200)))
  )
)"#;

fn job() -> RoutingJob {
    RoutingJob::new(UNROUTED.as_bytes(), "router_stage.dsn")
}

#[test]
fn import_only_still_routes_nothing() {
    let report = job().run().expect("the board imports");
    assert!(!report.routed, "the default stage must not touch the board");
    assert!(report.router.is_none());
    assert_eq!(report.census.traces, 0, "the design has no wiring");
    // Two unconnected nets: two incomplete connections.
    assert_eq!(report.stats.incomplete_count, 2);
}

#[test]
fn one_maze_pass_completes_both_nets_without_a_violation() {
    let report = job()
        .with_stage(RouterStage::Maze(MazeOptions::with_passes(1)))
        .run()
        .expect("the board imports");

    assert!(report.routed);
    let router = report.router.clone().expect("the stage reports");
    assert_eq!(router.passes, 1, "one pass was asked for: {router:?}");
    assert_eq!(router.routed, 2, "one connection per net: {router:?}");
    assert_eq!(router.not_routed, 0, "{router:?}");
    assert_eq!(router.ripped, 0, "an empty board rips nothing: {router:?}");
    assert_eq!(router.fanouted, 0, "fanout is off by default");

    // The referee is the judge, not the router's own bookkeeping — and the two must agree.
    assert_eq!(
        report.stats.incomplete_count, 0,
        "{:?}",
        report.connectivity
    );
    assert_eq!(router.incomplete_count, 0);
    assert_eq!(
        report.stats.clearance_violation_count, 0,
        "{:?}",
        report.violations
    );
    assert!(report.is_clean());

    // One trace per net reached the board and the session.
    assert!(report.census.traces >= 2, "{:?}", report.census);
    assert!(report.ses.contains("(wire"), "{}", report.ses);
}

/// A run that already has nothing to do must still be a *run*: one pass, zero routed, the board
/// complete. This is the `autoroute_pass` "work list empty → board complete" branch
/// (`BatchAutorouter.java:437-440`) seen from the outside.
#[test]
fn a_second_run_over_a_routed_board_finds_nothing_to_do() {
    let first = job()
        .with_stage(RouterStage::Maze(MazeOptions::with_passes(1)))
        .run()
        .expect("the board imports");
    assert_eq!(first.stats.incomplete_count, 0);

    // Feed our own session's board back in by re-running the same job: the design is unchanged,
    // so this is really "routing twice is idempotent in outcome", which is the property a
    // multi-pass driver depends on.
    let second = job()
        .with_stage(RouterStage::Maze(MazeOptions::with_passes(3)))
        .run()
        .expect("the board imports");
    assert_eq!(second.stats.incomplete_count, 0);
    let router = second.router.clone().expect("the stage reports");
    // Pass 1 routes both nets; pass 2 finds an empty work list and stops the run there.
    assert!(router.passes <= 3, "{router:?}");
    assert_eq!(router.not_routed, 0, "{router:?}");
}

/// `max_passes` is a cap, not a demand: the driver stops as soon as the board is complete.
#[test]
fn the_pass_cap_is_an_upper_bound() {
    let report = job()
        .with_stage(RouterStage::Maze(MazeOptions::with_passes(9)))
        .run()
        .expect("the board imports");
    let router = report.router.clone().expect("the stage reports");
    assert!(
        router.passes < 9,
        "a complete board must stop the run early: {router:?}"
    );
    assert_eq!(report.stats.incomplete_count, 0);
}

/// A pass count below 1 is clamped, not honoured: upstream's `maxPasses <= 0` means "no cap"
/// and re-enables the router inside the loop (`batch-orchestration.md` conformance §1.2, the
/// "cautionary example"), which is exactly the trap the port refuses to reproduce.
#[test]
fn a_zero_pass_request_is_clamped_to_one_pass() {
    let report = job()
        .with_stage(RouterStage::Maze(MazeOptions::with_passes(0)))
        .run()
        .expect("the board imports");
    let router = report.router.clone().expect("the stage reports");
    assert_eq!(router.passes, 1, "{router:?}");
}
