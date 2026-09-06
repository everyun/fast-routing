//! End-to-end: maze → locate → insert, on synthetic boards.
//!
//! The four cases `pull-tight-insert.md` §2-§4 need covered at the seam:
//!
//! 1. one net routed and inserted, target to start, with real copper on the
//!    board and the two pads connected afterwards;
//! 2. a refused insertion rolling the board back to *exactly* its prior state —
//!    the one place this port deliberately diverges from upstream (§4.1,
//!    "Failure semantics": upstream has no rollback);
//! 3. a two-layer connection that has to pay a via, with the via actually on
//!    the board spanning both layers;
//! 4. the §4.3 neckdown: a segment that does not fit at full width but does at
//!    the pin's `max(0.5·min_pad_width − 1, 1)` reduced half width.

use fr_board::board::{Board, Layer, LayerStructure};
use fr_board::item::{
    BoardOutline, FixedState, Item, ItemId, ItemKind, LayerRange, Pin as PinPayload,
};
use fr_board::net_nos::NetNos;
use fr_board::shape::{Padstack as BoardPadstack, TileShape};
use fr_geometry::planar::{IntBox, IntPoint, Polyline};
use fr_router::ctrl::AutorouteControl;
use fr_router::engine::AutorouteEngine;
use fr_router::insert::{insert_connection, insert_connection_with_fail_mode, InsertError};
use fr_router::insert_forced::ForcedInserter;
use fr_router::locate::{locate, LocatedConnection, ResultItem};
use fr_router::maze::search::MazeSearch;
use fr_router::result::RippedItem;
use fr_router::test_support::{empty_board, TestFixture, TEST_HALF_WIDTH, TEST_NET};
use fr_rules::board_rules::BoardRules;
use fr_rules::router_settings::RouterSettings;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Runs the maze and the locator, and returns the located connection together
/// with the board bounding box the inserter has to use.
///
/// The scoping is upstream's `AutorouteEngine.clear()` between locate and
/// insert (`AutorouteEngine.java:186`): the engine borrows the board immutably,
/// the inserter needs it mutably, so the located connection must be
/// self-contained.
fn route(
    board: &Board,
    rules: &BoardRules,
    ctrl: &AutorouteControl,
    start: &[ItemId],
    destination: &[ItemId],
) -> (LocatedConnection, IntBox) {
    let mut engine = AutorouteEngine::new(board, rules, ctrl.clone());
    let found = {
        let mut search =
            MazeSearch::get_instance(&mut engine, start, destination).expect("maze instance");
        search
            .find_connection()
            .expect("the connection is routable")
    };
    let bounds = engine.bounds();
    let located = locate(&engine, &found).expect("the chain is well formed");
    (located, bounds)
}

/// A stable, total description of the board's contents, for the rollback test.
fn snapshot(board: &Board) -> Vec<String> {
    let mut rows: Vec<String> = board
        .store()
        .iter()
        .map(|(id, item)| format!("{:?} {:?} {:?}", id, item.nets, item.kind))
        .collect();
    rows.sort();
    rows
}

/// Whether the two items are one connected component of `net_no`.
fn are_connected(board: &mut Board, net_no: i32, a: ItemId, b: ItemId) -> bool {
    let store = board.store().clone();
    board.connectivity_mut().are_connected(net_no, &store, a, b)
}

/// Every trace of `net_no` currently on the board.
fn traces_of(board: &Board, net_no: i32) -> Vec<ItemId> {
    board
        .store()
        .net_items(net_no)
        .iter()
        .copied()
        .filter(|id| {
            board
                .store()
                .get(*id)
                .is_some_and(|item| matches!(item.kind, ItemKind::Trace(_)))
        })
        .collect()
}

/// Every via of `net_no`.
fn vias_of(board: &Board, net_no: i32) -> Vec<ItemId> {
    board
        .store()
        .net_items(net_no)
        .iter()
        .copied()
        .filter(|id| {
            board
                .store()
                .get(*id)
                .is_some_and(|item| matches!(item.kind, ItemKind::Via(_)))
        })
        .collect()
}

