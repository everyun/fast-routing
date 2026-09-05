//! Integration tests for fork/end smoothing — spec §6.4,
//! `PullTightAlgo.smoothen_end_corners_at_trace` and the two per-mode rules.
//!
//! This is the branch `opt_changed_area` takes when ordinary pull-tight cannot
//! improve a trace, and it is the only member of the family that **mutates the
//! board itself** rather than returning a polyline: it removes the trace,
//! re-inserts the adjusted one, splits at both new end corners and normalizes.
//! The properties that matters are therefore board-level:
//!
//! * an acute fork onto a same-net trace is chamfered, and the net stays in one
//!   piece across the change;
//! * every guard upstream has is honoured — shove-fixed traces, the wrapper's
//!   net filter, the clip shape, and a contact that is not a `PolylineTrace`;
//! * the rule is **monotone and terminating**: a second call on a settled fork
//!   finds nothing left to do;
//! * and, because the `acute_angle` branch is deliberately *not*
//!   clearance-checked upstream, **no clearance violation may survive the
//!   round** on a fixture that started clean. That is the invariant the G2.1
//!   "zero added violations" gate reduces to locally, and the one that caught
//!   the compensated/raw half-width confusion in `curr_half_width`.

use fr_board::board::Board;
use fr_board::item::{FixedState, ItemId, ItemKind};
use fr_board::net_nos::NetNos;
use fr_check::clearance::check_clearance;
use fr_geometry::planar::{IntOctagon, IntPoint, Polyline};
use fr_router::insert_forced::ChangedArea;
use fr_router::pull_tight::{PullTightAlgo, DEFAULT_PULL_TIGHT_ACCURACY};
use fr_router::test_support::{TestFixture, TEST_HALF_WIDTH, TEST_NET};

/// The fork point. Three same-net traces meet here, and three is the smallest
/// number that makes a fork at all: `combine_at_start` merges a corner with
/// **exactly one** contact into a single polyline (§7.2), so a two-trace
/// "fork" does not survive insertion -- the board holds one trace and there is
/// nothing to smooth.
const FORK: IntPoint = IntPoint { x: -1500, y: 900 };

/// The main trace the branch doubles back over: a straight horizontal run of
/// [`TEST_NET`] on layer 0, well clear of the fixture's pads.
const MAIN: [IntPoint; 2] = [FORK, IntPoint { x: 1500, y: 900 }];

/// The third leg, which exists only to keep the fork from being combined away.
/// It leaves the fork downwards, so its projection against the branch is
/// negative and neither §6.4 branch fires on it.
const STUB: [IntPoint; 2] = [FORK, IntPoint { x: -1500, y: -500 }];

/// A **second** leg that fires the very same rule as [`MAIN`], so that the scan
/// has to choose between two candidates and its *order* decides the geometry.
///
/// `acute_angle` needs three things of a contact (`PullTightAlgo45.java:452-458`):
/// the branch's own line direction projects positively onto the contact's, the
/// previous end corner is not collinear with it, and — in 45° mode — the
/// contact's direction is orthogonal. `MAIN` leaves the fork along `(+1, 0)` and
/// `UP` along `(0, +1)`; the branch leaves along `(+1, +1)`, whose projection
/// onto both is positive. Both fire.
const UP: [IntPoint; 2] = [FORK, IntPoint { x: -1500, y: 1400 }];

/// The branch: it starts **exactly on** `MAIN`'s first corner — exact corner
/// equality is what `Trace.get_normal_contacts` requires (`Trace.java:172-176`)
/// — and leaves at 45° back over the main run, which is the acid trap §6.4
/// exists to chamfer.
const BRANCH: [IntPoint; 3] = [
    FORK,
    IntPoint { x: -1100, y: 1300 },
    IntPoint { x: -300, y: 1300 },
];

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

fn insert_fixed(board: &mut Board, corners: &[IntPoint], net: i32, fixed: FixedState) -> ItemId {
    board
        .insert_trace_points(corners, 0, TEST_HALF_WIDTH, NetNos::of(net), 1, fixed, None)
        .expect("trace inserted")
}

