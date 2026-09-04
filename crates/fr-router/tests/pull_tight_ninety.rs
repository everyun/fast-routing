//! Integration tests for the 90° pull-tight flavor (`fr_router::pull_tight90`,
//! upstream `board/PullTightAlgo90.java`) and the flavor dispatch.
//!
//! The properties under test are the ones `kicad-coldfire-unrouted` (a
//! `snap_angle ninety_degree` board) failed G4 on when `NINETY_DEGREE` was
//! routed into the 45° rules: 190 of 197 emitted segments were diagonals.
//!
//! * on an `AngleRestriction::Ninety` board, pull-tight **never chamfers** —
//!   every segment of every trace stays axis-aligned, because upstream's
//!   pipeline (`try_skip_second_corner` / `try_skip_corners` /
//!   `reposition_lines`) contains no smoothing rule;
//! * the 90° rules still optimize: a redundant jog is removed;
//! * fork/end smoothing is a no-op — both `PullTightAlgo90` hooks return
//!   `null` upstream (`:135-143`).

use fr_board::board::Board;
use fr_board::item::{FixedState, ItemId, ItemKind};
use fr_board::net_nos::NetNos;
use fr_board::shape::AngleRestriction;
use fr_check::clearance::check_clearance;
use fr_geometry::planar::{FloatPoint, IntPoint, Polyline};
use fr_router::insert_forced::ChangedArea;
use fr_router::pull_tight::{PullTightAlgo, DEFAULT_PULL_TIGHT_ACCURACY};
use fr_router::test_support::{TestFixture, TEST_HALF_WIDTH, TEST_NET};

/// A ninety-degree staircase: `steps` right-angle pairs of `leg` units. Every
/// segment is axis-aligned; a 45° chamfer is what the bug produced.
fn staircase(from: IntPoint, steps: i32, leg: i32) -> Vec<IntPoint> {
    let mut corners = vec![from];
    let mut curr = from;
    for _ in 0..steps {
        curr = IntPoint::new(curr.x + leg, curr.y);
        corners.push(curr);
        curr = IntPoint::new(curr.x, curr.y + leg);
        corners.push(curr);
    }
    corners
}

fn insert(board: &mut Board, corners: &[IntPoint], net: i32) -> ItemId {
    board
        .insert_trace_points(
            corners,
            0,
            TEST_HALF_WIDTH,
            NetNos::of(net),
            1,
            FixedState::Unfixed,
            None,
        )
        .expect("trace inserted")
}

fn polylines_of(board: &Board, net: i32) -> Vec<Polyline> {
    let mut ids: Vec<ItemId> = board
        .store()
        .iter()
        .filter(|(_, item)| matches!(item.kind, ItemKind::Trace(_)) && item.nets.contains(net))
        .map(|(id, _)| id)
        .collect();
    ids.sort_unstable();
    ids.into_iter()
        .filter_map(|id| match &board.store().get(id)?.kind {
            ItemKind::Trace(trace) => Some(trace.polyline.clone()),
            _ => None,
        })
        .collect()
}

/// Every segment of every trace of `net`, as (dx, dy) float displacements.
fn segments_of(board: &Board, net: i32) -> Vec<(f64, f64)> {
    let mut result = Vec::new();
    for polyline in polylines_of(board, net) {
        for i in 0..polyline.corner_count() - 1 {
            let a = polyline.corner_approx(i);
            let b = polyline.corner_approx(i + 1);
            result.push((b.x - a.x, b.y - a.y));
        }
    }
    result
}

fn whole_board_area(board: &Board) -> ChangedArea {
    let mut area = ChangedArea::new(board.layers().len());
    for layer in 0..board.layers().len() as i32 {
        area.join_point(FloatPoint::new(-3_000.0, -1_500.0), layer);
        area.join_point(FloatPoint::new(3_000.0, 1_500.0), layer);
    }
    area
}

fn pull_tight(fixture: &mut TestFixture, id: ItemId) -> bool {
    let mut area = whole_board_area(&fixture.board);
    let rules = fixture.rules.clone();
    let mut algo = PullTightAlgo::new(
        &mut fixture.board,
        &rules,
        &[],
        None,
        DEFAULT_PULL_TIGHT_ACCURACY,
        None,
        None,
        0,
    );
    algo.trace_pull_tight(id, &mut area).is_some()
}

