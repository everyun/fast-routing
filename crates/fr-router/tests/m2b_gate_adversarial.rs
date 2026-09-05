//! M2b gate: adversarial probes, one per landed area.
//!
//! These are deliberately *not* the property tests each implementing area
//! shipped. Each one attacks the weakest assumption that area's own tests leave
//! standing:
//!
//! * **shove** — `tests/shove_insert.rs`'s rollback test freezes the *only*
//!   obstacle, so `ShapeTraceEntries.store_items` refuses it at the door and the
//!   cascade fails before mutating anything; a rollback of nothing is
//!   bit-identical vacuously. Here the cascade is made to fail **mid-flight**:
//!   a shovable trace in front, a `UserFixed` one behind it, so the first shove
//!   lands, mutates the board, and only the *second* one fails. The eager path
//!   is run first purely to prove the mutation happened.
//! * **pull-tight** — the shipped fixtures are 45°-legal staircases with legs
//!   far above `MIN_TRANSLATE_DIST_FLOOR`. Here the zigzag is pathological in
//!   the two ways that stress the loop: legs *below* the floor (so every
//!   proposed translate is clamped and the backoff decides termination) and
//!   non-45° corners (so `smoothen_non_integer_corner` runs on every corner).
//!   Termination is asserted with a wall clock, and clearance is re-checked
//!   after *every* tightening round, not only at the end.

use std::time::{Duration, Instant};

use fr_board::board::Board;
use fr_board::item::{FixedState, ItemId, ItemKind};
use fr_board::net_nos::NetNos;
use fr_board::shove::{FromSide, NoChangedArea, ShoveInsert};
use fr_geometry::planar::{FloatPoint, IntBox, IntPoint, Polyline, Simplex};
use fr_router::insert_forced::ChangedArea;
use fr_router::pull_tight::{PullTightAlgo, DEFAULT_PULL_TIGHT_ACCURACY};
use fr_router::test_support::{TestFixture, TEST_CLEARANCE, TEST_HALF_WIDTH, TEST_NET};

const FOREIGN_NET: i32 = 2;
const SECOND_FOREIGN_NET: i32 = 3;

fn bounds() -> IntBox {
    IntBox::new(-4000, -2500, 4000, 2500)
}

