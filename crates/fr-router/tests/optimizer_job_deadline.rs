//! A spent job budget ends the optimizer, at the pass **and** at the item.
//!
//! Its own test binary because the stop flag is process-wide and sticky: once this file sets it
//! there is no unsetting it, so no other test may share the process. (`request_job_stop` rather
//! than a clock for the same reason `fr-cli`'s deadline test uses `--job-timeout 0`: a budget
//! that is already gone is the only value that cannot race a loaded machine.)
//!
//! # What it costs not to have this
//!
//! The per-item router polls the budget, so before this the optimizer *did* terminate — but
//! only after ripping, re-attempting against a zero allowance and rolling back every remaining
//! item, and a pass ends on 50 consecutive failures rather than on the first. Measured on
//! `Issue209-split10` (2 755 items, `-mp 20`, idle machine): **50 items and 3.0-5.7 s per pass
//! past the deadline**, and `run_batch_loop` then started *another* pass, for 11.4 s past a
//! `--job-timeout` the bench harness allows 15 s of margin for. At four concurrent sweep shards
//! that overran the cap and the run was `SIGKILL`ed holding a routed board it had not written.

use fr_board::item::FixedState;
use fr_board::net_nos::NetNos;
use fr_geometry::planar::IntPoint;
use fr_router::optimizer::{run_optimization_stage, LoopExit, PassExit};
use fr_router::test_support::TestFixture;
use fr_router::{request_job_stop, BatchOptimizer};

/// A board with several unfixed traces, so an optimizer that does *not* stop has plenty of
/// items to walk.
fn board_with_traces(count: i32) -> TestFixture {
    let mut fixture = TestFixture::empty_two_layer();
    for index in 0..count {
        let x = 100 + 40 * index;
        fixture
            .board
            .insert_trace_points(
                &[IntPoint::new(x, -1_200), IntPoint::new(x, -1_000)],
                0,
                20,
                NetNos::of(1),
                1,
                FixedState::Unfixed,
                None,
            )
            .expect("the trace inserts");
    }
    fixture
}

#[test]
fn a_spent_budget_ends_the_sweep_and_the_pass_loop() {
    let mut fixture = board_with_traces(8);
    let settings = fixture.settings.clone();

    // Sanity first, with the budget intact: the sweep visits the items and the pass ends
    // because it ran out of them, not because of anything below.
    let mut optimizer = BatchOptimizer::new(&fixture.rules, &settings);
    let warm = optimizer.opt_route_pass(&mut fixture.board, 1, true);
    assert_ne!(
        warm.exit,
        PassExit::JobDeadline,
        "no budget is armed yet: {warm:?}"
    );
    assert!(warm.items_attempted > 0, "{warm:?}");

    // From here the process is stopping. Nothing may attempt another item.
    request_job_stop();

    let mut optimizer = BatchOptimizer::new(&fixture.rules, &settings);
    let pass = optimizer.opt_route_pass(&mut fixture.board, 1, true);
    assert_eq!(pass.exit, PassExit::JobDeadline);
    assert_eq!(
        pass.items_attempted, 0,
        "not one item may be ripped and rolled back after the budget is gone: {pass:?}"
    );

    let mut optimizer = BatchOptimizer::new(&fixture.rules, &settings);
    let outcome = optimizer.run_batch_loop(&mut fixture.board);
    assert_eq!(outcome.exit, LoopExit::JobDeadline);
    assert!(
        outcome.passes.is_empty(),
        "the loop must not start a pass it cannot finish: {:?}",
        outcome.passes
    );

    // And the stage as the pipeline calls it: still a result, still no work.
    let outcome = run_optimization_stage(&mut fixture.board, &fixture.rules, &settings, 1.0)
        .expect("the stage still reports");
    assert_eq!(outcome.exit, LoopExit::JobDeadline);
    assert_eq!(outcome.total_items_optimized, 0);
}
