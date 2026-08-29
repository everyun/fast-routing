//! **T1, adversarially.** The two mechanical claims
//! `fr_router::optimizer::parallel`'s soundness argument rests on, each against
//! a case built to break it.
//!
//! The argument is: *"every mutation the parallel optimizer performs is a
//! mutation the sequential optimizer could have performed at that moment"*. It
//! has exactly two load-bearing steps, and `tests/optimizer_parallel.rs` covers
//! neither directly — that file measures the *outcome* (score never falls, two
//! runs agree) rather than the mechanism, and both of these can break while the
//! score still rises.
//!
//! 1. **A worker produces a verdict and never copper.**
//!    [`Disposition::Discard`] has to leave the worker's board observably as it
//!    found it, or a stale clone poisons every later verdict on that worker —
//!    and `WorkerPool` reuses a board across windows, so the poison persists.
//!    Asserted by rendering the whole item store before and after an attempt
//!    that *did* improve, i.e. the branch on which `Keep` would have committed.
//!    A vacuity guard replays the same attempt under `Keep` on a clone and
//!    requires the two to disagree.
//!
//! 2. **An `Improved` verdict is a proposal, not a decision.** Several
//!    candidates in one window are all evaluated against the *window-start*
//!    board, so their proposed reroutes can be mutually exclusive: the first
//!    promotion takes copper the second one's verdict assumed was free. A
//!    promoter that trusted the verdict — or took the shortcut the module docs
//!    explicitly defer, adopting the winning worker's store outright — would
//!    land both, and the result is copper on copper. The assertion is therefore
//!    a full [`fr_check::check_clearance`] scan of the finished board against
//!    the same scan of the input, at every thread count, plus the per-*pass*
//!    board-key lattice rather than only the run's score.
//!
//! The third claim — determinism at a fixed thread count — is covered by
//! `tests/optimizer_parallel.rs` and is not repeated here.

use fr_board::board::{Board, Layer, LayerStructure};
use fr_board::item::{BoardOutline, FixedState, Item, ItemId, ItemKind, LayerRange, Pin};
use fr_board::net_nos::NetNos;
use fr_board::shape::{Padstack as BoardPadstack, TileShape};
use fr_check::{check_clearance, shape_store, StoreView};
use fr_geometry::planar::{IntBox, IntPoint, Polyline};
use fr_router::batch::threads::Threads;
use fr_router::optimizer::attempt::{attempt_item, AttemptConfig, AttemptScratch, Disposition};
use fr_router::optimizer::parallel::max_window_size;
use fr_router::optimizer::{
    stats, BatchOptimizer, LengthMetric, MazeRerouter, DEFAULT_BOARD_UNITS_PER_MM,
};
use fr_router::test_support::{empty_board, OUTLINE_HALF_EXTENT, TEST_HALF_WIDTH};
use fr_rules::board_rules::BoardRules;
use fr_rules::router_settings::RouterSettings;

struct Fixture {
    board: Board,
    rules: BoardRules,
    settings: RouterSettings,
}