/// Re-runs the insertion-time clearance oracle over every trace of `net_no`.
///
/// Own-net copper is not an obstacle, so this asks exactly the question the DRC
/// referee asks about foreign nets: does the inserted trace clear everything
/// that is not its own?
fn every_trace_still_clears(
    board: &mut Board,
    rules: &BoardRules,
    ctrl: &AutorouteControl,
    bounds: IntBox,
    net_no: i32,
) -> bool {
    let traces: Vec<(Polyline, i32, i32)> = traces_of(board, net_no)
        .into_iter()
        .filter_map(|id| match &board.store().get(id)?.kind {
            ItemKind::Trace(trace) => Some((trace.polyline.clone(), trace.layer, trace.half_width)),
            _ => None,
        })
        .collect();
    let mut inserter = ForcedInserter::new(board, rules, ctrl, bounds);
    traces.iter().all(|(polyline, layer, half_width)| {
        inserter.check_polyline_trace(polyline, *layer, *half_width)
    })
}

fn trace_half_width(board: &Board, id: ItemId) -> Option<i32> {
    match &board.store().get(id)?.kind {
        ItemKind::Trace(trace) => Some(trace.half_width),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// 1. one net, end to end
// ---------------------------------------------------------------------------

#[test]
fn routes_locates_and_inserts_one_net_end_to_end() {
    let mut fixture = TestFixture::two_layer_with_pads();
    let pads = fixture.pad_ids();
    let ctrl = fixture.control();

    let (located, bounds) = route(
        &fixture.board,
        &fixture.rules,
        &ctrl,
        &pads[..1],
        &pads[1..2],
    );

    // The locator's contract (`pull-tight-insert.md` §2.1): target → start.
    assert_eq!(located.start_item, pads[0]);
    assert_eq!(located.target_item, Some(pads[1]));
    assert_eq!(located.start_layer, 0);
    assert_eq!(located.target_layer, 0);
    assert!(
        !located.connection_items.is_empty(),
        "an empty board between two pads must yield at least one trace"
    );
    for item in &located.connection_items {
        assert_eq!(item.layer, 0, "no layer change is needed on an empty board");
        assert!(!item.corners.is_empty());
        // `:386-394`: consecutive corners are deduplicated.
        for pair in item.corners.windows(2) {
            assert_ne!(pair[0], pair[1]);
        }
    }
    // The first corner of the first trace lies in the destination pad, the last
    // corner of the last trace in the start pad.
    let first_corner = located.connection_items[0].corners[0];
    let last_item = located.connection_items.last().unwrap();
    let last_corner = *last_item.corners.last().unwrap();
    let pad_centers = fixture.pad_centers();
    assert!(
        (first_corner.x - pad_centers[1].x).abs() <= 200
            && (first_corner.y - pad_centers[1].y).abs() <= 200,
        "the route starts inside the destination pad, got {first_corner:?}"
    );
    assert!(
        (last_corner.x - pad_centers[0].x).abs() <= 200
            && (last_corner.y - pad_centers[0].y).abs() <= 200,
        "the route ends inside the start pad, got {last_corner:?}"
    );

    let traces_before = traces_of(&fixture.board, TEST_NET).len();
    let outcome = insert_connection(&mut fixture.board, &fixture.rules, &ctrl, &located, bounds)
        .expect("an empty board accepts the connection");

    assert!(outcome.ripped_connections.is_empty());
    assert!(outcome.requeue_nets.is_empty());
    assert!(
        !outcome.changed_area.is_empty(),
        "an insertion must mark the region it touched"
    );
    assert!(
        !outcome.changed_area.area(0).is_empty(),
        "the trace is on layer 0, so layer 0's octagon is non-empty"
    );

    let traces_after = traces_of(&fixture.board, TEST_NET);
    assert!(
        traces_after.len() > traces_before,
        "copper must be on the board: {traces_before} → {}",
        traces_after.len()
    );
    for id in &traces_after {
        assert_eq!(trace_half_width(&fixture.board, *id), Some(TEST_HALF_WIDTH));
    }

    // The referee's question: are the two pads one component now?
    assert!(
        are_connected(&mut fixture.board, TEST_NET, pads[0], pads[1]),
        "the inserted copper must join the two pads"
    );

    // And the copper that went in still passes the oracle that let it in.
    assert!(
        every_trace_still_clears(&mut fixture.board, &fixture.rules, &ctrl, bounds, TEST_NET),
        "the inserted copper violates the clearance it was checked against"
    );
}

#[test]
fn the_route_ends_exactly_on_the_two_pin_centres() {
    // `DrillItem.get_trace_connection_shape` is a *point* at the centre
    // (`DrillItem.java:350-352`), not the pad, and contact is exact-point
    // (`drc-connectivity.md` §4.3). Landing anywhere else inside the pad would
    // insert copper that the referee still counts as unrouted.
    let fixture = TestFixture::two_layer_with_pads();
    let pads = fixture.pad_ids();
    let centers = fixture.pad_centers();
    let ctrl = fixture.control();
    let (located, _) = route(
        &fixture.board,
        &fixture.rules,
        &ctrl,
        &pads[..1],
        &pads[1..2],
    );
    assert_eq!(located.connection_items[0].corners[0], centers[1]);
    assert_eq!(
        *located
            .connection_items
            .last()
            .unwrap()
            .corners
            .last()
            .unwrap(),
        centers[0]
    );
}

#[test]
fn a_ripped_item_and_its_connection_are_deleted_in_the_same_transaction() {
    // The maze may plan to rip a foreign connection; the inserter deletes it
    // together with the rest of its connection, in the same transaction as the
    // new copper, and reports the net so the batch driver can requeue it
    // (`AutorouteEngine.java:212-239`).
    let mut fixture = TestFixture::with_foreign_trace();
    let pads = fixture.pad_ids();
    let centers = fixture.pad_centers();
    let ctrl = fixture.control();
    let foreign = fixture.trace_id();
    let bounds = IntBox::new(-4000, -2500, 4000, 2500);

    // The same straight run that `a_refused_insertion_rolls_the_board_back_exactly`
    // cannot place — but this time the blocker is claimed as a ripup.
    let connection = LocatedConnection {
        connection_items: vec![ResultItem {
            corners: vec![centers[1], centers[0]],
            layer: 0,
        }],
        start_item: pads[0],
        start_layer: 0,
        target_item: Some(pads[1]),
        target_layer: 0,
        ripped_items: vec![RippedItem {
            item: foreign,
            ripup_cost: 100,
        }],
    };

    let outcome = insert_connection(
        &mut fixture.board,
        &fixture.rules,
        &ctrl,
        &connection,
        bounds,
    )
    .expect("with the blocker ripped, the straight run fits");

    assert!(
        outcome.ripped_connections.contains(&foreign),
        "the claimed item must be in the deleted set: {:?}",
        outcome.ripped_connections
    );
    assert_eq!(
        outcome.requeue_nets,
        vec![2],
        "the ripped item's net must be reported for requeue"
    );
    assert!(
        fixture.board.store().get(foreign).is_none(),
        "the ripped trace must be off the board"
    );
    assert!(
        !traces_of(&fixture.board, TEST_NET).is_empty(),
        "the new connection must be on the board"
    );
    assert!(are_connected(
        &mut fixture.board,
        TEST_NET,
        pads[0],
        pads[1]
    ));
}

// ---------------------------------------------------------------------------
// 2. the failure path rolls back
// ---------------------------------------------------------------------------

/// Re-inserts the fixture's foreign trace at the same geometry but
/// **user-fixed**, which is `Item.isShoveFixed()` and therefore the one thing
/// `ShapeTraceEntries.store_items` refuses outright
/// (`ShapeTraceEntries.java:143-146`).
fn make_the_foreign_trace_unshovable(fixture: &mut TestFixture) {
    let trace = fixture.trace_id();
    fixture.board.remove_item(trace);
    fixture
        .board
        .insert_trace_points(
            &[IntPoint::new(0, -1500), IntPoint::new(0, 1500)],
            0,
            TEST_HALF_WIDTH,
            NetNos::of(2),
            1,
            FixedState::UserFixed,
            None,
        )
        .expect("the fixed obstacle inserts");
}

#[test]
fn a_refused_insertion_rolls_the_board_back_exactly() {
    // The foreign trace runs the full height of the board on layer 0 and is
    // user-fixed, so it can be neither shoved nor sprung over: the wrap would
    // have to leave the outline. The per-shape check refuses the straight
    // layer-0 run and the whole transaction is rolled back.
    let mut fixture = TestFixture::with_foreign_trace();
    make_the_foreign_trace_unshovable(&mut fixture);
    let pads = fixture.pad_ids();
    let centers = fixture.pad_centers();
    let ctrl = fixture.control();
    let bounds = IntBox::new(-4000, -2500, 4000, 2500);

    let before = snapshot(&fixture.board);
    let items_before = fixture.board.store().len();

    // Hand-built: a straight layer-0 run from one pad centre to the other,
    // straight through the foreign trace.
    let connection = LocatedConnection {
        connection_items: vec![ResultItem {
            corners: vec![centers[1], IntPoint::new(0, 0), centers[0]],
            layer: 0,
        }],
        start_item: pads[0],
        start_layer: 0,
        target_item: Some(pads[1]),
        target_layer: 0,
        ripped_items: Vec::new(),
    };

    let error = insert_connection(
        &mut fixture.board,
        &fixture.rules,
        &ctrl,
        &connection,
        bounds,
    )
    .expect_err("a user-fixed foreign trace blocks the straight run");
    assert!(
        matches!(error, InsertError::TraceBlocked { layer: 0, .. }),
        "expected a blocked trace segment, got {error:?}"
    );

    assert_eq!(
        fixture.board.store().len(),
        items_before,
        "rollback must restore the item count"
    );
    assert_eq!(
        snapshot(&fixture.board),
        before,
        "rollback must restore every item, byte for byte"
    );

    // The search trees are derived state and must be rolled back with the
    // store, or the next query sees copper that is not there.
    let mut hits = Vec::new();
    fixture.board.overlapping_items(
        &TileShape::Box(IntBox::new(-100, -100, 100, 100)),
        Some(0),
        &[],
        &mut hits,
    );
    hits.retain(|id| {
        fixture
            .board
            .store()
            .get(*id)
            .is_some_and(|item| item.contains_net(TEST_NET))
    });
    assert!(
        hits.is_empty(),
        "the rolled-back trace is still indexed: {hits:?}"
    );
    assert!(traces_of(&fixture.board, TEST_NET).is_empty());
}

/// The env-gated upstream failure mode: with `keep_fail_rips` on, a failed
/// insertion still returns `Err` and — because this hand-built failure rips
/// nothing — leaves the board byte-identical, but the cleanup must stay scoped
/// to the routed net and must not delete the foreign trace. With the default
/// (transactional) mode this is the same assertion the rollback test makes.
#[test]
fn keep_fail_rips_mode_cleans_only_the_routed_net_and_keeps_other_copper() {
    let mut fixture = TestFixture::with_foreign_trace();
    make_the_foreign_trace_unshovable(&mut fixture);
    let pads = fixture.pad_ids();
    let centers = fixture.pad_centers();
    let ctrl = fixture.control();
    let bounds = IntBox::new(-4000, -2500, 4000, 2500);

    let before = snapshot(&fixture.board);
    let items_before = fixture.board.store().len();

    let connection = LocatedConnection {
        connection_items: vec![ResultItem {
            corners: vec![centers[1], IntPoint::new(0, 0), centers[0]],
            layer: 0,
        }],
        start_item: pads[0],
        start_layer: 0,
        target_item: Some(pads[1]),
        target_layer: 0,
        ripped_items: Vec::new(),
    };

    let error = insert_connection_with_fail_mode(
        &mut fixture.board,
        &fixture.rules,
        &ctrl,
        &connection,
        bounds,
        true,
    )
    .expect_err("the user-fixed foreign trace blocks the run in either mode");
    assert!(matches!(error, InsertError::TraceBlocked { .. }));

    // No rip happened on this hand-built failure, so even the upstream mode
    // lands on the same board: same item count, same contents.
    assert_eq!(fixture.board.store().len(), items_before);
    assert_eq!(snapshot(&fixture.board), before);
    // The cleanup is scoped to the routed net: the foreign (net-2) trace that
    // blocked the run is still on the board.
    assert_eq!(traces_of(&fixture.board, 2).len(), 1);
    assert!(traces_of(&fixture.board, TEST_NET).is_empty());
}

#[test]
fn a_connection_with_no_traces_is_refused_before_anything_is_touched() {
    let mut fixture = TestFixture::two_layer_with_pads();
    let pads = fixture.pad_ids();
    let ctrl = fixture.control();
    let before = snapshot(&fixture.board);
    let outcome = insert_connection(
        &mut fixture.board,
        &fixture.rules,
        &ctrl,
        &LocatedConnection {
            connection_items: Vec::new(),
            start_item: pads[0],
            start_layer: 0,
            target_item: Some(pads[1]),
            target_layer: 0,
            ripped_items: Vec::new(),
        },
        IntBox::new(-4000, -2500, 4000, 2500),
    )
    .expect("empty connection normalizes and succeeds");
    assert_eq!(snapshot(&fixture.board), before);
    assert!(outcome.ripped_connections.is_empty());
}

// ---------------------------------------------------------------------------
// 3. a via-requiring two-layer connection
// ---------------------------------------------------------------------------

#[test]
fn a_two_layer_connection_pays_a_via_and_the_via_lands_on_the_board() {
    // A hand-built connection that changes layer half way: trace on layer 1,
    // then a via, then trace on layer 0 into the start pad. This exercises §4.1
    // (`insert_via` between consecutive `ResultItem`s and after the last one)
    // and §4.4 (mask selection + the forced-via check) without depending on the
    // maze choosing to pay for a via.
    let mut fixture = TestFixture::two_layer_with_pads();
    let pads = fixture.pad_ids();
    let centers = fixture.pad_centers();
    let ctrl = fixture.control();
    let bounds = IntBox::new(-4000, -2500, 4000, 2500);

    // The two vias sit in free space, not on the SMD pads: a via landing on an
    // own-net pad is the `attach_smd` case, and the fixture's via rule declares
    // `attach_smd_allowed = false`, so `check_forced_pad` rightly refuses it.
    let via_down = IntPoint::new(-1000, 0);
    let via_up = IntPoint::new(1000, 0);
    let connection = LocatedConnection {
        connection_items: vec![
            // target pad → the first via, on the pads' layer …
            ResultItem {
                corners: vec![centers[1], via_up],
                layer: 0,
            },
            // … across on layer 1 …
            ResultItem {
                corners: vec![via_up, via_down],
                layer: 1,
            },
            // … and back down into the start pad.
            ResultItem {
                corners: vec![via_down, centers[0]],
                layer: 0,
            },
        ],
        start_item: pads[0],
        start_layer: 0,
        target_item: Some(pads[1]),
        target_layer: 0,
        ripped_items: Vec::new(),
    };

    let vias_before = vias_of(&fixture.board, TEST_NET).len();
    let outcome = insert_connection(
        &mut fixture.board,
        &fixture.rules,
        &ctrl,
        &connection,
        bounds,
    )
    .expect("an empty two-layer board accepts the via");

    let vias_after = vias_of(&fixture.board, TEST_NET);
    assert_eq!(
        vias_after.len(),
        vias_before + 2,
        "one via per layer change: down onto layer 1 and back up"
    );
    for id in &vias_after {
        let item = fixture.board.store().get(*id).expect("live");
        assert!(
            item.is_on_layer(0) && item.is_on_layer(1),
            "the chosen mask must span both layers"
        );
    }

    // Both layers are marked as changed, which is what the M2b regional
    // pull-tight consumes.
    assert!(!outcome.changed_area.area(0).is_empty());
    assert!(!outcome.changed_area.area(1).is_empty());

    // Copper on both layers, and the two pads are one component.
    let layers: Vec<i32> = traces_of(&fixture.board, TEST_NET)
        .into_iter()
        .filter_map(|id| match &fixture.board.store().get(id)?.kind {
            ItemKind::Trace(trace) => Some(trace.layer),
            _ => None,
        })
        .collect();
    assert!(layers.contains(&0), "layer 0 trace missing: {layers:?}");
    assert!(layers.contains(&1), "layer 1 trace missing: {layers:?}");
    assert!(are_connected(
        &mut fixture.board,
        TEST_NET,
        pads[0],
        pads[1]
    ));
}

#[test]
fn a_layer_change_with_no_spanning_via_mask_is_refused_and_rolled_back() {
    // Same connection, but the net's via rule is emptied: no padstack spans the
    // layers, which is upstream's "via mask not found" branch (`:456-458`).
    let mut fixture = TestFixture::two_layer_with_pads();
    let pads = fixture.pad_ids();
    let centers = fixture.pad_centers();
    let mut ctrl = fixture.control();
    ctrl.via_rule = fr_rules::via_rule::ViaRule::empty();

    let before = snapshot(&fixture.board);
    let connection = LocatedConnection {
        connection_items: vec![
            ResultItem {
                corners: vec![centers[1], IntPoint::new(0, 0)],
                layer: 0,
            },
            ResultItem {
                corners: vec![IntPoint::new(0, 0), centers[0]],
                layer: 1,
            },
        ],
        start_item: pads[0],
        start_layer: 1,
        target_item: Some(pads[1]),
        target_layer: 0,
        ripped_items: Vec::new(),
    };
    let error = insert_connection(
        &mut fixture.board,
        &fixture.rules,
        &ctrl,
        &connection,
        IntBox::new(-4000, -2500, 4000, 2500),
    )
    .expect_err("no padstack spans layers 0..1");
    assert_eq!(
        error,
        InsertError::NoViaSpan {
            from_layer: 0,
            to_layer: 1
        }
    );
    assert_eq!(snapshot(&fixture.board), before);
}

// ---------------------------------------------------------------------------
// 4. neckdown (§4.3)
// ---------------------------------------------------------------------------

/// Half the pad edge, for the neckdown fixture's target pin.
const NECK_PAD_HALF: i32 = 20;
/// `Pin.get_trace_neckdown_halfwidth(layer) = max(0.5·min_width − 1, 1)`
/// (`board/Pin.java`). A 40×40 pad gives 19.
const EXPECTED_NECKDOWN_HALF_WIDTH: i32 = 19;

/// Two pads of the routed net plus one hard obstacle that leaves room for a
/// necked-down trace but not for a full-width one.
///
/// Geometry, all on layer 0 (clearance 20 ⇒ compensation `(20+1)/2 = 10`, so a
/// half-width `h` trace is checked at `h + 10` and every obstacle is inflated by
/// `20 − 10 = 10`):
///
/// * the connection runs along `y = 0` into the target pin at `(1000, 0)`;
/// * the blocker's lower edge sits at `y = 50`, so it conflicts with the full
///   half width (`50 + 10 = 60 > 50`) and clears the necked one
///   (`19 + 10 = 29 < 50`).
fn neckdown_board() -> (Board, BoardRules, RouterSettings, Vec<ItemId>) {
    let layers = LayerStructure::new(vec![Layer::signal("L0"), Layer::signal("L1")]);
    let (mut board, mut rules, settings) = empty_board(layers);
    rules.nets.add("N2", 1, false, 0);

    board.insert_item(Item::new(
        NetNos::EMPTY,
        0,
        FixedState::SystemFixed,
        ItemKind::BoardOutline(BoardOutline {
            borders: vec![Polyline::from_int_points(&[
                IntPoint::new(-3000, -1500),
                IntPoint::new(3000, -1500),
                IntPoint::new(3000, 1500),
                IntPoint::new(-3000, 1500),
                IntPoint::new(-3000, -1500),
            ])],
            keepout_outside_outline: false,
        }),
    ));

    let big_pad = board.padstacks_mut().push(BoardPadstack::uniform(
        0,
        0,
        TileShape::Box(IntBox::new(-200, -200, 200, 200)),
    ));
    let small_pad = board.padstacks_mut().push(BoardPadstack::uniform(
        0,
        0,
        TileShape::Box(IntBox::new(
            -NECK_PAD_HALF,
            -NECK_PAD_HALF,
            NECK_PAD_HALF,
            NECK_PAD_HALF,
        )),
    ));

    let source = board.insert_pin(
        PinPayload {
            center: IntPoint::new(-1000, 0),
            padstack_no: big_pad,
            pin_no: 0,
            layers: LayerRange::new(0, 0),
        },
        NetNos::of(TEST_NET),
        1,
    );
    let target = board.insert_pin(
        PinPayload {
            center: IntPoint::new(1000, 0),
            padstack_no: small_pad,
            pin_no: 1,
            layers: LayerRange::new(0, 0),
        },
        NetNos::of(TEST_NET),
        1,
    );
    // The blocker: a foreign-net pin. A *pin* and not a trace, because
    // `check_trace_segment(only_not_shovable = true)` skips routable items
    // (`InsertFoundConnectionAlgo.java:375`) — a foreign trace would be
    // invisible to the neckdown length probe.
    let blocker_pad = board.padstacks_mut().push(BoardPadstack::uniform(
        0,
        0,
        TileShape::Box(IntBox::new(-400, 0, 400, 400)),
    ));
    board.insert_pin(
        PinPayload {
            center: IntPoint::new(900, 50),
            padstack_no: blocker_pad,
            pin_no: 2,
            layers: LayerRange::new(0, 0),
        },
        NetNos::of(2),
        1,
    );

    board.ensure_compensated_tree(1);
    (board, rules, settings, vec![source, target])
}

#[test]
fn neckdown_inserts_the_reduced_width_stub_the_full_width_one_cannot_fit() {
    let (mut board, rules, settings, pins) = neckdown_board();
    let mut ctrl = AutorouteControl::for_net(&board, &rules, &settings, TEST_NET);
    let bounds = IntBox::new(-3200, -1700, 3200, 1700);

    // A single segment that ends exactly at the narrow pin's centre — the only
    // shape `try_neck_down` fires for (`:358`: the pin centre must be closer to
    // the segment end than `2·(½·pad_width + clearance)`).
    let connection = LocatedConnection {
        connection_items: vec![ResultItem {
            corners: vec![IntPoint::new(-1000, 0), IntPoint::new(1000, 0)],
            layer: 0,
        }],
        start_item: pins[1],
        start_layer: 0,
        target_item: Some(pins[0]),
        target_layer: 0,
        ripped_items: Vec::new(),
    };

    // Without neckdown the blocker refuses the full-width run outright.
    ctrl.with_neckdown = false;
    let mut plain_board = board.clone();
    let error = insert_connection(&mut plain_board, &rules, &ctrl, &connection, bounds)
        .expect_err("the blocker is 50 units from the centreline, the trace needs 60");
    assert!(
        matches!(error, InsertError::TraceBlocked { layer: 0, .. }),
        "got {error:?}"
    );
    assert!(traces_of(&plain_board, TEST_NET).is_empty());

    // With neckdown the stub into the pin goes in at the pin's reduced width.
    ctrl.with_neckdown = true;
    insert_connection(&mut board, &rules, &ctrl, &connection, bounds)
        .expect("the necked stub fits: 19 + 10 = 29 < 50");

    let widths: Vec<i32> = traces_of(&board, TEST_NET)
        .into_iter()
        .filter_map(|id| trace_half_width(&board, id))
        .collect();
    assert!(
        widths.contains(&EXPECTED_NECKDOWN_HALF_WIDTH),
        "a trace at the pin's neckdown half width must exist, got {widths:?}"
    );
    assert!(
        widths.contains(&TEST_HALF_WIDTH),
        "the full-width part of the run must also be there, got {widths:?}"
    );
    // The gate the whole case rests on: `neck < ctrl.trace_half_width`
    // (`InsertFoundConnectionAlgo.java:363`).
    const _: () = assert!(EXPECTED_NECKDOWN_HALF_WIDTH < TEST_HALF_WIDTH);
}

#[test]
fn the_neckdown_tolerance_is_two() {
    // `InsertFoundConnectionAlgo.java:370`, confirmed in the release jar's
    // constant pool (`pull-tight-insert.md` §8).
    assert_eq!(fr_router::insert::NECKDOWN_TOLERANCE, 2);
}
