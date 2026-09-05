//! Step 8 of `RoutingBoard.insert_forced_trace_polyline` (`:821-889`): the
//! in-insert pull-tight, keyed on the segment's end corner.
//!
//! The behaviour is on by default, as it is upstream
//! ([`fr_router::insert_forced::INSERT_PULL_TIGHT_DEFAULT`]); these tests drive
//! both sides of [`ForcedInserter::with_insert_pull_tight`] explicitly, because
//! the switch exists to be A/B'd. What they pin is exactly what it buys and what
//! it must never cost:
//!
//! 1. the detour goes away. A located chain that staircases its way across the
//!    board comes out of the insertion straightened, which is the whole of what
//!    upstream's `:868-869` buys ("to avoid, that a separate handling for moving
//!    backwards in the own trace line becomes necessary");
//! 2. with the switch off the copper is left exactly as the caller drew it, so
//!    the ON case really is step 8 doing the work and not something else in the
//!    insertion path;
//! 3. the **keep point survives**. `new_corner` is the ok-point the caller's
//!    per-window loop continues from; if pull-tight moved it the next window
//!    would start off copper. This is the invariant
//!    `split_traces_at_keep_point` exists to protect;
//! 4. the switch cannot turn a clean board dirty: a foreign trace at clearance
//!    still is at clearance afterwards.

use fr_board::board::Board;
use fr_board::item::{ItemId, ItemKind};
use fr_geometry::planar::{IntBox, IntPoint, Point, Polyline};
use fr_router::insert_forced::{ForcedInserter, INSERT_PULL_TIGHT_DEFAULT};
use fr_router::test_support::{TestFixture, TEST_HALF_WIDTH, TEST_NET};

fn bounds() -> IntBox {
    IntBox::new(-4000, -2500, 4000, 2500)
}

/// A 45°-legal staircase — the shape the locator emits, one corner pair per
/// door it crossed, and the shape step 8 exists to straighten.
///
/// A *fold* (three collinear corners doubling back) would be the wrong fixture:
/// `Polyline`'s own constructor drops a line parallel to its predecessor, so a
/// fold never survives long enough to reach pull-tight and the test would pass
/// with step 8 switched off.
fn staircase() -> Vec<IntPoint> {
    let mut corners = vec![IntPoint::new(-2000, -600)];
    let mut curr = corners[0];
    for _ in 0..8 {
        curr = IntPoint::new(curr.x + 120, curr.y + 120);
        corners.push(curr);
        curr = IntPoint::new(curr.x + 120, curr.y);
        corners.push(curr);
    }
    corners
}

/// Every trace of `net` on the board, ascending by id.
fn traces_of(board: &Board, net: i32) -> Vec<ItemId> {
    let mut ids: Vec<ItemId> = board
        .store()
        .iter()
        .filter(|(_, item)| matches!(item.kind, ItemKind::Trace(_)) && item.contains_net(net))
        .map(|(id, _)| id)
        .collect();
    ids.sort_unstable();
    ids
}

/// Total corner count over every trace of `net`.
///
/// Corner count, not length: a *monotone* staircase and the L it collapses to
/// have exactly the same length (`8·120·√2 + 8·120` either way), so length
/// cannot see this move at all. What pull-tight buys on a 45° board is corners.
fn corner_total(board: &Board, net: i32) -> usize {
    traces_of(board, net)
        .into_iter()
        .filter_map(|id| match &board.store().get(id)?.kind {
            ItemKind::Trace(trace) => Some(trace.polyline.corner_count()),
            _ => None,
        })
        .sum()
}

/// Inserts [`staircase`] with the switch in the given position and reports
/// `(ok_point, corner count of the routed net's copper)`.
fn insert_staircase(pull_tight: bool) -> (Option<Point>, usize) {
    let mut fixture = TestFixture::two_layer_with_pads();
    let ctrl = fixture.control();
    let corners = staircase();
    let polyline = Polyline::from_int_points(&corners);
    let ok_point = {
        let mut inserter = ForcedInserter::new(&mut fixture.board, &fixture.rules, &ctrl, bounds())
            .with_insert_pull_tight(pull_tight);
        inserter.insert_forced_trace_polyline(&polyline, TEST_HALF_WIDTH, 0, i32::MAX)
    };
    (ok_point, corner_total(&fixture.board, TEST_NET))
}

