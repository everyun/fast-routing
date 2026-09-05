//! The shove stack at the seam: `insert_forced_trace_polyline` with
//! `ShoveTraceAlgo` behind it (`docs/upstream-study/shove.md` §5, §9, §11).
//!
//! Four properties, all of which the M2a stubs could not have:
//!
//! 1. a run that a foreign trace blocks now goes in by **pushing that trace
//!    aside**, and the pushed net stays connected;
//! 2. the shoved board **re-checks clean** — the inserted geometry meets no
//!    obstacle it should not (the acceptance property of this wave);
//! 3. `insert` is a **transaction**: a cascade that fails half way leaves the
//!    board bit-identical, which is `shove.md` §15's "make insert transactional
//!    instead of *may damage the board*";
//! 4. that transaction is equivalent to upstream's eager
//!    insert-plus-external-undo — the M2b gate's equivalence property, run as
//!    `insert` under a manual mark versus `insert_transaction`.

use fr_board::board::Board;
use fr_board::item::{FixedState, ItemId, ItemKind};
use fr_board::net_nos::NetNos;
use fr_board::shove::entries::compensated_half_width;
use fr_board::shove::{FromSide, NoChangedArea, ShoveCheck, ShoveInsert};
use fr_geometry::planar::{IntBox, IntPoint, Point, Polyline, Simplex};
use fr_router::insert_forced::ForcedInserter;
use fr_router::test_support::{TestFixture, TEST_HALF_WIDTH, TEST_NET};

fn bounds() -> IntBox {
    IntBox::new(-4000, -2500, 4000, 2500)
}
const FOREIGN_NET: i32 = 2;

/// Every item as a comparable string, for the byte-for-byte rollback assertion.
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

fn traces_of(board: &Board, net_no: i32) -> Vec<ItemId> {
    board
        .store()
        .iter()
        .filter(|(_, item)| matches!(item.kind, ItemKind::Trace(_)) && item.contains_net(net_no))
        .map(|(id, _)| id)
        .collect()
}

/// The end-to-end extent of a net's copper on `layer`: the two corners furthest
/// apart. A shove that broke the net apart loses the span.
fn net_is_one_component(board: &mut Board, net_no: i32) -> bool {
    board.net_component_count(net_no) <= 1
}

/// The offset shapes of a straight run, as the inserter builds them.
fn segment_shapes(board: &Board, from: IntPoint, to: IntPoint, layer: i32) -> Vec<Simplex> {
    let polyline = Polyline::from_int_points(&[from, to]);
    let half_width = compensated_half_width(board, TEST_HALF_WIDTH, 1, layer);
    polyline.offset_shapes(half_width)
}

// ---------------------------------------------------------------------------
// 1 + 2. the shove happens, and the result re-checks clean
// ---------------------------------------------------------------------------

#[test]
fn a_blocking_foreign_trace_is_pushed_aside_and_stays_connected() {
    let mut fixture = TestFixture::with_foreign_trace();
    let ctrl = fixture.control();
    let centers = fixture.pad_centers();
    let obstacle = fixture.trace_id();
    let obstacle_corners = {
        let item = fixture
            .board
            .store()
            .get(obstacle)
            .expect("the fixture trace");
        let ItemKind::Trace(trace) = &item.kind else {
            panic!("the fixture trace is a trace");
        };
        (trace.first_corner(), trace.last_corner())
    };

    let mut inserter = ForcedInserter::new(&mut fixture.board, &fixture.rules, &ctrl, bounds());
    let polyline = Polyline::from_int_points(&[centers[1], centers[0]]);
    let ok_point = inserter
        .insert_forced_trace_polyline(&polyline, TEST_HALF_WIDTH, 0, i32::MAX)
        .expect("the shove must not report a damaged board");
    assert_eq!(
        ok_point,
        Point::Int(centers[0]),
        "the whole run goes in once the obstacle can be shoved"
    );

    // The obstacle net is still one connected component: the shove cut it and
    // replaced the middle with substitute pieces, it did not delete it.
    assert!(
        !traces_of(&fixture.board, FOREIGN_NET).is_empty(),
        "the shoved net still has copper"
    );
    assert!(
        net_is_one_component(&mut fixture.board, FOREIGN_NET),
        "the shove must not break the net it pushed aside"
    );

    // ... and it still spans what it spanned before.
    let (first, last) = obstacle_corners;
    for corner in [first, last] {
        let mut picked = Vec::new();
        fixture.board.pick_items(&corner, Some(0), &mut picked);
        assert!(
            picked.iter().any(|id| fixture
                .board
                .store()
                .get(*id)
                .is_some_and(|item| item.contains_net(FOREIGN_NET))),
            "the shoved net lost its end at {corner:?}"
        );
    }
}

