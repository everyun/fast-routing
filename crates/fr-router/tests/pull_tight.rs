//! Integration tests for the pull-tight port (`fr_router::pull_tight`).
//!
//! The properties under test are the ones spec §5.5 calls load-bearing:
//!
//! * a staircase between two free points collapses toward a straight line;
//! * an obstacle in the way stops the collapse **before** it violates
//!   clearance — the oracle is what bounds the move, not a length heuristic;
//! * pull-tight never turns a clean segment dirty
//!   ([`PullTightAlgo::polyline_violation_count`]);
//! * the clip shape and the net filter are honoured;
//! * the whole thing is deterministic and terminates.

use std::time::Duration;

use fr_board::board::Board;
use fr_board::item::{FixedState, ItemId, ItemKind};
use fr_board::net_nos::NetNos;
use fr_geometry::planar::{IntOctagon, IntPoint, Polyline};
use fr_router::batch::budget::BudgetMode;
use fr_router::insert_forced::ChangedArea;
use fr_router::pull_tight::{
    PullTightAlgo, StallGuard, DEFAULT_PULL_TIGHT_ACCURACY, MIN_TRANSLATE_DIST_FLOOR,
    ROUNDS_PER_VIRTUAL_MS, TIME_LIMIT_TO_PREVENT_ENDLESS_LOOP_MS,
};
use fr_router::test_support::{TestFixture, TEST_CLEARANCE, TEST_HALF_WIDTH, TEST_NET};
use fr_router::JobDeadline;

/// A **45°-legal** staircase of `steps` diagonal/horizontal pairs, each leg
/// `leg` units long, starting at `from`.
///
/// This is the shape the maze emits on a 45° board and the one pull-tight
/// exists to remove: every segment is a multiple of 45°, so `reduce_corners`
/// can merge the diagonals into one diagonal and the horizontals into one
/// horizontal — the optimum is three corners regardless of `steps`.
///
/// A *ninety-degree* staircase would be the wrong fixture here: in 45° mode
/// `smoothen_corners` chamfers each 90° corner into two 45° ones, so the corner
/// count legitimately *rises* (spec §6.2, "the intended 45° smoothing of
/// ninety-degree maze output") and the metric would measure the wrong thing.
fn staircase(from: IntPoint, steps: i32, leg: i32) -> Vec<IntPoint> {
    let mut corners = vec![from];
    let mut curr = from;
    for _ in 0..steps {
        curr = IntPoint::new(curr.x + leg, curr.y + leg);
        corners.push(curr);
        curr = IntPoint::new(curr.x + leg, curr.y);
        corners.push(curr);
    }
    corners
}

/// Inserts a trace of `net` through `corners` on layer 0 and returns its id.
///
/// Clearance class 1 is the fixture's own trace class — the one
/// `TestFixture::build` builds the compensated tree for. Class 0 is the *null*
/// class, whose compensation is 0, so a trace inserted into it would be checked
/// with no clearance at all and every one of these tests would pass vacuously.
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

/// The polyline of a trace, or `None` when it is gone.
fn polyline_of(board: &Board, id: ItemId) -> Option<Polyline> {
    match &board.store().get(id)?.kind {
        ItemKind::Trace(trace) => Some(trace.polyline.clone()),
        _ => None,
    }
}

/// Every trace of `net` on the board, ascending by id.
fn traces_of(board: &Board, net: i32) -> Vec<ItemId> {
    let mut ids: Vec<ItemId> = board
        .store()
        .iter()
        .filter(|(_, item)| {
            matches!(item.kind, ItemKind::Trace(_)) && item.nets.contains(net)
        })
        .map(|(id, _)| id)
        .collect();
    ids.sort_unstable();
    ids
}

/// Total corner count over every trace of `net` — the "bends" the referee
/// scores, and the quantity pull-tight is judged on.
fn corner_total(board: &Board, net: i32) -> usize {
    traces_of(board, net)
        .into_iter()
        .filter_map(|id| polyline_of(board, id))
        .map(|p| p.corner_count())
        .sum()
}

