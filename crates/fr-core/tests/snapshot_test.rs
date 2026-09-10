use fr_board::item::FixedState as BoardFixedState;
use fr_core::pipeline::{MazeOptions, RouterStage, RoutingJob};
use fr_core::snapshot::{default_layer_color, snapshot, FixState, Stats};
use std::path::PathBuf;

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
fn issue026_snapshot_counts_and_ratsnest_parity() {
    let path = upstream_fixture("Issue026-J2_reference.dsn");
    let report = RoutingJob::from_path(&path)
        .expect("the fixture reads")
        .run()
        .expect("Issue026 imports");

    let model = snapshot(&report.design.board, Stats::default());

    // Traces and vias are 0 on unrouted Issue026
    assert_eq!(model.traces.len(), 0, "unrouted fixture has 0 traces");
    assert_eq!(model.vias.len(), 0, "unrouted fixture has 0 vias");
    assert_eq!(model.traces.len(), report.census.traces);
    assert_eq!(model.vias.len(), report.census.vias);

    // Pads and areas must be present
    assert_eq!(model.pads.len(), 65);
    assert_eq!(model.areas.len(), 22);

    // Ratsnest count must agree exactly with g21-referee's incomplete count (33)
    assert_eq!(model.ratsnest.len(), 33);
    assert_eq!(
        model.ratsnest.len(),
        report.connectivity.incomplete_count,
        "ratsnest count must match referee incomplete count"
    );
    assert_eq!(
        model.ratsnest.len() as u64,
        report.stats.incomplete_count,
        "ratsnest count must match stats incomplete count"
    );

    // Bounding box matches board outline: [1060450, -984250, 1530350, -730250]
    assert_eq!(model.bbox, [1060450.0, -984250.0, 1530350.0, -730250.0]);

    // Layers check
    assert_eq!(model.layers.len(), 2);
    assert_eq!(model.layers[0].name, "F.Cu");
    assert!(model.layers[0].is_signal);
    assert_eq!(model.layers[1].name, "B.Cu");
    assert!(model.layers[1].is_signal);
}

#[test]
fn issue026_routed_snapshot_parity() {
    let path = upstream_fixture("Issue026-J2_reference.dsn");
    let report = RoutingJob::from_path(&path)
        .expect("the fixture reads")
        .with_stage(RouterStage::Maze(MazeOptions::with_passes(1)))
        .run()
        .expect("Issue026 routes");

    let stats = Stats {
        incomplete: report.stats.incomplete_count as u32,
        violations: report.stats.clearance_violation_count as u32,
        pass: 1,
        elapsed_s: 0.1,
        score: report.score.normalized,
        items: report.census.total() as u32,
    };

    let model = snapshot(&report.design.board, stats);

    // Routed board must have traces and vias
    assert!(!model.traces.is_empty(), "routed board must have traces");
    assert_eq!(model.traces.len(), report.census.traces);
    assert_eq!(model.vias.len(), report.census.vias);

    // Ratsnest count must still match the referee's incomplete count
    assert_eq!(
        model.ratsnest.len(),
        report.connectivity.incomplete_count,
        "ratsnest count must match referee incomplete count"
    );
    assert_eq!(
        model.ratsnest.len() as u64,
        report.stats.incomplete_count,
        "ratsnest count must match stats incomplete count"
    );
}

#[test]
fn layer_color_palette_matches_java_default() {
    // 2-layer board: front red, back blue
    let f_cu = default_layer_color(0, 2, true, 0);
    assert_eq!(f_cu, [200.0 / 255.0, 52.0 / 255.0, 52.0 / 255.0, 1.0]);

    let b_cu = default_layer_color(1, 2, true, 1);
    assert_eq!(b_cu, [77.0 / 255.0, 127.0 / 255.0, 196.0 / 255.0, 1.0]);

    // 4-layer board: front red, inner1 green, inner2 orange, back blue
    let in1 = default_layer_color(1, 4, true, 1);
    assert_eq!(in1, [127.0 / 255.0, 200.0 / 255.0, 127.0 / 255.0, 1.0]);

    let in2 = default_layer_color(2, 4, true, 2);
    assert_eq!(in2, [206.0 / 255.0, 125.0 / 255.0, 44.0 / 255.0, 1.0]);

    // Non-signal layer: gray
    let plane = default_layer_color(1, 4, false, 0);
    assert_eq!(plane, [128.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0, 1.0]);
}

#[test]
fn fix_state_roundtrip() {
    assert_eq!(FixState::from(BoardFixedState::Unfixed), FixState::Unfixed);
    assert_eq!(
        FixState::from(BoardFixedState::ShoveFixed),
        FixState::ShoveFixed
    );
    assert_eq!(
        FixState::from(BoardFixedState::UserFixed),
        FixState::UserFixed
    );
    assert_eq!(
        FixState::from(BoardFixedState::SystemFixed),
        FixState::SystemFixed
    );

    assert_eq!(
        BoardFixedState::from(FixState::Unfixed),
        BoardFixedState::Unfixed
    );
    assert_eq!(
        BoardFixedState::from(FixState::ShoveFixed),
        BoardFixedState::ShoveFixed
    );
    assert_eq!(
        BoardFixedState::from(FixState::UserFixed),
        BoardFixedState::UserFixed
    );
    assert_eq!(
        BoardFixedState::from(FixState::SystemFixed),
        BoardFixedState::SystemFixed
    );
}
