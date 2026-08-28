//! End-to-end: the real optimizer, driven by the real maze engine, on a small
//! board — and the two promises it must never break.
//!
//! 1. **It never makes the board worse.** Incompletes and clearance violations
//!    after the stage are never above what they were before it. This is a
//!    property of the port's structure, not of the board: every item is
//!    bracketed by a journal transaction and a reroute that did not strictly
//!    improve the board is rolled back byte for byte
//!    (`BatchOptimizer.java:405-411`).
//! 2. **When it claims an improvement, something actually fell.** A pass that
//!    kept `n > 0` items must have strictly lowered the lexicographic key
//!    `(incompletes, vias, weighted trace length)`, and — since incompletes are
//!    already covered by (1) — when incompletes tie, vias or weighted length
//!    strictly fell. A pass that kept nothing must have left the key alone.
//!
//! Promise 2 is asserted under [`LengthMetric::WeightedBoth`], because under
//! the release's own [`LengthMetric::UpstreamMixed`] the statement is *false*
//! upstream: the release compares a weighted `before` with an unweighted
//! `after` (`batch-orchestration.md` conformance row #87), so "improved" does
//! not imply "shorter". The last test pins that difference so the day the
//! default flips, this file says why.

use fr_board::board::{Board, Layer, LayerStructure};
use fr_board::item::{BoardOutline, FixedState, Item, ItemKind, LayerRange, Pin};
use fr_board::net_nos::NetNos;
use fr_board::shape::{Padstack as BoardPadstack, TileShape};
use fr_geometry::planar::{IntBox, IntPoint, Polyline};
use fr_router::batch::BatchRouter;
use fr_router::optimizer::{
    stats, BatchOptimizer, BoardKey, LengthMetric, OptimizerOutcome, DEFAULT_BOARD_UNITS_PER_MM,
};
use fr_router::test_support::{empty_board, OUTLINE_HALF_EXTENT, TEST_HALF_WIDTH};
use fr_rules::board_rules::BoardRules;
use fr_rules::router_settings::RouterSettings;

/// A two-layer board with two point-to-point nets, each wired the long way
/// round so that there is something for the optimizer to win back.
struct SmallBoard {
    board: Board,
    rules: BoardRules,
    settings: RouterSettings,
}

fn small_board(detoured: bool) -> SmallBoard {
    let layers = LayerStructure::new(vec![Layer::signal("L0"), Layer::signal("L1")]);
    let (mut board, mut rules, settings) = empty_board(layers);

    let (ox, oy) = OUTLINE_HALF_EXTENT;
    board.insert_item(Item::new(
        NetNos::EMPTY,
        0,
        FixedState::SystemFixed,
        ItemKind::BoardOutline(BoardOutline {
            borders: vec![Polyline::from_int_points(&[
                IntPoint::new(-ox, -oy),
                IntPoint::new(ox, -oy),
                IntPoint::new(ox, oy),
                IntPoint::new(-ox, oy),
                IntPoint::new(-ox, -oy),
            ])],
            keepout_outside_outline: false,
        }),
    ));

    let pad_padstack_no = board.padstacks_mut().push(BoardPadstack::uniform(
        0,
        0,
        TileShape::Box(IntBox::new(-200, -200, 200, 200)),
    ));
    rules.nets.add("N2", 1, false, 0);

    // Net 1 low with its detour bulging up, net 2 high with its detour bulging
    // down; 45°-legal corners so the trace inserts on a 45° board too.
    let rows: [(i32, i32, i32); 2] = [(1, -900, 500), (2, 900, -500)];
    for (net, y, peak) in rows {
        let left = IntPoint::new(-2_000, y);
        let right = IntPoint::new(2_000, y);
        for (pin_no, center) in [left, right].into_iter().enumerate() {
            board.insert_pin(
                Pin {
                    center,
                    padstack_no: pad_padstack_no,
                    pin_no: pin_no as u32,
                    layers: LayerRange::new(0, 0),
                },
                NetNos::of(net),
                1,
            );
        }
        if !detoured {
            continue;
        }
        let rise = (peak - y).abs();
        let corners = [
            left,
            IntPoint::new(left.x + rise, peak),
            IntPoint::new(right.x - rise, peak),
            right,
        ];
        board
            .insert_trace_points(
                &corners,
                0,
                TEST_HALF_WIDTH,
                NetNos::of(net),
                1,
                FixedState::Unfixed,
                None,
            )
            .expect("the detour inserts");
    }

    board.ensure_compensated_tree(1);
    SmallBoard {
        board,
        rules,
        settings,
    }
}