/// `nets` point-to-point nets wired the long way round, packed `row_pitch`
/// apart so that neighbouring nets are competing for the same space.
///
/// The crowding is the point. At the pitch used below, straightening two
/// neighbouring nets puts their copper within a trace width of each other, so
/// the improvements the workers propose for one window cannot all be taken —
/// which is the situation the promoter's live re-run exists to resolve.
fn crowded_board(nets: usize, row_pitch: i32) -> Fixture {
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
        let y = -1_800 + row_pitch * index as i32;
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

/// Every clearance violation the referee's own checker finds on the board as it
/// stands. The *input's* count is the baseline; only a rise is a failure.
fn clearance_violations(fixture: &Fixture) -> usize {
    let shapes = shape_store(&fixture.board);
    let view = StoreView::new(
        fixture.board.store(),
        &shapes,
        &fixture.rules.clearance_matrix,
        fixture.board.layers().len(),
    );
    check_clearance(&view).len()
}

fn board_text(board: &Board) -> String {
    board
        .store()
        .iter()
        .map(|(id, item)| format!("{id:?} {item:?}\n"))
        .collect()
}

fn trace_ids(board: &Board) -> Vec<ItemId> {
    board
        .store()
        .iter()
        .filter(|(_, item)| matches!(item.kind, ItemKind::Trace(_)))
        .map(|(id, _)| id)
        .collect()
}

fn optimize(fixture: &mut Fixture, threads: Threads) -> fr_router::optimizer::OptimizerOutcome {
    let mut optimizer =
        BatchOptimizer::new(&fixture.rules, &fixture.settings).with_threads(threads);
    optimizer.length_metric = LengthMetric::WeightedBoth;
    optimizer.board_units_per_mm = DEFAULT_BOARD_UNITS_PER_MM;
    optimizer.run_batch_loop(&mut fixture.board)
}

const NETS: usize = 24;
/// Tight enough that neighbouring nets contend, loose enough that the optimizer
/// can still win something. `optimizer_parallel.rs` uses 400, which leaves each
/// net its own band; 150 is *too* tight — the sweep hands out items and not one
/// of them improves, which the vacuity guards below turn into a failure rather
/// than into a green test that proves nothing.
const CROWDED_PITCH: i32 = 300;
/// Sequential first, so it is the control for the two parallel runs.
/// A `const` would need `Threads::new` to be `const fn`; only
/// [`Threads::single`] is.
fn thread_counts() -> [Threads; 3] {
    [Threads::single(), Threads::new(2), Threads::new(4)]
}

// ---------------------------------------------------------------------------
// Claim 1 — a worker's attempt is observably read-only.
// ---------------------------------------------------------------------------

#[test]
fn a_discarded_attempt_leaves_the_worker_board_where_it_found_it() {
    let mut fixture = crowded_board(NETS, CROWDED_PITCH);
    let settings = fixture.settings.clone();
    let cfg = AttemptConfig {
        rules: &fixture.rules,
        pass_settings: &settings,
        min_cumulative_trace_length: stats::board_key(&mut fixture.board).weighted_trace_length,
        length_metric: LengthMetric::WeightedBoth,
        via_ripup_costs: 100,
        trace_ripup_costs: 60,
        max_autoroute_passes: 6,
        with_preferred_directions: true,
    };

    let mut improved_at_least_once = false;
    for item in trace_ids(&fixture.board) {
        let before = board_text(&fixture.board);
        let mut scratch = AttemptScratch::new();
        let mut rerouter = MazeRerouter;
        let discarded = attempt_item(
            &mut fixture.board,
            item,
            &cfg,
            &mut rerouter,
            &mut scratch,
            Disposition::Discard,
        );
        assert_eq!(
            before,
            board_text(&fixture.board),
            "{item:?}: a discarded attempt changed the board"
        );

        if discarded.improved() {
            improved_at_least_once = true;
            // The same attempt under `Keep`, on a clone, has to actually move
            // the board — otherwise the equality above is vacuous for this item.
            let mut kept_board = fixture.board.clone();
            let mut scratch = AttemptScratch::new();
            let mut rerouter = MazeRerouter;
            let kept = attempt_item(
                &mut kept_board,
                item,
                &cfg,
                &mut rerouter,
                &mut scratch,
                Disposition::Keep,
            );
            assert!(kept.improved(), "{item:?}: Keep and Discard disagreed");
            assert_ne!(
                before,
                board_text(&kept_board),
                "{item:?}: an improving attempt left the board unchanged, so the \
                 rollback assertion above proves nothing"
            );
        }
    }
    assert!(
        improved_at_least_once,
        "no item on the fixture improved, so the rollback was never exercised \
         on the branch that commits"
    );
}

// ---------------------------------------------------------------------------
// Claim 2 — conflicting candidates inside one window.
// ---------------------------------------------------------------------------

#[test]
fn conflicting_candidates_in_one_window_never_both_land() {
    let baseline = clearance_violations(&crowded_board(NETS, CROWDED_PITCH));

    for threads in thread_counts() {
        let mut fixture = crowded_board(NETS, CROWDED_PITCH);
        let outcome = optimize(&mut fixture, threads);

        assert!(
            clearance_violations(&fixture) <= baseline,
            "{threads:?}: the optimizer added clearance violations ({} > {baseline}) \
             — two mutually exclusive improvements both landed",
            clearance_violations(&fixture)
        );
        assert!(
            outcome.score_after.normalized >= outcome.score_before.normalized,
            "{threads:?}: score {} -> {}",
            outcome.score_before.normalized,
            outcome.score_after.normalized
        );
        // Per *pass*, not just per run: the promoter commits per item, so a pass
        // is the granularity a bad promotion shows up at. `BoardKey` is only
        // `PartialEq`, so the lattice is spelled out — the same three components
        // `ItemRouteResult` compares.
        for pass in &outcome.passes {
            let (before, after) = (pass.key_before, pass.key_after);
            assert!(
                after.incomplete_count <= before.incomplete_count
                    && after.via_count <= before.via_count
                    && after.weighted_trace_length <= before.weighted_trace_length + 1e-6,
                "{threads:?}: pass {} raised the board key: {before:?} -> {after:?}",
                pass.pass_no,
            );
        }
    }
}

/// The fixture has to span more than one window and keep something at each
/// thread count, or the assertions above are measuring an optimizer that never
/// speculated.
#[test]
fn the_crowded_fixture_spans_several_windows() {
    for threads in thread_counts() {
        let mut fixture = crowded_board(NETS, CROWDED_PITCH);
        let outcome = optimize(&mut fixture, threads);
        assert!(
            outcome.total_items_optimized as usize > max_window_size(threads),
            "{threads:?}: {} items is at most one window of {}",
            outcome.total_items_optimized,
            max_window_size(threads)
        );
        assert!(
            outcome.items_improved() > 0,
            "{threads:?}: nothing was ever kept"
        );
    }
}