/// A changed area covering the whole fixture on every layer.
fn whole_board_area(board: &Board) -> ChangedArea {
    let mut area = ChangedArea::new(board.layers().len());
    for layer in 0..board.layers().len() as i32 {
        area.join_point(
            fr_geometry::planar::FloatPoint::new(-3_000.0, -1_500.0),
            layer,
        );
        area.join_point(
            fr_geometry::planar::FloatPoint::new(3_000.0, 1_500.0),
            layer,
        );
    }
    area
}

/// Every trace of `net` on the board, ascending by id.
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

fn polylines_of(board: &Board, net: i32) -> Vec<Polyline> {
    traces_of(board, net)
        .into_iter()
        .filter_map(|id| match &board.store().get(id)?.kind {
            ItemKind::Trace(trace) => Some(trace.polyline.clone()),
            _ => None,
        })
        .collect()
}

/// The board's clearance violations, counted the way the G2.1 referee counts
/// them.
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

/// Builds the `MAIN` + `BRANCH` fork and returns `(fixture, branch id)`.
fn fork_fixture() -> (TestFixture, ItemId) {
    let mut fixture = TestFixture::two_layer_with_pads();
    insert(&mut fixture.board, &MAIN, TEST_NET);
    insert(&mut fixture.board, &STUB, TEST_NET);
    let branch = insert(&mut fixture.board, &BRANCH, TEST_NET);
    (fixture, branch)
}

/// Runs `smoothen_end_corners_at_trace` on `id` with the given knobs.
fn smoothen(
    fixture: &mut TestFixture,
    id: ItemId,
    only_nets: &[i32],
    clip: Option<IntOctagon>,
) -> bool {
    let mut area = whole_board_area(&fixture.board);
    let rules = fixture.rules.clone();
    let mut algo = PullTightAlgo::new(
        &mut fixture.board,
        &rules,
        only_nets,
        clip,
        DEFAULT_PULL_TIGHT_ACCURACY,
        None,
        None,
        0,
    );
    algo.smoothen_end_corners_at_trace(id, &mut area)
}

#[test]
fn an_acute_fork_onto_a_same_net_trace_is_chamfered() {
    let (mut fixture, branch) = fork_fixture();
    let before = polylines_of(&fixture.board, TEST_NET);
    assert_eq!(
        before.len(),
        3,
        "the fork survived insertion as three traces"
    );

    assert!(
        smoothen(&mut fixture, branch, &[], None),
        "an acute 45° fork onto a same-net trace is exactly what §6.4 chamfers"
    );

    let after = polylines_of(&fixture.board, TEST_NET);
    assert_ne!(
        before, after,
        "reporting a change means the board's geometry moved"
    );
    // The chamfer replaces the branch's leading corner with two, so the net
    // gains copper geometry rather than losing it.
    let corners_before: usize = before.iter().map(Polyline::corner_count).sum();
    let corners_after: usize = after.iter().map(Polyline::corner_count).sum();
    assert!(
        corners_after >= corners_before,
        "a chamfer adds a corner, it never removes one: {corners_before} -> {corners_after}"
    );
}

#[test]
fn the_chamfered_fork_adds_no_clearance_violation() {
    let (mut fixture, branch) = fork_fixture();
    assert_eq!(
        violations(&fixture),
        0,
        "the fixture must start clean or the test proves nothing"
    );

    smoothen(&mut fixture, branch, &[], None);

    // Upstream's `acute_angle` branch runs **no** clearance check — it trusts
    // the re-insert path and the following pull-tight passes. That makes this
    // the only place the invariant can be asserted, and it is the assertion
    // that caught `curr_half_width` being installed uncompensated: with the raw
    // half width the oracle inside the `bend` branch checks a shape one
    // clearance compensation too thin and slides traces into foreign pads.
    assert_eq!(
        violations(&fixture),
        0,
        "fork smoothing may never leave a clearance violation behind"
    );
}