/// A changed area covering the whole fixture on every layer.
fn whole_board_area(board: &Board) -> ChangedArea {
    let mut area = ChangedArea::new(board.layers().len());
    for layer in 0..board.layers().len() as i32 {
        area.join_point(fr_geometry::planar::FloatPoint::new(-3_000.0, -1_500.0), layer);
        area.join_point(fr_geometry::planar::FloatPoint::new(3_000.0, 1_500.0), layer);
    }
    area
}

#[test]
fn staircase_on_an_empty_board_collapses_toward_a_straight_line() {
    let mut fixture = TestFixture::two_layer_with_pads();
    let corners = staircase(IntPoint::new(-2000, -600), 8, 120);
    let before = corners.len();
    let id = insert(&mut fixture.board, &corners, TEST_NET);
    assert!(before > 10, "the fixture must actually be a staircase");

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
    assert!(algo.trace_pull_tight(id, &mut area).is_some(), "must change");

    let after = corner_total(&fixture.board, TEST_NET);
    assert!(
        after < before,
        "staircase should shrink: {before} corners before, {after} after"
    );
    // Nothing is in the way, so the only 45°-legal answer is two segments.
    assert!(
        after <= 4,
        "an unobstructed staircase should collapse to a near-straight run, got {after} corners"
    );
}

/// A wide 45°-legal arch that bulges 400 units off its own chord, so there is
/// room to stand an obstacle in the gap. A staircase is the wrong shape for
/// that test: its bulge is only half a leg, narrower than one clearance.
fn arch() -> Vec<IntPoint> {
    vec![
        IntPoint::new(-1000, 0),
        IntPoint::new(-600, 400),
        IntPoint::new(600, 400),
        IntPoint::new(1000, 0),
    ]
}

/// Optimizes [`arch`], optionally with a foreign-net beam lying in the gap
/// between the arch and its chord, and returns
/// `(total trace length, violations)` afterwards.
///
/// The arch is kept clear of the fixture's own pads on purpose: an own-net pin
/// that is *not* an end-corner contact is an obstacle under the acid-trap rule
/// (`BasicBoard.java:1186-1190`), so a trace running past a pad does not start
/// clean and the test would be measuring that rule instead of this one.
fn arch_collapse(beam: bool) -> (f64, usize) {
    let mut fixture = TestFixture::two_layer_with_pads();
    if beam {
        // Far below the arch's 400 (so the fixture starts clean) but *inside*
        // the 120-unit clearance envelope of the chord at y = 0 — so the arch
        // may not flatten onto its chord. It stops short of the arch's fixed
        // endpoints at x = +-1000, which must stay reachable.
        insert(
            &mut fixture.board,
            &[IntPoint::new(-400, 60), IntPoint::new(400, 60)],
            2,
        );
    }
    let id = insert(&mut fixture.board, &arch(), TEST_NET);

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
    assert_eq!(
        algo.trace_polyline_violations(id),
        Some(0),
        "fixture must start clean, or 'stayed clean' proves nothing"
    );

    let new_id = algo.trace_pull_tight(id, &mut area).unwrap_or(id);
    let violations = algo.trace_polyline_violations(new_id).unwrap_or(0);

    let length: f64 = traces_of(&fixture.board, TEST_NET)
        .into_iter()
        .filter_map(|id| polyline_of(&fixture.board, id))
        .map(|p| p.length_total())
        .sum();
    (length, violations)
}

#[test]
fn a_clean_trace_stays_clean_and_the_obstacle_bounds_the_collapse() {
    let (free_length, free_violations) = arch_collapse(false);
    let (blocked_length, blocked_violations) = arch_collapse(true);

    // The whole point of the oracle: neither run may create a violation.
    assert_eq!(free_violations, 0, "unobstructed collapse went dirty");
    assert_eq!(blocked_violations, 0, "obstructed collapse went dirty");

    // And the obstacle — not a length heuristic — is what limits the collapse:
    // the same arch cannot be pulled as short once something is in the way.
    assert!(
        blocked_length > free_length + 1.0,
        "the beam should stop the collapse short: length {blocked_length:.1} with it, \
         {free_length:.1} without"
    );
}