/// `(incompletes, vias, weighted length)`, the key promise 2 is about.
fn lex(key: &BoardKey) -> (usize, usize, f64) {
    (
        key.incomplete_count,
        key.via_count,
        key.weighted_trace_length,
    )
}

fn strictly_below(after: &BoardKey, before: &BoardKey) -> bool {
    let (ai, av, al) = lex(after);
    let (bi, bv, bl) = lex(before);
    ai < bi || (ai == bi && (av < bv || (av == bv && al < bl)))
}

fn optimize(fixture: &mut SmallBoard, metric: LengthMetric) -> OptimizerOutcome {
    let mut optimizer = BatchOptimizer::new(&fixture.rules, &fixture.settings);
    optimizer.length_metric = metric;
    optimizer.board_units_per_mm = DEFAULT_BOARD_UNITS_PER_MM;
    optimizer.run_batch_loop(&mut fixture.board)
}

// ---------------------------------------------------------------------------

#[test]
fn the_optimizer_never_increases_incompletes_or_violations() {
    for detoured in [true, false] {
        for metric in [LengthMetric::UpstreamMixed, LengthMetric::WeightedBoth] {
            let mut fixture = small_board(detoured);
            let before = stats::pass_score(
                &mut fixture.board,
                &fixture.rules,
                &fixture.settings.scoring,
                DEFAULT_BOARD_UNITS_PER_MM,
            );
            let outcome = optimize(&mut fixture, metric);
            let after = stats::pass_score(
                &mut fixture.board,
                &fixture.rules,
                &fixture.settings.scoring,
                DEFAULT_BOARD_UNITS_PER_MM,
            );

            assert!(
                after.incomplete_count <= before.incomplete_count,
                "detoured={detoured} metric={metric:?}: incompletes {} -> {}",
                before.incomplete_count,
                after.incomplete_count
            );
            assert!(
                after.violation_count <= before.violation_count,
                "detoured={detoured} metric={metric:?}: violations {} -> {}",
                before.violation_count,
                after.violation_count
            );
            // The stage always terminates on one of its four documented exits.
            assert!(
                outcome.passes.len() as i32 <= fixture.settings.optimizer.max_passes,
                "{:?}",
                outcome.exit
            );
        }
    }
}

#[test]
fn a_pass_that_claims_an_improvement_strictly_lowers_vias_or_length() {
    let mut fixture = small_board(true);
    let outcome = optimize(&mut fixture, LengthMetric::WeightedBoth);

    for pass in &outcome.passes {
        if pass.items_improved > 0 {
            assert!(
                strictly_below(&pass.key_after, &pass.key_before),
                "pass #{} kept {} items but the board key did not fall: {:?} -> {:?}",
                pass.pass_no,
                pass.items_improved,
                pass.key_before,
                pass.key_after
            );
            if pass.key_after.incomplete_count == pass.key_before.incomplete_count {
                assert!(
                    pass.key_after.via_count < pass.key_before.via_count
                        || pass.key_after.weighted_trace_length
                            < pass.key_before.weighted_trace_length,
                    "pass #{}: vias {} -> {}, weighted length {} -> {}",
                    pass.pass_no,
                    pass.key_before.via_count,
                    pass.key_after.via_count,
                    pass.key_before.weighted_trace_length,
                    pass.key_after.weighted_trace_length
                );
            }
        } else {
            assert_eq!(
                pass.key_after, pass.key_before,
                "pass #{} kept nothing, so every reroute must have been rolled back",
                pass.pass_no
            );
        }
    }
}

#[test]
fn the_optimizer_shortens_a_deliberately_detoured_board() {
    let mut fixture = small_board(true);
    let before = stats::board_key(&mut fixture.board);
    assert_eq!(before.incomplete_count, 0, "the bench starts fully wired");

    let outcome = optimize(&mut fixture, LengthMetric::WeightedBoth);
    let after = stats::board_key(&mut fixture.board);

    assert_eq!(after.incomplete_count, 0, "and stays fully wired");
    assert!(
        outcome.items_improved() > 0,
        "a bulging detour between two pads on an otherwise empty board is the \
         easiest win there is; the stage exited {:?}",
        outcome.exit
    );
    assert!(
        strictly_below(&after, &before),
        "{before:?} -> {after:?} ({:?})",
        outcome.exit
    );
    assert!(
        after.weighted_trace_length < before.weighted_trace_length,
        "and the win was copper: {} -> {}",
        before.weighted_trace_length,
        after.weighted_trace_length
    );
}

