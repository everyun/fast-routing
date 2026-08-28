//! The any-angle pull-tight flavor (`fr_router::pull_tight_any`, upstream
//! `board/PullTightAlgoAnyAngle.java`) and the flavor dispatch that selects it.
//!
//! The properties under test are the ones that distinguish the flavor from the
//! 45° rules — anything they share is already covered by `tests/pull_tight.rs`:
//!
//! * on an `AngleRestriction::None` board a **ninety-degree** staircase
//!   collapses toward its chord. The 45° rules cannot do this: `smoothen_corners`
//!   chamfers each 90° corner into two 45° ones, so under them the corner count
//!   legitimately *rises* (spec §6.2). That difference is the observable
//!   signature of the dispatch;
//! * the dispatch is driven by the board's angle restriction and by nothing
//!   else — the same fixture, the same trace, one flag apart;
//! * the any-angle moves are still clearance-bounded: an obstacle in the gap
//!   stops the collapse, and a clean trace never comes back dirty;
//! * the clip shape is honoured and the result is deterministic.

use fr_board::board::Board;
use fr_board::item::{FixedState, ItemId, ItemKind};
use fr_board::net_nos::NetNos;
use fr_board::shape::AngleRestriction;
use fr_geometry::planar::{FloatPoint, IntOctagon, IntPoint, Polyline};
use fr_router::insert_forced::ChangedArea;
use fr_router::pull_tight::{PullTightAlgo, DEFAULT_PULL_TIGHT_ACCURACY};
use fr_router::test_support::{TestFixture, TEST_HALF_WIDTH, TEST_NET};