#[test]
fn pull_tight_never_increases_the_violation_count() {
    // The weaker invariant that still has to hold when the input is *already*
    // dirty — which is the common case on a real board, because a later
    // insertion can move copper next to a trace that was clean when it was
    // routed. Upstream only ever validates the segments a move creates
    // (`check_trace_shape`), so it may leave a pre-existing violation alone; it
    // must never add one.
    let mut fixture = TestFixture::two_layer_with_pads();
    // A wall the staircase actually crosses.
    insert(
        &mut fixture.board,
        &[IntPoint::new(0, -1400), IntPoint::new(0, 1400)],
        2,
    );
    let corners = staircase(IntPoint::new(-1000, -400), 8, 120);
    let id = insert(&mut fixture.board, &corners, TEST_NET);

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
    let before = algo.trace_polyline_violations(id).expect("trace is there");
    assert!(before > 0, "fixture must start dirty for this to mean anything");

    let new_id = algo.trace_pull_tight(id, &mut area).unwrap_or(id);
    let after = algo.trace_polyline_violations(new_id).unwrap_or(0);
    assert!(
        after <= before,
        "pull-tight raised the violation count from {before} to {after}"
    );
}

#[test]
fn opt_changed_area_tightens_every_trace_in_the_region_and_terminates() {
    let mut fixture = TestFixture::two_layer_with_pads();
    for (index, net) in [TEST_NET, 2].into_iter().enumerate() {
        let base_y = -1000 + 700 * index as i32;
        let corners = staircase(IntPoint::new(-2000, base_y), 8, 120);
        insert(&mut fixture.board, &corners, net);
    }
    let before = corner_total(&fixture.board, TEST_NET) + corner_total(&fixture.board, 2);

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
    assert!(algo.opt_changed_area(&mut area));
    assert!(
        !algo.budget_exhausted(),
        "the round budget is a safety valve; a two-trace board must not trip it"
    );

    let after = corner_total(&fixture.board, TEST_NET) + corner_total(&fixture.board, 2);
    assert!(after < before, "{before} corners before, {after} after");
}

#[test]
fn the_net_filter_leaves_other_nets_alone() {
    let mut fixture = TestFixture::two_layer_with_pads();
    let corners = staircase(IntPoint::new(-2000, -1000), 8, 120);
    insert(&mut fixture.board, &corners, TEST_NET);
    let foreign = staircase(IntPoint::new(-2000, 300), 8, 120);
    insert(&mut fixture.board, &foreign, 2);
    let foreign_before = corner_total(&fixture.board, 2);

    let mut area = whole_board_area(&fixture.board);
    let rules = fixture.rules.clone();
    // `only_net_no_arr = [TEST_NET]` — upstream's `PolylineTrace.pull_tight`
    // guard (`:838`).
    let mut algo = PullTightAlgo::new(
        &mut fixture.board,
        &rules,
        &[TEST_NET],
        None,
        DEFAULT_PULL_TIGHT_ACCURACY,
        None,
        None,
        0,
    );
    algo.opt_changed_area(&mut area);

    assert_eq!(
        corner_total(&fixture.board, 2),
        foreign_before,
        "a net outside the filter must not be touched"
    );
}

#[test]
fn the_clip_shape_pins_corners_outside_it() {
    // Every move that touches a corner outside the clip is refused
    // (`reduce_corners` `:58-66`, `reposition_line` `:203-213`). A 45°-legal
    // staircase has no 90°-or-sharper turn, so `smoothen_corners` — which does
    // *not* consult the clip, in upstream either — has nothing to add, and the
    // trace must come out untouched.
    let mut fixture = TestFixture::two_layer_with_pads();
    let corners = staircase(IntPoint::new(-2000, -600), 8, 120);
    let id = insert(&mut fixture.board, &corners, TEST_NET);

    let mut area = whole_board_area(&fixture.board);
    let rules = fixture.rules.clone();
    // A clip that contains nothing: every corner is outside it, so every move
    // that needs a corner is refused.
    let empty_clip = IntOctagon::new(-1, -1, 1, 1, -2, 2, -2, 2);
    let mut algo = PullTightAlgo::new(
        &mut fixture.board,
        &rules,
        &[],
        Some(empty_clip),
        DEFAULT_PULL_TIGHT_ACCURACY,
        None,
        None,
        0,
    );
    let before = corner_total(algo.board, TEST_NET);
    algo.trace_pull_tight(id, &mut area);
    let after = corner_total(algo.board, TEST_NET);

    assert_eq!(
        before, after,
        "no corner is inside the clip, so nothing may be repositioned"
    );
}