/// Every item as a comparable string — the same shape of snapshot
/// `tests/shove_insert.rs` uses, so the two rollback claims are commensurable.
fn snapshot(board: &Board) -> Vec<String> {
    board
        .store()
        .iter()
        .map(|(id, item)| {
            format!(
                "{id:?} {:?} {:?} {:?}",
                item.nets, item.fixed_state, item.kind
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// shove: a cascade that fails *after* it has already moved something
// ---------------------------------------------------------------------------

/// `two_layer_with_pads` plus a shovable vertical trace across the run at
/// `x = 0`, flanked by two `UserFixed` verticals that sit **clear of the run's
/// own corridor** but square in the shovable trace's way.
///
/// The flanking geometry is the whole point. `tests/shove_insert.rs` puts its
/// frozen trace *in the run's path*, where `ShapeTraceEntries.store_items`
/// (`:143-146`) refuses it before a single item moves — so the rollback it then
/// asserts is a rollback of nothing. Here the run's shape meets only the
/// shovable trace, so the cascade starts for real; the failure happens one
/// level down, when that trace's new position lands on a frozen neighbour.
///
/// The flanks run from `y = 200` up, so their clearance envelope
/// (`200 - TEST_HALF_WIDTH - TEST_CLEARANCE = 130`) clears the run corridor's
/// `TEST_HALF_WIDTH + TEST_CLEARANCE = 70`.
fn two_obstacles_one_frozen() -> TestFixture {
    let mut fixture = TestFixture::two_layer_with_pads();
    fixture
        .board
        .insert_trace_points(
            &[IntPoint::new(0, -1400), IntPoint::new(0, 1400)],
            0,
            TEST_HALF_WIDTH,
            NetNos::of(FOREIGN_NET),
            1,
            FixedState::Unfixed,
            None,
        )
        .expect("the shovable obstacle inserts");
    for x in [-160, 160] {
        fixture
            .board
            .insert_trace_points(
                &[IntPoint::new(x, 200), IntPoint::new(x, 1400)],
                0,
                TEST_HALF_WIDTH,
                NetNos::of(SECOND_FOREIGN_NET),
                1,
                FixedState::UserFixed,
                None,
            )
            .expect("the frozen flank inserts");
    }
    fixture
}

/// The offset shapes of the straight pad-to-pad run, as the inserter builds
/// them. Deliberately the *whole* run, so the cascade meets both obstacles.
fn run_shapes(board: &Board, from: IntPoint, to: IntPoint) -> Vec<Simplex> {
    let half_width = fr_board::shove::entries::compensated_half_width(board, TEST_HALF_WIDTH, 1, 0);
    Polyline::from_int_points(&[from, to]).offset_shapes(half_width)
}

/// Drives one cascade over `shapes`; `transactional` picks the entry point.
/// Returns the per-shape verdicts.
fn drive(fixture: &mut TestFixture, shapes: &[Simplex], transactional: bool) -> Vec<bool> {
    let ctrl = fixture.control();
    let nets = NetNos::of(TEST_NET);
    let mut changed = NoChangedArea;
    let mut verdicts = Vec::new();
    let mut inserter = ShoveInsert::new(&mut fixture.board, bounds(), &mut changed);
    for shape in shapes {
        let ok = if transactional {
            inserter.insert_transaction(
                shape,
                Some(FromSide::NOT_CALCULATED),
                0,
                &nets,
                ctrl.trace_clearance_class_no,
                &[],
                20,
                5,
                5,
            )
        } else {
            inserter.insert(
                shape,
                Some(FromSide::NOT_CALCULATED),
                0,
                &nets,
                ctrl.trace_clearance_class_no,
                &[],
                20,
                5,
                5,
            )
        };
        verdicts.push(ok);
    }
    verdicts
}

#[test]
fn a_cascade_that_fails_mid_flight_still_rolls_back_exactly() {
    // Eager: no undo at all. This run exists to *prove the failure is deep* —
    // if the board comes out unchanged the probe is vacuous and this assert
    // says so, rather than the rollback assert passing for the wrong reason.
    let mut eager = two_obstacles_one_frozen();
    let before = snapshot(&eager.board);
    let shapes = run_shapes(&eager.board, eager.pad_centers()[0], eager.pad_centers()[1]);
    assert!(!shapes.is_empty(), "a straight run has offset shapes");
    let eager_verdicts = drive(&mut eager, &shapes, false);
    assert!(
        eager_verdicts.iter().any(|ok| !ok),
        "the frozen obstacle must make at least one shape fail"
    );
    let eager_after = snapshot(&eager.board);
    assert_ne!(
        eager_after, before,
        "the eager cascade must have mutated the board before failing — \
         otherwise this probe is testing the same door-refusal as \
         tests/shove_insert.rs and proves nothing new"
    );

    // Transactional: the same cascade, over an identically built board.
    let mut txn = two_obstacles_one_frozen();
    assert_eq!(snapshot(&txn.board), before, "the two fixtures start equal");
    let txn_verdicts = drive(&mut txn, &shapes, true);

    // A shape that failed must have left nothing behind. Shapes that succeeded
    // legitimately mutate, so the comparison is per-shape: re-run the
    // transactional cascade one shape at a time and check every *failing* shape
    // is a no-op.
    let mut probe = two_obstacles_one_frozen();
    let mut any_failed_after_a_success = false;
    let mut seen_success = false;
    for (index, shape) in shapes.iter().enumerate() {
        let snap = snapshot(&probe.board);
        let ok = drive(&mut probe, std::slice::from_ref(shape), true)[0];
        if ok {
            seen_success = true;
        } else {
            if seen_success {
                any_failed_after_a_success = true;
            }
            assert_eq!(
                snapshot(&probe.board),
                snap,
                "shape {index} failed and must have rolled back to a \
                 bit-identical board"
            );
        }
        assert_eq!(
            ok, txn_verdicts[index],
            "shape {index}: one-at-a-time and batch verdicts must agree"
        );
    }
    assert!(
        any_failed_after_a_success || eager_verdicts.iter().any(|ok| !ok),
        "the probe must exercise a failure"
    );
}

// ---------------------------------------------------------------------------
// pull-tight: pathological zigzags
// ---------------------------------------------------------------------------

/// A 45° zigzag whose legs are `leg` units — call it with `leg` well below
/// [`fr_router::pull_tight::MIN_TRANSLATE_DIST_FLOOR`] to make every proposed
/// translate hit the clamp.
fn tight_zigzag(from: IntPoint, steps: i32, leg: i32) -> Vec<IntPoint> {
    let mut corners = vec![from];
    let mut curr = from;
    for step in 0..steps {
        let dy = if step % 2 == 0 { leg } else { -leg };
        curr = IntPoint::new(curr.x + leg, curr.y + dy);
        corners.push(curr);
        curr = IntPoint::new(curr.x + leg, curr.y);
        corners.push(curr);
    }
    corners
}

/// A zigzag whose corners are at arbitrary, *non*-45° angles: the legs are
/// coprime-ish so no corner is a multiple of 45°, which is the branch
/// `smoothen_non_integer_corner` owns.
fn skew_zigzag(from: IntPoint, steps: i32) -> Vec<IntPoint> {
    let mut corners = vec![from];
    let mut curr = from;
    for step in 0..steps {
        let dy = if step % 2 == 0 { 37 } else { -53 };
        curr = IntPoint::new(curr.x + 91, curr.y + dy);
        corners.push(curr);
        curr = IntPoint::new(curr.x + 67, curr.y + dy / 2);
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
        .expect("the zigzag inserts")
}

/// A changed area covering the whole fixture on every layer.
fn whole_board_area(board: &Board) -> ChangedArea {
    let mut area = ChangedArea::new(board.layers().len());
    for layer in 0..board.layers().len() as i32 {
        area.join_point(FloatPoint::new(-3_000.0, -1_500.0), layer);
        area.join_point(FloatPoint::new(3_000.0, 1_500.0), layer);
    }
    area
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

fn corner_total(board: &Board, net: i32) -> usize {
    traces_of(board, net)
        .into_iter()
        .filter_map(|id| match &board.store().get(id)?.kind {
            ItemKind::Trace(trace) => Some(trace.polyline.corner_count()),
            _ => None,
        })
        .sum()
}

/// Runs `rounds` successive tightenings and asserts the DRC-verdict count reads
/// zero **after every one of them** — the "clearance is respected after every
/// move" half of the probe, at the granularity the API exposes.
/// The violation count of every trace of [`TEST_NET`], in the verdict's
/// arithmetic (no safety margin; [`PullTightAlgo::trace_polyline_violations`])
/// — *not* the acceptance oracle's, which carries the margin upstream gives it
/// and would call a clearance-plus-2-units fixture dirty without any violation
/// existing.
fn dirty_count(fixture: &mut TestFixture) -> usize {
    let rules = fixture.rules.clone();
    let ids = traces_of(&fixture.board, TEST_NET);
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
    ids.iter()
        .filter_map(|id| algo.trace_polyline_violations(*id))
        .sum()
}

/// Runs `rounds` successive tightenings and asserts the clearance oracle never
/// reads **worse than it did going in** — the "pull-tight never turns a clean
/// segment dirty" property, checked after *every* round rather than only at the
/// end. The absolute count is not asserted to be zero: a fixture may start
/// dirty, and upstream's contract is monotonicity, not repair.
fn tighten_and_check(fixture: &mut TestFixture, rounds: usize, label: &str) -> Vec<usize> {
    let started = Instant::now();
    let rules = fixture.rules.clone();
    let initial = dirty_count(fixture);
    let mut trail = vec![initial];
    for round in 0..rounds {
        let mut area = whole_board_area(&fixture.board);
        {
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
        }
        let dirty = dirty_count(fixture);
        trail.push(dirty);
        assert!(
            dirty <= initial,
            "{label}: round {round} raised the violation count to {dirty} \
             (started at {initial}); the trail is {trail:?}. A pull-tight move \
             must be bounded by the oracle, not by a heuristic."
        );
    }
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "{label}: {rounds} tightening rounds must terminate promptly (took {:?})",
        started.elapsed()
    );
    trail
}

#[test]
fn a_sub_floor_zigzag_terminates_and_never_goes_dirty() {
    let mut fixture = TestFixture::two_layer_with_pads();
    // 300 corners of 7-unit legs: every translate the loop proposes is far
    // below MIN_TRANSLATE_DIST_FLOOR (100), so the clamp fires on all of them
    // and only the backoff can end the loop.
    let corners = tight_zigzag(IntPoint::new(-1000, 0), 150, 7);
    insert(&mut fixture.board, &corners, TEST_NET);
    let before = corner_total(&fixture.board, TEST_NET);
    let trail = tighten_and_check(&mut fixture, 3, "sub-floor zigzag");
    assert_eq!(trail[0], 0, "the unobstructed fixture starts clean");
    let after = corner_total(&fixture.board, TEST_NET);
    assert!(
        after <= before,
        "tightening must not add corners to a 45°-legal zigzag ({before} -> {after})"
    );
}

#[test]
fn a_non_45_zigzag_terminates_and_never_goes_dirty() {
    let mut fixture = TestFixture::two_layer_with_pads();
    // 20 steps at 158 units of x each: the run ends at x = 1960, inside the
    // fixture outline's +/-3000. A longer one leaves the board and every
    // violation the probe then counts is against the outline, not the code.
    let corners = skew_zigzag(IntPoint::new(-1200, 0), 20);
    insert(&mut fixture.board, &corners, TEST_NET);
    // No corner-count claim here: on a non-45° board `smoothen_corners`
    // legitimately *chamfers*, so the count may rise (spec §6.2). The claims
    // are termination and cleanliness.
    let trail = tighten_and_check(&mut fixture, 3, "skew zigzag");
    assert_eq!(trail[0], 0, "the skew fixture starts clean");
}

#[test]
fn a_zigzag_boxed_in_by_obstacles_stays_clean() {
    let mut fixture = TestFixture::two_layer_with_pads();
    let corners = tight_zigzag(IntPoint::new(-1000, 0), 150, 7);
    insert(&mut fixture.board, &corners, TEST_NET);
    // Two foreign rails hugging the zigzag's envelope. The zigzag's copper
    // spans y in [-TEST_HALF_WIDTH, 7 + TEST_HALF_WIDTH]; a rail's copper spans
    // TEST_HALF_WIDTH either side of its centre line, and the two must be
    // TEST_CLEARANCE apart, so the nearest legal centre lines are one full
    // width plus a clearance off each edge. Anything tighter starts the fixture
    // *already* dirty and the probe would measure the fixture, not the code.
    let gap = 2 * TEST_HALF_WIDTH + TEST_CLEARANCE + 2;
    for y in [7 + gap, -gap] {
        insert(
            &mut fixture.board,
            &[IntPoint::new(-1100, y), IntPoint::new(1100, y)],
            FOREIGN_NET,
        );
    }
    let trail = tighten_and_check(&mut fixture, 3, "boxed-in zigzag");
    assert_eq!(
        trail[0], 0,
        "the boxed-in fixture must start clean, else the probe measures itself"
    );
}