#[test]
fn switching_step_8_off_leaves_the_staircase_alone() {
    const {
        assert!(
            INSERT_PULL_TIGHT_DEFAULT,
            "the default must stay upstream's: step 8 runs unless a caller says not to"
        )
    };
    let corners = staircase();
    let (ok_point, corners_after) = insert_staircase(false);
    assert_eq!(
        ok_point,
        Some(Point::Int(corners[corners.len() - 1])),
        "the whole chain goes in on an empty board"
    );
    assert_eq!(
        corners_after,
        corners.len(),
        "with step 8 off the copper is exactly what the caller drew"
    );
}

#[test]
fn the_in_insert_pull_tight_straightens_the_staircase() {
    let corners = staircase();
    let (ok_point, corners_after) = insert_staircase(true);
    assert_eq!(
        ok_point,
        Some(Point::Int(corners[corners.len() - 1])),
        "the ok-point protocol is unchanged by the switch"
    );
    assert!(
        corners_after < corners.len(),
        "step 8 must straighten the run: {corners_after} corners with, \
         {} as drawn",
        corners.len()
    );
    // Nothing is in the way and the board is a 45° one, so the tautest legal
    // answer is one diagonal plus one axis run: three corners.
    assert!(
        corners_after <= 4,
        "an unobstructed staircase should collapse to a near-straight run, \
         got {corners_after} corners"
    );
}

#[test]
fn the_keep_point_stays_a_corner_of_the_nets_copper() {
    // The ok-point is where the caller's next window starts. If pull-tight moved
    // it, the next `insert_forced_trace_polyline` would begin off copper and the
    // connection would come apart — this is the invariant
    // `split_traces_at_keep_point` protects.
    let mut fixture = TestFixture::two_layer_with_pads();
    let ctrl = fixture.control();
    let polyline = Polyline::from_int_points(&staircase());
    let ok_point = {
        let mut inserter = ForcedInserter::new(&mut fixture.board, &fixture.rules, &ctrl, bounds())
            .with_insert_pull_tight(true);
        inserter
            .insert_forced_trace_polyline(&polyline, TEST_HALF_WIDTH, 0, i32::MAX)
            .expect("no damage")
    };
    let keep = match ok_point {
        Point::Int(point) => point,
        other => panic!("the ok-point must be an integer corner, got {other:?}"),
    };
    let on_a_corner = traces_of(&fixture.board, TEST_NET)
        .into_iter()
        .filter_map(|id| match &fixture.board.store().get(id)?.kind {
            ItemKind::Trace(trace) => Some(trace.polyline.corner_arr()),
            _ => None,
        })
        .any(|corners| corners.contains(&Point::Int(keep)));
    assert!(
        on_a_corner,
        "the keep point {keep:?} is no longer a corner of the net's copper"
    );
}

/// Inserts the staircase into `with_foreign_trace` and reports
/// `(foreign corner count, whether the foreign net is still in one piece)`.
///
/// The shove bends the foreign trace on its own — that is what the shove is for
/// — so the question here is only whether *step 8* adds anything to that.
fn insert_over_foreign(pull_tight: bool) -> (usize, bool) {
    const FOREIGN_NET: i32 = 2;
    let mut fixture = TestFixture::with_foreign_trace();
    let ctrl = fixture.control();
    let polyline = Polyline::from_int_points(&staircase());
    {
        let mut inserter = ForcedInserter::new(&mut fixture.board, &fixture.rules, &ctrl, bounds())
            .with_insert_pull_tight(pull_tight);
        inserter.insert_forced_trace_polyline(&polyline, TEST_HALF_WIDTH, 0, i32::MAX);
    }
    let one_piece = fixture.board.net_component_count(FOREIGN_NET) <= 1;
    (corner_total(&fixture.board, FOREIGN_NET), one_piece)
}

#[test]
fn the_switch_leaves_a_foreign_net_exactly_as_the_shove_left_it() {
    // Step 8 calls `trace_pull_tight` on the routed net's trace and on nothing
    // else, so whatever the shove did to the foreign net must be all that
    // happened to it — with the switch on and with it off alike.
    let (off, off_one_piece) = insert_over_foreign(false);
    let (on, on_one_piece) = insert_over_foreign(true);
    assert!(off > 0, "the fixture must actually carry foreign copper");
    assert!(
        off_one_piece && on_one_piece,
        "the foreign net must stay in one piece either way"
    );
    assert_eq!(
        on, off,
        "step 8 optimizes the routed net only; the foreign net must come out of \
         the insertion exactly as the shove left it"
    );
}