#[test]
fn a_settled_fork_is_left_alone_the_second_time() {
    let (mut fixture, branch) = fork_fixture();
    assert!(smoothen(&mut fixture, branch, &[], None));

    // `_1` removed and re-inserted the trace, so the id is stale; the settled
    // geometry is whatever the net carries now. Every one of those traces must
    // report "nothing to do", which is what makes the driver's outer loop
    // terminate.
    for id in traces_of(&fixture.board, TEST_NET) {
        assert!(
            !smoothen(&mut fixture, id, &[], None),
            "a settled fork must report no change, or `opt_changed_area` never converges"
        );
    }
}

#[test]
fn a_shove_fixed_trace_is_refused() {
    let mut fixture = TestFixture::two_layer_with_pads();
    insert(&mut fixture.board, &MAIN, TEST_NET);
    insert(&mut fixture.board, &STUB, TEST_NET);
    let branch = insert_fixed(
        &mut fixture.board,
        &BRANCH,
        TEST_NET,
        FixedState::ShoveFixed,
    );
    assert!(
        !smoothen(&mut fixture, branch, &[], None),
        "`_1`'s first guard (`PullTightAlgo.java:362-364`) refuses a shove-fixed trace"
    );
}

#[test]
fn a_shove_fixed_contact_aborts_the_whole_rule() {
    let mut fixture = TestFixture::two_layer_with_pads();
    insert_fixed(&mut fixture.board, &MAIN, TEST_NET, FixedState::ShoveFixed);
    insert(&mut fixture.board, &STUB, TEST_NET);
    let branch = insert(&mut fixture.board, &BRANCH, TEST_NET);
    assert!(
        !smoothen(&mut fixture, branch, &[], None),
        "a shove-fixed contact is upstream's `else return null` (`:435`), not a skip"
    );
}

#[test]
fn the_wrapper_net_filter_is_equality_not_membership() {
    let (mut fixture, branch) = fork_fixture();
    assert!(
        !smoothen(&mut fixture, branch, &[TEST_NET + 1], None),
        "`nets_equal(only_net_no_arr)` (`:348`) refuses a trace on another net"
    );
    assert!(
        smoothen(&mut fixture, branch, &[TEST_NET], None),
        "and admits the trace whose net array it names"
    );
}

#[test]
fn an_end_corner_outside_the_clip_shape_is_left_alone() {
    let (mut fixture, branch) = fork_fixture();
    // A clip well away from the fork at (-1500, 900).
    let clip = IntOctagon::new(1000, -1000, 2000, 0, -3000, 3000, -3000, 3000);
    assert!(
        !smoothen(&mut fixture, branch, &[], Some(clip)),
        "both rules return null when the end corner is outside `curr_clip_shape`"
    );
}

#[test]
fn a_trace_with_no_trace_contacts_reports_no_change() {
    let mut fixture = TestFixture::two_layer_with_pads();
    // A lone trace touching nothing of its net.
    let lone = insert(
        &mut fixture.board,
        &[IntPoint::new(-800, -900), IntPoint::new(800, -900)],
        TEST_NET,
    );
    assert!(
        !smoothen(&mut fixture, lone, &[], None),
        "no contacts means neither `acute_angle` nor `bend` fires"
    );
}