#[test]
fn the_inserted_shoved_geometry_re_checks_clean() {
    let mut fixture = TestFixture::with_foreign_trace();
    let ctrl = fixture.control();
    let centers = fixture.pad_centers();

    {
        let mut inserter = ForcedInserter::new(&mut fixture.board, &fixture.rules, &ctrl, bounds());
        let polyline = Polyline::from_int_points(&[centers[1], centers[0]]);
        inserter
            .insert_forced_trace_polyline(&polyline, TEST_HALF_WIDTH, 0, i32::MAX)
            .expect("no damage");
    }

    // Every tile shape of every routed-net trace must now be free of foreign
    // copper at the required clearance — the acceptance property: *the
    // inserted-shoved geometry re-checks clean*.
    let nets = NetNos::of(TEST_NET);
    let mut offenders = Vec::new();
    for id in traces_of(&fixture.board, TEST_NET) {
        let Some(slot) = fixture.board.store().slot_of(id) else {
            continue;
        };
        let shapes: Vec<(Simplex, i32)> = fixture
            .board
            .default_tree()
            .item_shapes(slot, id)
            .iter()
            .filter_map(|tile| {
                tile.shape
                    .as_ref()
                    .map(|s| (fr_board::shove::geom::tile_to_simplex(s), tile.layer))
            })
            .collect();
        for (shape, layer) in shapes {
            let mut hits = Vec::new();
            fr_board::shove::geom::overlapping_items_with_clearance(
                &fixture.board,
                &shape,
                layer,
                nets.as_slice(),
                ctrl.trace_clearance_class_no,
                &mut hits,
            );
            for hit in hits {
                if fixture
                    .board
                    .store()
                    .get(hit)
                    .is_some_and(|item| matches!(item.kind, ItemKind::Trace(_)))
                {
                    offenders.push((id, hit));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "shoved geometry violates a clearance: {offenders:?}"
    );
}

// ---------------------------------------------------------------------------
// 3 + 4. the transaction
// ---------------------------------------------------------------------------

/// Makes the fixture's foreign trace `USER_FIXED`, i.e. `isShoveFixed()`, which
/// `ShapeTraceEntries.store_items` (`:143-146`) refuses outright.
fn freeze_the_obstacle(fixture: &mut TestFixture) {
    let trace = fixture.trace_id();
    fixture.board.remove_item(trace);
    fixture
        .board
        .insert_trace_points(
            &[IntPoint::new(0, -1500), IntPoint::new(0, 1500)],
            0,
            TEST_HALF_WIDTH,
            NetNos::of(FOREIGN_NET),
            1,
            FixedState::UserFixed,
            None,
        )
        .expect("the frozen obstacle inserts");
}

#[test]
fn a_failed_shove_cascade_leaves_the_board_bit_identical() {
    let mut fixture = TestFixture::with_foreign_trace();
    freeze_the_obstacle(&mut fixture);
    let ctrl = fixture.control();
    let centers = fixture.pad_centers();
    let shapes = segment_shapes(&fixture.board, centers[1], centers[0], 0);
    assert!(!shapes.is_empty(), "a straight run has offset shapes");

    let before = snapshot(&fixture.board);
    let items_before = fixture.board.store().len();

    let nets = NetNos::of(TEST_NET);
    let mut changed = NoChangedArea;
    let mut any_failed = false;
    {
        let mut inserter = ShoveInsert::new(&mut fixture.board, bounds(), &mut changed);
        for shape in &shapes {
            let from_side = FromSide::NOT_CALCULATED;
            if !inserter.insert_transaction(
                shape,
                Some(from_side),
                0,
                &nets,
                ctrl.trace_clearance_class_no,
                &[],
                20,
                5,
                5,
            ) {
                any_failed = true;
            }
        }
    }
    assert!(
        any_failed,
        "a user-fixed obstacle across the run must fail at least one shape"
    );
    assert_eq!(fixture.board.store().len(), items_before);
    assert_eq!(
        snapshot(&fixture.board),
        before,
        "a failed cascade must roll back exactly"
    );
}

#[test]
fn the_transaction_equals_the_eager_insert_plus_undo() {
    // The M2b gate's equivalence property. Not "check == insert" — upstream
    // specifies `check` as deliberately optimistic — but
    // "`insert_transaction` == `insert` wrapped in a caller-owned undo".
    for freeze in [false, true] {
        let mut eager = TestFixture::with_foreign_trace();
        let mut transactional = TestFixture::with_foreign_trace();
        if freeze {
            freeze_the_obstacle(&mut eager);
            freeze_the_obstacle(&mut transactional);
        }
        let ctrl = eager.control();
        let centers = eager.pad_centers();
        let shapes = segment_shapes(&eager.board, centers[1], centers[0], 0);
        let nets = NetNos::of(TEST_NET);

        let mut eager_results = Vec::new();
        let mut txn_results = Vec::new();

        {
            let mut changed = NoChangedArea;
            let mut inserter = ShoveInsert::new(&mut eager.board, bounds(), &mut changed);
            for shape in &shapes {
                // Upstream's protocol: take a snapshot, insert eagerly, undo on
                // failure.
                let mark = inserter.board.begin();
                let ok = inserter.insert(
                    shape,
                    Some(FromSide::NOT_CALCULATED),
                    0,
                    &nets,
                    ctrl.trace_clearance_class_no,
                    &[],
                    20,
                    5,
                    5,
                );
                if ok {
                    inserter.board.commit(mark);
                } else {
                    inserter.board.rollback_to(mark);
                }
                eager_results.push(ok);
            }
        }
        {
            let mut changed = NoChangedArea;
            let mut inserter = ShoveInsert::new(&mut transactional.board, bounds(), &mut changed);
            for shape in &shapes {
                txn_results.push(inserter.insert_transaction(
                    shape,
                    Some(FromSide::NOT_CALCULATED),
                    0,
                    &nets,
                    ctrl.trace_clearance_class_no,
                    &[],
                    20,
                    5,
                    5,
                ));
            }
        }

        assert_eq!(
            eager_results, txn_results,
            "same verdicts (freeze={freeze})"
        );
        assert_eq!(
            snapshot(&eager.board),
            snapshot(&transactional.board),
            "same board (freeze={freeze})"
        );
    }
}

#[test]
fn the_check_admits_every_shape_the_transaction_commits() {
    // One direction of the check/insert contract that *does* hold: whatever
    // `insert` commits, `check` must have been willing to accept. (The other
    // direction does not — upstream's `check` is optimistic, which is why the
    // gate is the transaction equivalence above and not "check == insert".)
    let mut fixture = TestFixture::with_foreign_trace();
    let ctrl = fixture.control();
    let centers = fixture.pad_centers();
    let shapes = segment_shapes(&fixture.board, centers[1], centers[0], 0);
    let nets = NetNos::of(TEST_NET);

    for shape in &shapes {
        let checked = {
            let mut checker = ShoveCheck::new(&fixture.board, bounds());
            checker.check(
                shape,
                Some(FromSide::NOT_CALCULATED),
                None,
                0,
                &nets,
                ctrl.trace_clearance_class_no,
                20,
                5,
                5,
            )
        };
        let mut changed = NoChangedArea;
        let inserted = {
            let mut inserter = ShoveInsert::new(&mut fixture.board, bounds(), &mut changed);
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
        };
        assert!(
            !inserted || checked,
            "insert committed a shape the check rejected"
        );
    }
}
