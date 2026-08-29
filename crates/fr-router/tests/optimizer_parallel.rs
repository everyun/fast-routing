//! **T1.** The parallel optimizer against the three promises it inherits and the
//! one it adds.
//!
//! Inherited from `optimizer_small_board.rs` — and they are inherited *by
//! construction*, because a worker never writes to the live board and every
//! promotion goes through the same `opt_route_item` the sequential driver calls:
//!
//! 1. it never makes the board worse;
//! 2. when it claims an improvement, the board key actually fell.
//!
//! Added, and the reason the flag can exist at all:
//!
//! 3. **it is reproducible at a fixed thread count.** Two runs at `--threads 4`
//!    leave byte-identical boards. That is what
//!    `fr_router::optimizer::parallel`'s determinism argument claims, and it is
//!    the property `--deterministic --threads N` rests on.
//!
//! What is deliberately *not* asserted: that the parallel optimizer produces the
//! same board as the sequential one. It does not, and it is not supposed to — a
//! worker's `Rejected` verdict is trusted for the rest of its window even if a
//! promotion moved the context underneath it, which can hide an improvement the
//! sequential driver would have found. The promises above are what make that
//! trade safe.

use fr_board::board::{Board, Layer, LayerStructure};
use fr_board::item::{BoardOutline, FixedState, Item, ItemKind, LayerRange, Pin};
use fr_board::net_nos::NetNos;
use fr_board::shape::{Padstack as BoardPadstack, TileShape};
use fr_geometry::planar::{IntBox, IntPoint, Polyline};
use fr_router::batch::threads::Threads;
use fr_router::optimizer::{
    stats, BatchOptimizer, ItemRerouter, LengthMetric, RerouteRequest, DEFAULT_BOARD_UNITS_PER_MM,
};
use fr_router::test_support::{empty_board, OUTLINE_HALF_EXTENT, TEST_HALF_WIDTH};
use fr_rules::board_rules::BoardRules;
use fr_rules::router_settings::RouterSettings;

struct Fixture {
    board: Board,
    rules: BoardRules,
    settings: RouterSettings,
}

