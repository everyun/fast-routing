//! Determinism: two runs of the same search settle the same sequence.
//!
//! `maze-engine.md` §13 makes this the validation gate for the whole engine:
//! *"validate the Rust engine against the Java one by diffing the settle order
//! `(door_id, section, g, f)` on identical boards with the deterministic
//! tie-breaks enabled."* Cross-tool that diff is limited to `(g, f)` and the
//! sequence length, because our ids are our own arena ids; **within** our own
//! runs the whole key must be reproducible, and that is what this asserts.

use fr_router::maze::search::{MazeSearch, SettleRecord};
use fr_router::test_support::TestFixture;

fn settle_sequence(with_trace: bool) -> (Vec<SettleRecord>, usize) {
    let fixture = TestFixture::with_foreign_trace();
    let mut engine = fixture.engine();
    engine.ctrl.ripup_allowed = true;
    engine.ctrl.ripup_costs = engine.ctrl.start_ripup_costs;
    let pads = fixture.pad_ids();
    let mut search =
        MazeSearch::get_instance(&mut engine, &pads[..1], &pads[1..2]).expect("instance");
    search.record_settles(with_trace);
    let hops = search
        .find_connection()
        .map(|connection| connection.backtrack.len())
        .unwrap_or(0);
    (search.settles().to_vec(), hops)
}

#[test]
fn two_runs_settle_the_identical_sequence() {
    let (first, first_hops) = settle_sequence(true);
    let (second, second_hops) = settle_sequence(true);

    assert!(!first.is_empty(), "the search must settle something");
    assert_eq!(first.len(), second.len(), "settle counts diverge");
    assert_eq!(first_hops, second_hops, "backtrack lengths diverge");

    for (index, (a, b)) in first.iter().zip(second.iter()).enumerate() {
        // `frlab_parse.py::diff_settles` calls a `(door_id, section)` mismatch
        // a `topology` divergence and a g/f drift a `value` divergence; within
        // one implementation neither may happen.
        assert_eq!(
            (a.door_id, a.section),
            (b.door_id, b.section),
            "topology divergence at settle {index}: {a:?} vs {b:?}"
        );
        assert_eq!(a.g.to_bits(), b.g.to_bits(), "g drift at settle {index}");
        assert_eq!(a.f.to_bits(), b.f.to_bits(), "f drift at settle {index}");
    }
}

#[test]
fn recording_the_trace_does_not_change_the_search() {
    let (traced, traced_hops) = settle_sequence(true);
    let (untraced, untraced_hops) = settle_sequence(false);
    assert!(untraced.is_empty(), "recording is off by default");
    assert_eq!(traced_hops, untraced_hops);
    assert!(!traced.is_empty());
}

#[test]
fn f_is_never_below_g_so_the_heuristic_stays_non_negative() {
    // `f = g + h` with `h >= 0` (`DestinationDistance` is a lower bound on a
    // non-negative cost). A settle with `f < g` would mean a negative estimate
    // and would break the A* argument.
    let (settles, _) = settle_sequence(true);
    for record in &settles {
        assert!(
            record.f >= record.g - 1e-9,
            "f < g at door {} section {}: {record:?}",
            record.door_id,
            record.section
        );
    }
}

#[test]
fn the_settle_sequence_is_non_decreasing_in_f() {
    // A best-first pop order over a consistent heuristic settles in
    // non-decreasing `f`. The delayed-occupation requeue only ever raises `f`,
    // so this holds for the ripup path too.
    let (settles, _) = settle_sequence(true);
    for window in settles.windows(2) {
        assert!(
            window[1].f >= window[0].f - 1e-9,
            "f decreased between settles: {:?} then {:?}",
            window[0],
            window[1]
        );
    }
}