fn violations(fixture: &TestFixture) -> usize {
    let shapes = fr_check::shape_store(&fixture.board);
    let view = fr_check::view::StoreView::new(
        fixture.board.store(),
        &shapes,
        &fixture.rules.clearance_matrix,
        fixture.board.layers().len(),
    );
    check_clearance(&view).len()
}

#[test]
fn a_ninety_degree_board_never_gets_a_diagonal_from_pull_tight() {
    let mut fixture = TestFixture::two_layer_with_pads();
    fixture.board.set_angle_restriction(AngleRestriction::Ninety);
    let corners = staircase(IntPoint::new(-2000, -600), 8, 120);
    let id = insert(&mut fixture.board, &corners, TEST_NET);
    assert_eq!(violations(&fixture), 0, "the fixture must start clean");

    let _ = pull_tight(&mut fixture, id);

    let segments = segments_of(&fixture.board, TEST_NET);
    assert!(!segments.is_empty(), "the staircase must survive optimization");
    for (dx, dy) in &segments {
        assert!(
            *dx == 0.0 || *dy == 0.0,
            "a 90° board must stay axis-aligned, got a segment with \
             dx={dx}, dy={dy} — the chamfering 45° rules ran"
        );
    }
    assert_eq!(
        violations(&fixture),
        0,
        "the 90° moves are clearance-bounded like every other flavor"
    );
}

#[test]
fn a_ninety_degree_board_still_loses_a_redundant_jog() {
    // A U-shaped bump on an otherwise straight run: the two 90° corners of the
    // bump are exactly what `try_skip_corners` removes — proof that the 90°
    // pipeline is not just a refusal to move anything.
    let bump = [
        IntPoint::new(-2400, -900),
        IntPoint::new(-1200, -900),
        IntPoint::new(-1200, -600),
        IntPoint::new(-600, -600),
        IntPoint::new(-600, -900),
        IntPoint::new(600, -900),
    ];
    let mut fixture = TestFixture::two_layer_with_pads();
    fixture.board.set_angle_restriction(AngleRestriction::Ninety);
    let id = insert(&mut fixture.board, &bump, TEST_NET);
    let before: usize = polylines_of(&fixture.board, TEST_NET)
        .iter()
        .map(Polyline::corner_count)
        .sum();

    let _ = pull_tight(&mut fixture, id);

    let after: usize = polylines_of(&fixture.board, TEST_NET)
        .iter()
        .map(Polyline::corner_count)
        .sum();
    assert!(
        after < before,
        "the bump should be optimized away: {before} corners before, {after} after"
    );
    for (dx, dy) in segments_of(&fixture.board, TEST_NET) {
        assert!(dx == 0.0 || dy == 0.0, "still axis-aligned: dx={dx}, dy={dy}");
    }
}

#[test]
fn fork_smoothing_is_a_noop_on_a_ninety_degree_board() {
    // The acute-fork fixture of `pull_tight_fork_smoothing.rs`: a branch that
    // doubles back over a same-net trace at 45°. On a 90° board both
    // `PullTightAlgo90` hooks return null, so nothing may change — the chamfer
    // would introduce a diagonal.
    const FORK: IntPoint = IntPoint { x: -1500, y: 900 };
    let main = [FORK, IntPoint { x: 1500, y: 900 }];
    let stub = [FORK, IntPoint { x: -1500, y: -500 }];
    let branch = [FORK, IntPoint { x: -1100, y: 1300 }, IntPoint { x: -300, y: 1300 }];

    let mut fixture = TestFixture::two_layer_with_pads();
    fixture.board.set_angle_restriction(AngleRestriction::Ninety);
    insert(&mut fixture.board, &main, TEST_NET);
    insert(&mut fixture.board, &stub, TEST_NET);
    let branch_id = insert(&mut fixture.board, &branch, TEST_NET);
    let before = polylines_of(&fixture.board, TEST_NET);

    let mut area = whole_board_area(&fixture.board);
    let rules = fixture.rules.clone();
    let mut algo = PullTightAlgo::new(
        &mut fixture.board,
        &rules,
        &[],
        None,
        DEFAULT_PULL_TIGHT_ACCURACY,
        None,
        None,
        0,
    );
    assert!(
        !algo.smoothen_end_corners_at_trace(branch_id, &mut area),
        "PullTightAlgo90's fork-smoothing hooks return null upstream"
    );
    assert_eq!(
        before,
        polylines_of(&fixture.board, TEST_NET),
        "a no-op must not move any geometry"
    );
}