/// `nets` point-to-point nets, each already wired the long way round so the
/// optimizer has something to win back on every one of them.
///
/// Enough of them that the sweep spans more than one window at the thread counts
/// under test — the windowing is the whole of what the parallel driver adds, and
/// a fixture that fits in one window would test the promoter's first iteration
/// and nothing else.
fn detoured_board(nets: usize) -> Fixture {
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
    let pad = board.padstacks_mut().push(BoardPadstack::uniform(
        0,
        0,
        TileShape::Box(IntBox::new(-150, -150, 150, 150)),
    ));

    for index in 0..nets {
        let net = index as i32 + 1;
        if net > 1 {
            rules.nets.add(format!("N{net}"), 1, false, 0);
        }
        // Rows 500 apart, alternating which way the detour bulges, so no two
        // nets' spare copper occupies the same band.
        let y = -1_800 + 400 * index as i32;
        let peak = if index % 2 == 0 { y + 200 } else { y - 200 };
        let left = IntPoint::new(-1_600, y);
        let right = IntPoint::new(1_600, y);
        for (pin_no, center) in [left, right].into_iter().enumerate() {
            board.insert_pin(
                Pin {
                    center,
                    padstack_no: pad,
                    pin_no: pin_no as u32,
                    layers: LayerRange::new(0, 0),
                },
                NetNos::of(net),
                1,
            );
        }
        let rise = (peak - y).abs();
        board
            .insert_trace_points(
                &[
                    left,
                    IntPoint::new(left.x + rise, peak),
                    IntPoint::new(right.x - rise, peak),
                    right,
                ],
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
    Fixture {
        board,
        rules,
        settings,
    }
}

fn optimize(fixture: &mut Fixture, threads: Threads) -> fr_router::optimizer::OptimizerOutcome {
    let mut optimizer =
        BatchOptimizer::new(&fixture.rules, &fixture.settings).with_threads(threads);
    optimizer.length_metric = LengthMetric::WeightedBoth;
    optimizer.board_units_per_mm = DEFAULT_BOARD_UNITS_PER_MM;
    optimizer.run_batch_loop(&mut fixture.board)
}

/// Every item the board holds, in the store's own order, rendered.
///
/// The comparison determinism is asserted on. It is `Debug` rather than a
/// hand-rolled digest so that a failure prints the two boards' difference rather
/// than two hashes.
fn board_text(board: &Board) -> String {
    board
        .store()
        .iter()
        .map(|(id, item)| format!("{:?} {:?}\n", id, item))
        .collect()
}

const NETS: usize = 24;

// ---------------------------------------------------------------------------

/// Promise 1, on the parallel path.
#[test]
fn the_parallel_optimizer_never_makes_the_board_worse() {
    for threads in [Threads::new(2), Threads::new(4)] {
        let mut fixture = detoured_board(NETS);
        let before = stats::pass_score(
            &mut fixture.board,
            &fixture.rules,
            &fixture.settings.scoring,
            DEFAULT_BOARD_UNITS_PER_MM,
        );
        optimize(&mut fixture, threads);
        let after = stats::pass_score(
            &mut fixture.board,
            &fixture.rules,
            &fixture.settings.scoring,
            DEFAULT_BOARD_UNITS_PER_MM,
        );
        assert!(
            after.incomplete_count <= before.incomplete_count,
            "{threads:?}: incompletes {} -> {}",
            before.incomplete_count,
            after.incomplete_count
        );
        assert!(
            after.violation_count <= before.violation_count,
            "{threads:?}: violations {} -> {}",
            before.violation_count,
            after.violation_count
        );
    }
}

/// Promise 2, on the parallel path: a pass that kept items strictly lowered the
/// lexicographic board key, and a pass that kept none left it exactly alone.
#[test]
fn a_parallel_pass_that_keeps_items_strictly_lowers_the_board_key() {
    let mut fixture = detoured_board(NETS);
    let outcome = optimize(&mut fixture, Threads::new(4));
    for pass in &outcome.passes {
        let key = |k: &fr_router::optimizer::BoardKey| {
            (k.incomplete_count, k.via_count, k.weighted_trace_length)
        };
        let (before, after) = (key(&pass.key_before), key(&pass.key_after));
        if pass.items_improved > 0 {
            assert!(
                after < before,
                "pass #{} kept {} items but the key did not fall: {before:?} -> {after:?}",
                pass.pass_no,
                pass.items_improved
            );
        } else {
            assert_eq!(
                after, before,
                "pass #{} kept nothing and must have changed nothing",
                pass.pass_no
            );
        }
    }
}

/// Promise 3 — the one the flag rests on.
///
/// Not "the same as sequential": the same as *itself*, at the same thread count,
/// across runs whose worker interleaving is at the operating system's discretion.
#[test]
fn two_runs_at_the_same_thread_count_leave_identical_boards() {
    for threads in [Threads::new(2), Threads::new(4)] {
        let mut first = detoured_board(NETS);
        let mut second = detoured_board(NETS);
        assert_eq!(
            board_text(&first.board),
            board_text(&second.board),
            "the fixture itself has to be reproducible or this proves nothing"
        );

        let outcome_a = optimize(&mut first, threads);
        let outcome_b = optimize(&mut second, threads);

        assert_eq!(
            board_text(&first.board),
            board_text(&second.board),
            "{threads:?}: two runs left different boards"
        );
        assert_eq!(
            outcome_a.total_items_optimized,
            outcome_b.total_items_optimized
        );
        assert_eq!(outcome_a.items_improved(), outcome_b.items_improved());
        assert_eq!(outcome_a.passes.len(), outcome_b.passes.len());
        for (a, b) in outcome_a.passes.iter().zip(&outcome_b.passes) {
            assert_eq!(a, b, "{threads:?}: pass {} differs", a.pass_no);
        }
    }
}

/// A worker rebuilds a [`fr_router::optimizer::MazeRerouter`] on its own thread,
/// so an optimizer holding a caller-supplied one cannot fan out — and says so
/// rather than silently ignoring the thread count.
#[test]
fn a_scripted_rerouter_pins_the_optimizer_to_one_thread() {
    struct Noop;
    impl ItemRerouter for Noop {
        fn reroute(
            &mut self,
            _board: &mut Board,
            _rules: &BoardRules,
            _settings: &RouterSettings,
            _request: &RerouteRequest,
        ) {
        }
    }

    let fixture = detoured_board(2);
    let scripted = BatchOptimizer::with_rerouter(&fixture.rules, &fixture.settings, Box::new(Noop))
        .with_threads(Threads::new(8));
    assert!(!scripted.parallel_capable());
    assert_eq!(scripted.threads(), Threads::single());

    let production =
        BatchOptimizer::new(&fixture.rules, &fixture.settings).with_threads(Threads::new(8));
    assert!(production.parallel_capable());
    assert_eq!(production.threads(), Threads::new(8));
    // ... and the default is still sequential.
    assert_eq!(
        BatchOptimizer::new(&fixture.rules, &fixture.settings).threads(),
        Threads::single()
    );
}

/// The fixture has to actually give the optimizer something to do, at both
/// thread counts, or every assertion above is vacuous.
#[test]
fn the_fixture_is_not_a_no_op_for_the_parallel_optimizer() {
    for threads in [Threads::single(), Threads::new(2), Threads::new(4)] {
        let mut fixture = detoured_board(NETS);
        let outcome = optimize(&mut fixture, threads);
        assert!(
            outcome.total_items_optimized > 0,
            "{threads:?}: the sweep handed out nothing"
        );
        assert!(
            outcome.items_improved() > 0,
            "{threads:?}: nothing was ever kept, so the promises are untested"
        );
        assert!(
            outcome.score_after.normalized >= outcome.score_before.normalized,
            "{threads:?}: score {} -> {}",
            outcome.score_before.normalized,
            outcome.score_after.normalized
        );
    }
}