/// A ninety-degree staircase: `steps` right-angle pairs of `leg` units.
///
/// Deliberately **not** 45°-legal in shape — this is exactly the input the two
/// flavors disagree about.
fn ninety_staircase(from: IntPoint, steps: i32, leg: i32) -> Vec<IntPoint> {
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

fn polyline_of(board: &Board, id: ItemId) -> Option<Polyline> {
    match &board.store().get(id)?.kind {
        ItemKind::Trace(trace) => Some(trace.polyline.clone()),
        _ => None,
    }
}

fn traces_of(board: &Board, net: i32) -> Vec<ItemId> {
    let mut ids: Vec<ItemId> = board
        .store()
        .iter()
        .filter(|(_, item)| matches!(item.kind, ItemKind::Trace(_)) && item.nets.contains(net))
        .map(|(id, _)| id)
        .collect();
    ids.sort_unstable();
    ids
}

/// Total corner count of every trace of `net` — the metric, because a move may
/// split the trace and the surviving id is not the one we started with.
fn corner_total(board: &Board, net: i32) -> usize {
    traces_of(board, net)
        .into_iter()
        .filter_map(|id| polyline_of(board, id))
        .map(|polyline| polyline.corner_count())
        .sum()
}

fn whole_board_area(board: &Board) -> ChangedArea {
    let mut area = ChangedArea::new(board.layers().len());
    for layer in 0..board.layers().len() as i32 {
        area.join_point(FloatPoint::new(-3_000.0, -1_500.0), layer);
        area.join_point(FloatPoint::new(3_000.0, 1_500.0), layer);
    }
    area
}

/// Pull-tights one trace on a fixture built in `restriction` mode and reports
/// `(changed, corner_total_after)`.
fn collapse(restriction: AngleRestriction, corners: &[IntPoint]) -> (bool, usize) {
    let mut fixture = TestFixture::two_layer_with_pads();
    fixture.board.set_angle_restriction(restriction);
    let id = insert(&mut fixture.board, corners, TEST_NET);
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
    let changed = algo.trace_pull_tight(id, &mut area).is_some();
    (changed, corner_total(&fixture.board, TEST_NET))
}

#[test]
fn a_ninety_degree_staircase_collapses_on_an_any_angle_board() {
    let corners = ninety_staircase(IntPoint::new(-2000, -600), 8, 120);
    let before = corners.len();
    assert!(before > 10, "the fixture must actually be a staircase");

    let (changed, after) = collapse(AngleRestriction::None, &corners);
    assert!(changed, "the any-angle flavor must change a staircase");
    assert!(
        after < before,
        "staircase should shrink: {before} corners before, {after} after"
    );
    // Nothing is in the way and any direction is legal, so the optimum is the
    // chord itself: two corners.
    assert!(
        after <= 4,
        "an unobstructed staircase should collapse to a near-straight run on an \
         any-angle board, got {after} corners"
    );
}

/// A staircase of `steps` (horizontal `dx`, vertical `dy`) pairs.
///
/// With `dx != dy` the chord is at a **non-45°** angle, which is the shape the
/// two flavors cannot both reach: the 45° rules may only emit the eight
/// directions, so their best answer is one diagonal plus one axis run, strictly
/// longer than the chord the any-angle rules can lay down.
fn shallow_staircase(from: IntPoint, steps: i32, dx: i32, dy: i32) -> Vec<IntPoint> {
    let mut corners = vec![from];
    let mut curr = from;
    for _ in 0..steps {
        curr = IntPoint::new(curr.x + dx, curr.y);
        corners.push(curr);
        curr = IntPoint::new(curr.x, curr.y + dy);
        corners.push(curr);
    }
    corners
}

/// Total wire length of every trace of `net`.
fn length_total(board: &Board, net: i32) -> f64 {
    traces_of(board, net)
        .into_iter()
        .filter_map(|id| polyline_of(board, id))
        .map(|polyline| polyline.length_total())
        .sum()
}

/// [`collapse`], reporting the resulting wire length instead of the corner
/// count.
fn collapse_length(restriction: AngleRestriction, corners: &[IntPoint]) -> f64 {
    let mut fixture = TestFixture::two_layer_with_pads();
    fixture.board.set_angle_restriction(restriction);
    let id = insert(&mut fixture.board, corners, TEST_NET);
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
    let _ = algo.trace_pull_tight(id, &mut area);
    length_total(&fixture.board, TEST_NET)
}

#[test]
fn the_flavor_is_selected_by_the_boards_angle_restriction() {
    // The same trace, the same fixture, one flag apart. The chord of this
    // staircase runs 3000 across and 450 up — no multiple of 45°. The 45° rules
    // can do no better than `450·√2 + 2550 ≈ 3186`; the any-angle rules can lay
    // the chord itself, `√(3000² + 450²) ≈ 3034`. If the dispatch regressed to
    // "always 45" the two runs would produce the same length.
    let corners = shallow_staircase(IntPoint::new(-2000, -600), 15, 200, 30);
    let any_angle = collapse_length(AngleRestriction::None, &corners);
    let forty_five = collapse_length(AngleRestriction::FortyFive, &corners);
    assert!(
        any_angle < forty_five - 1.0,
        "the any-angle flavor must lay a shorter path than the 45° one on a \
         non-45° chord: {any_angle:.1} vs {forty_five:.1}"
    );
    // Sanity: neither may be shorter than the straight line between the ends.
    let chord = ((3000.0f64).powi(2) + (450.0f64).powi(2)).sqrt();
    assert!(
        any_angle >= chord - 1.0,
        "the any-angle result is shorter than the chord, which is impossible: \
         {any_angle:.1} < {chord:.1}"
    );
}

#[test]
fn the_any_angle_flavor_never_increases_the_violation_count() {
    // `with_foreign_trace` puts a different net's copper across the fixture, so
    // the oracle has something to say. Whatever the rules do to the staircase,
    // no segment of it may end up violating that was clean before.
    let mut fixture = TestFixture::with_foreign_trace();
    fixture.board.set_angle_restriction(AngleRestriction::None);
    let corners = ninety_staircase(IntPoint::new(-1200, -400), 6, 100);
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
    let before = algo
        .trace_polyline_violations(id)
        .expect("the trace is optimizable");
    let after_id = algo.trace_pull_tight(id, &mut area).unwrap_or(id);
    let after = algo.trace_polyline_violations(after_id).unwrap_or(before);
    assert!(
        after <= before,
        "pull-tight turned a clean trace dirty: {before} violating segments before, \
         {after} after"
    );
}

#[test]
fn the_clip_shape_pins_corners_outside_it() {
    // Every any-angle rule tests its corners against `curr_clip_shape` before
    // moving them (`:73`, `:275`, `:375`, `:466`, `:653`). A clip that contains
    // none of the staircase's corners must therefore leave it alone.
    let mut fixture = TestFixture::two_layer_with_pads();
    fixture.board.set_angle_restriction(AngleRestriction::None);
    let corners = ninety_staircase(IntPoint::new(-2000, -600), 8, 120);
    let id = insert(&mut fixture.board, &corners, TEST_NET);
    let before = polyline_of(&fixture.board, id).expect("inserted");

    // A clip far away from every corner of the trace.
    let clip = IntOctagon::new(2_000, 1_000, 2_400, 1_400, -4_000, 4_000, 0, 8_000);
    let mut area = whole_board_area(&fixture.board);
    let rules = fixture.rules.clone();
    let mut algo = PullTightAlgo::new(
        &mut fixture.board,
        &rules,
        &[],
        Some(clip),
        DEFAULT_PULL_TIGHT_ACCURACY,
        None,
        None,
        0,
    );
    assert!(
        algo.trace_pull_tight(id, &mut area).is_none(),
        "a clip containing none of the corners must leave the trace alone"
    );
    let after = polyline_of(&fixture.board, id).expect("still there");
    assert_eq!(before.corner_arr(), after.corner_arr());
}

#[test]
fn the_any_angle_flavor_is_deterministic() {
    let corners = ninety_staircase(IntPoint::new(-2000, -600), 8, 120);
    let first = collapse(AngleRestriction::None, &corners);
    let second = collapse(AngleRestriction::None, &corners);
    assert_eq!(first, second, "two identical runs must agree");
}

#[test]
fn a_trace_already_at_its_optimum_is_reported_unchanged() {
    // Two corners, nothing in the way: there is no shorter any-angle path, so
    // every rule must decline and `trace_pull_tight` must report "no change".
    let mut fixture = TestFixture::two_layer_with_pads();
    fixture.board.set_angle_restriction(AngleRestriction::None);
    let straight = [IntPoint::new(-1500, -300), IntPoint::new(1500, -300)];
    let id = insert(&mut fixture.board, &straight, TEST_NET);
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
        "a straight trace has nothing to gain"
    );
}