/// Every corner of every trace of `net`, approximated.
fn corners_of(board: &Board, net: i32) -> Vec<(f64, f64)> {
    polylines_of(board, net)
        .iter()
        .flat_map(|polyline| {
            (0..polyline.corner_count())
                .map(|i| {
                    let p = polyline.corner_approx(i);
                    (p.x, p.y)
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The board's own id for the leg that runs from [`FORK`] to `far`.
///
/// Read from the board rather than kept from `insert`, because inserting the
/// branch re-creates the legs it forks off: in this fixture `MAIN` goes in
/// first and still ends up with a *higher* id than `UP`. Which is the point —
/// the rule is about the ids the *scan* sees, and assuming insertion order
/// would test the wrong thing.
fn leg_at(board: &Board, far: IntPoint) -> ItemId {
    let target = (f64::from(far.x), f64::from(far.y));
    let near = |a: fr_geometry::planar::FloatPoint| {
        (a.x - target.0).abs() < 1.0 && (a.y - target.1).abs() < 1.0
    };
    traces_of(board, TEST_NET)
        .into_iter()
        .find(|id| match &board.store().get(*id).map(|item| &item.kind) {
            Some(ItemKind::Trace(trace)) => {
                let last = trace.polyline.corner_count() - 1;
                near(trace.polyline.corner_approx(0)) || near(trace.polyline.corner_approx(last))
            }
            _ => false,
        })
        .unwrap_or_else(|| panic!("no leg of the net reaches {far:?}"))
}

/// **The scan order is routing-visible, and it is descending by id.**
///
/// `get_start_contacts` returns a `TreeSet<Item>` (`Trace.java:101-104`, via
/// `get_normal_contacts(point, false)` at `:172-199`) and `Item.compareTo` is
/// `other.id_no - this.id_no` (`Item.java:94-99`), so `a` sorts before `b`
/// exactly when `a`'s id is the **larger**: the set iterates highest id first.
/// `scan_fork_contacts` keeps the **last** contact that fires, so among the
/// contacts that fire the winner is the one with the **smallest** id.
///
/// Two legs fire here, and the winner is observable with no reference to ids at
/// all: the `acute_angle` branch opens the new polyline with the winning
/// contact's own line (`new_lines[0] = other_trace_line`,
/// `PullTightAlgo45.java:495`), so the branch's end corner ends up sliding along
/// *that* leg — onto `y = 900` if `MAIN` won, onto `x = -1500` if `UP` did.
/// Sorting the contacts ascending picks the other one, every time.
#[test]
fn a_fork_with_two_firing_contacts_picks_the_lowest_id_of_them() {
    let mut fixture = TestFixture::two_layer_with_pads();
    insert(&mut fixture.board, &MAIN, TEST_NET);
    insert(&mut fixture.board, &STUB, TEST_NET);
    insert(&mut fixture.board, &UP, TEST_NET);
    let branch = insert(&mut fixture.board, &BRANCH, TEST_NET);

    let main_id = leg_at(&fixture.board, MAIN[1]);
    let up_id = leg_at(&fixture.board, UP[1]);
    assert_ne!(main_id, up_id, "the two firing legs are distinct traces");
    // Upstream's rule, stated once: descending scan, last one wins.
    let main_should_win = main_id < up_id;

    let before = corners_of(&fixture.board, TEST_NET);
    assert!(
        !before.iter().any(on_main_interior) && !before.iter().any(on_up_interior),
        "neither leg has an interior corner before the chamfer"
    );

    assert!(
        smoothen(&mut fixture, branch, &[], None),
        "an acute fork with two orthogonal contacts is still a fork §6.4 chamfers"
    );

    let after = corners_of(&fixture.board, TEST_NET);
    let on_main = after.iter().any(on_main_interior);
    let on_up = after.iter().any(on_up_interior);
    assert_eq!(
        (on_main, on_up),
        (main_should_win, !main_should_win),
        "the fork must be chamfered against the smaller of the two firing \
         contact ids (MAIN={main_id:?}, UP={up_id:?}), which is what upstream's \
         descending TreeSet plus keep-the-last leaves; corners after: {after:?}"
    );
}

/// A corner strictly inside `MAIN`'s run.
fn on_main_interior(corner: &(f64, f64)) -> bool {
    let (x, y) = *corner;
    (y - 900.0).abs() < 1.0 && x > -1499.0 && x < 1499.0
}

/// A corner strictly inside `UP`'s run.
fn on_up_interior(corner: &(f64, f64)) -> bool {
    let (x, y) = *corner;
    (x + 1500.0).abs() < 1.0 && y > 901.0 && y < 1399.0
}

#[test]
fn the_net_is_still_one_piece_after_the_chamfer() {
    let (mut fixture, branch) = fork_fixture();
    let before = fixture.board.net_component_count(TEST_NET);
    smoothen(&mut fixture, branch, &[], None);
    let after = fixture.board.net_component_count(TEST_NET);
    assert!(
        after <= before,
        "sliding a fork may not disconnect the net: {before} components before, {after} after"
    );
}
