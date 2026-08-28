//! Emits an `FRMETRIC settle` trace for the crate's synthetic two-pad board.
//!
//! The differential harness (`bench/tools/frlab_parse.py`) reads the same
//! record from the lab jar's `FRLAB settle` output, so this is the smallest
//! runnable producer of our side of the diff:
//!
//! ```text
//! FR_SETTLE_TRACE=/tmp/ours.log cargo run -p fr-router --example settle_trace
//! python3 bench/tools/frlab_parse.py /tmp/ours.log
//! ```

use fr_router::maze::search::MazeSearch;
use fr_router::test_support::TestFixture;

fn main() {
    let fixture = TestFixture::with_foreign_trace();
    let mut engine = fixture.engine();
    engine.ctrl.ripup_allowed = true;
    engine.ctrl.ripup_costs = engine.ctrl.start_ripup_costs;
    let pads = fixture.pad_ids();
    match MazeSearch::get_instance(&mut engine, &pads[..1], &pads[1..2]) {
        Ok(mut search) => {
            let outcome = search.find_connection();
            println!(
                "expansions={} routed={} hops={}",
                search.expansions(),
                outcome.is_ok(),
                outcome.map(|c| c.backtrack.len()).unwrap_or(0)
            );
        }
        Err(failure) => println!("init failed: {failure:?}"),
    }
    fr_metrics::settle_trace::flush();
}