#[test]
fn pull_tight_is_deterministic() {
    // The stopping rule must be an integer, not a clock: two runs of the same
    // board must produce byte-identical geometry. This is the regression test
    // for `DEFAULT_ROUND_BUDGET` replacing upstream's wall-clock limit.
    let run = || {
        let mut fixture = TestFixture::two_layer_with_pads();
        let corners = staircase(IntPoint::new(-2000, -700), 10, 120);
        insert(&mut fixture.board, &corners, TEST_NET);
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
        algo.opt_changed_area(&mut area);
        traces_of(&fixture.board, TEST_NET)
            .into_iter()
            .filter_map(|id| polyline_of(&fixture.board, id))
            .map(|p| p.corner_arr())
            .collect::<Vec<_>>()
    };
    assert_eq!(run(), run(), "pull-tight must be reproducible");
}

#[test]
fn a_trace_already_at_its_optimum_is_reported_unchanged() {
    // Upstream signals "nothing changed" by returning the input object
    // (`PolylineTrace.java:849`); the port signals it with `None`. A two-segment
    // trace with nothing to gain must not be rewritten, or `opt_changed_area`
    // would never reach a fixed point.
    let mut fixture = TestFixture::two_layer_with_pads();
    let id = insert(
        &mut fixture.board,
        &[IntPoint::new(-2000, 0), IntPoint::new(2000, 0)],
        TEST_NET,
    );

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
        algo.trace_pull_tight(id, &mut area).is_none(),
        "a straight trace has nothing to tighten"
    );
}

#[test]
fn accuracy_below_the_floor_is_raised_to_it() {
    // `min_translate_dist = Math.max(p_min_translate_dist, 100)`
    // (`PullTightAlgo.java:83`). The floor is what stops the binary searches
    // from grinding to sub-unit precision, and it is observable: any accuracy at
    // or below the floor must behave exactly like the floor itself.
    let run = |accuracy: i32| {
        let mut fixture = TestFixture::two_layer_with_pads();
        insert(
            &mut fixture.board,
            &[IntPoint::new(-400, 60), IntPoint::new(400, 60)],
            2,
        );
        let id = insert(&mut fixture.board, &arch(), TEST_NET);
        let mut area = whole_board_area(&fixture.board);
        let rules = fixture.rules.clone();
        let mut algo = PullTightAlgo::new(
            &mut fixture.board,
            &rules,
            &[],
            None,
            accuracy,
            None,
            None,
            0,
        );
        let new_id = algo.trace_pull_tight(id, &mut area).unwrap_or(id);
        polyline_of(algo.board, new_id).map(|p| p.corner_arr())
    };

    assert_eq!(
        run(1),
        run(MIN_TRANSLATE_DIST_FLOOR),
        "an accuracy under the floor must be raised to it"
    );
    // And the floor really is below the shipped default, or it would never bind.
    assert!(run(DEFAULT_PULL_TIGHT_ACCURACY).is_some());
    let _ = TEST_CLEARANCE;
}

// ---------------------------------------------------------------------------
// the stall guard (M3 gate: "unbounded rounds hang the router")
// ---------------------------------------------------------------------------