#[test]
fn the_optimizer_leaves_an_already_routed_board_connected() {
    // Route the bench from scratch first, so the optimizer sees the engine's
    // own output rather than a hand-drawn detour — the release's sequencing:
    // router batch loop, then optimization stage.
    let mut fixture = small_board(false);
    {
        let mut router = BatchRouter::new(&fixture.rules, &fixture.settings);
        router.run(&mut fixture.board, fixture.settings.max_passes);
    }
    let before = stats::board_key(&mut fixture.board);

    optimize(&mut fixture, LengthMetric::WeightedBoth);
    let after = stats::board_key(&mut fixture.board);

    assert!(
        after.incomplete_count <= before.incomplete_count,
        "{before:?} -> {after:?}"
    );
    assert!(!strictly_below(&before, &after), "{before:?} -> {after:?}");
}

#[test]
fn the_stage_gate_is_the_optimizer_enabled_flag() {
    let mut fixture = small_board(true);
    let before = stats::board_key(&mut fixture.board);

    let mut off = fixture.settings.clone();
    off.optimizer.enabled = Some(false);
    assert!(fr_router::run_optimization_stage(
        &mut fixture.board,
        &fixture.rules,
        &off,
        DEFAULT_BOARD_UNITS_PER_MM,
    )
    .is_none());
    assert_eq!(
        stats::board_key(&mut fixture.board),
        before,
        "a disabled stage touches nothing"
    );

    // The shipped default runs it (`DefaultSettings.java:126`, conformance #7).
    assert!(fixture.settings.run_optimizer());
    assert!(fr_router::run_optimization_stage(
        &mut fixture.board,
        &fixture.rules,
        &fixture.settings,
        DEFAULT_BOARD_UNITS_PER_MM,
    )
    .is_some());
}

#[test]
fn the_release_metric_and_the_fixed_metric_agree_on_safety_and_may_differ_on_zeal() {
    // Both metrics must respect promise 1; only the fixed one is allowed to be
    // claimed to respect promise 2 (conformance #87).
    let mut released = small_board(true);
    let released_outcome = optimize(&mut released, LengthMetric::UpstreamMixed);
    let mut fixed = small_board(true);
    let fixed_outcome = optimize(&mut fixed, LengthMetric::WeightedBoth);

    assert_eq!(stats::board_key(&mut released.board).incomplete_count, 0);
    assert_eq!(stats::board_key(&mut fixed.board).incomplete_count, 0);
    // The mixed comparison's length branch fires far more readily, so the
    // release keeps at least as many items as the fixed metric does.
    assert!(
        released_outcome.items_improved() >= fixed_outcome.items_improved(),
        "released kept {}, fixed kept {}",
        released_outcome.items_improved(),
        fixed_outcome.items_improved()
    );
}

#[test]
fn the_release_sequence_is_route_then_optimize() {
    // `thread_action()`: the router's batch loop runs to completion, then the
    // optimization stage runs on its output. Nothing interleaves them.
    let mut fixture = small_board(false);
    let outcome = fr_router::route_then_optimize(
        &mut fixture.board,
        &fixture.rules,
        &fixture.settings,
        fixture.settings.max_passes,
        DEFAULT_BOARD_UNITS_PER_MM,
    );

    assert!(!outcome.routing.passes.is_empty(), "the router ran first");
    assert!(
        outcome.optimization.is_some(),
        "and the optimizer ran after it, because the flag defaults on"
    );
    let after = stats::board_key(&mut fixture.board);
    assert_eq!(after.incomplete_count, outcome.routing.incomplete_count);

    // With the stage switched off the router still runs and the board is the
    // same board the router left behind.
    let mut off_fixture = small_board(false);
    let mut off = off_fixture.settings.clone();
    off.optimizer.enabled = Some(false);
    let off_outcome = fr_router::route_then_optimize(
        &mut off_fixture.board,
        &off_fixture.rules,
        &off,
        off.max_passes,
        DEFAULT_BOARD_UNITS_PER_MM,
    );
    assert!(off_outcome.optimization.is_none());
    assert!(!off_outcome.routing.passes.is_empty());
}