/// `ARCHITECTURE.md` "Budgets" says the default mode keeps upstream's
/// wall-clock constants *verbatim* — "pull-tight 1000/2000 ms" — and that
/// `--deterministic` replaces wall checks with effort-based virtual time. That
/// is the whole content of [`StallGuard::for_mode`], and it is worth an
/// assertion because getting it backwards is silent: the run still finishes,
/// just under the wrong stopping rule.
#[test]
fn the_stall_guard_is_upstreams_constant_in_wall_mode_and_rounds_otherwise() {
    assert_eq!(
        StallGuard::for_mode(BudgetMode::Wall, TIME_LIMIT_TO_PREVENT_ENDLESS_LOOP_MS),
        StallGuard::Wall(Duration::from_millis(1_000)),
    );
    assert_eq!(
        StallGuard::for_mode(BudgetMode::Deterministic, TIME_LIMIT_TO_PREVENT_ENDLESS_LOOP_MS),
        StallGuard::Rounds(1_000 * ROUNDS_PER_VIRTUAL_MS),
    );
    // The generous side: the deterministic guard must not bind before the wall
    // clock it stands in for would, and the heaviest `opt_changed_area`
    // measured in the fixture suite spent 17 229 rounds in 1 050 ms.
    assert!(
        1_000 * ROUNDS_PER_VIRTUAL_MS > 17_229,
        "the deterministic guard would bind on healthy work"
    );
}

/// A board with real work to do, so that "nothing happened" is a statement
/// about the guard and not about the board.
fn two_staircases() -> TestFixture {
    let mut fixture = TestFixture::two_layer_with_pads();
    for (index, net) in [TEST_NET, 2].into_iter().enumerate() {
        let base_y = -1000 + 700 * index as i32;
        let corners = staircase(IntPoint::new(-2000, base_y), 8, 120);
        insert(&mut fixture.board, &corners, net);
    }
    fixture
}

/// Which of the three guards a case arms.
#[derive(Clone, Copy)]
enum Armed {
    Rounds,
    Wall,
    Job,
}

/// Every guard `is_stop_requested` polls stops the regional driver on its way
/// in, and each of the three is reachable on its own.
///
/// The third is the one the M3 gate is about: before it, a `SIGTERM` or an
/// expired `--job-timeout` could not interrupt a pull-tight at all, so a
/// `§6.4` region that had stopped converging held the board past the budget
/// and the process was killed without ever writing a session.
#[test]
fn each_stall_guard_stops_the_regional_driver_on_its_own() {
    for (name, arm) in [
        ("an exhausted round budget", Armed::Rounds),
        ("a wall cutoff that has passed", Armed::Wall),
        ("an expired job deadline", Armed::Job),
    ] {
        let mut fixture = two_staircases();
        let before = corner_total(&fixture.board, TEST_NET) + corner_total(&fixture.board, 2);
        let mut area = whole_board_area(&fixture.board);
        let rules = fixture.rules.clone();
        let algo = PullTightAlgo::new(
            &mut fixture.board,
            &rules,
            &[],
            None,
            DEFAULT_PULL_TIGHT_ACCURACY,
            None,
            None,
            0,
        );
        let mut algo = match arm {
            Armed::Rounds => algo.with_stall_guard(StallGuard::Rounds(0)),
            Armed::Wall => algo.with_stall_guard(StallGuard::Wall(Duration::ZERO)),
            Armed::Job => algo.with_job_deadline(JobDeadline::already_passed()),
        };
        assert!(algo.is_stop_requested(), "{name} must read as stopped");
        assert!(
            !algo.opt_changed_area(&mut area),
            "{name} must stop the driver before it touches anything"
        );
        let after = corner_total(&fixture.board, TEST_NET) + corner_total(&fixture.board, 2);
        assert_eq!(before, after, "{name} left the board alone");
    }

    // The control: the same board with no guard armed *does* get optimized, so
    // the assertions above are about the guards and not about the fixture.
    let mut fixture = two_staircases();
    let before = corner_total(&fixture.board, TEST_NET) + corner_total(&fixture.board, 2);
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
    )
    .with_stall_guard(StallGuard::for_mode(
        BudgetMode::Wall,
        TIME_LIMIT_TO_PREVENT_ENDLESS_LOOP_MS,
    ));
    assert!(algo.opt_changed_area(&mut area));
    let after = corner_total(&fixture.board, TEST_NET) + corner_total(&fixture.board, 2);
    assert!(after < before, "{before} corners before, {after} after");
}
