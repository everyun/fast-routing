//! The per-connection budget, proven on a real imported board.
//!
//! `fr_router::batch`'s own end-to-end budget case runs on a four-pad synthetic
//! fixture whose connections settle a handful of maze elements. That proves the
//! chain is wired; it does not prove the bail survives a board with real rooms,
//! real obstacles and a search that would otherwise settle thousands of
//! elements. This file does that, on an upstream fixture, and it lives in
//! `fr-core` because that is the lowest crate that can turn a `.dsn` into a
//! [`fr_board::Board`].
//!
//! The claim under test is the one that matters operationally: **an exhausted
//! allowance stops the search rather than being merely counted.** So the assert
//! is not "some counter moved" — it is that the same board, same router, same
//! pass, routes under the default policy and routes *nothing* under a
//! one-settle ceiling, while the pass reports every connection as having hit its
//! budget.
//!
//! [`fr_router::batch::budget::BudgetMode::Deterministic`] is used rather than
//! the wall clock because the point is a bail that cannot depend on how loaded
//! the machine is: a wall-clock ceiling small enough to bind here would be a
//! race, and the effort ceiling is the same code path past
//! `ConnectionClock::is_exhausted`.

use std::path::PathBuf;

use fr_io::specctra::import;
use fr_router::batch::budget::BudgetPolicy;
use fr_router::batch::{incomplete_count, BatchRouter};
use fr_rules::router_settings::RouterSettings;

/// A small upstream fixture with nothing routed and real geometry: 12 multi-pin
/// nets, 33 connections, two layers.
const FIXTURE: &str = "Issue026-J2_reference.dsn";

fn fixture_dir() -> PathBuf {
    std::env::var_os("FR_FIXTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/freerouting-upstream/fixtures"))
}

fn settings(layer_count: usize) -> RouterSettings {
    let mut settings = RouterSettings::default();
    settings.set_layer_count(layer_count);
    settings
}

#[test]
fn a_one_settle_ceiling_stops_every_connection_on_a_real_board() {
    let path = fixture_dir().join(FIXTURE);
    let Ok(bytes) = std::fs::read(&path) else {
        eprintln!("skipping: {} is not on disk", path.display());
        return;
    };
    let design = import(&bytes).expect("the fixture imports");
    let layer_count = design.rules.layer_structure.len();
    let settings = settings(layer_count);

    // The unbounded pass, for the contrast: a real board with real work on it.
    let mut free_board = design.board.clone();
    let mut free = BatchRouter::new(&design.rules, &settings);
    let free_pass = free.autoroute_pass(&mut free_board, 1);
    assert!(
        free_pass.routed > 1,
        "the fixture must give the router real work: {free_pass:?}"
    );
    assert_eq!(free_pass.connections_hitting_budget, 0, "{free_pass:?}");
    assert!(
        free_pass.effort_used > 100 * free_pass.queued as u64,
        "a real search settles far more than one element per connection: {free_pass:?}"
    );

    // The same pass, with one settled element as the whole allowance.
    let mut board = design.board.clone();
    let incomplete_before = incomplete_count(&mut board);
    let mut router = BatchRouter::new(&design.rules, &settings)
        .with_budget(BudgetPolicy::deterministic().with_effort_ceiling(Some(1)));
    let pass = router.autoroute_pass(&mut board, 1);

    assert_eq!(pass.queued, free_pass.queued, "the same work list: {pass:?}");
    assert_eq!(pass.routed, 0, "no connection can finish: {pass:?}");
    assert_eq!(
        pass.connections_hitting_budget, pass.queued,
        "every connection must be reported as having hit its allowance: {pass:?}"
    );
    assert_eq!(
        pass.effort_used, pass.queued as u64,
        "exactly one settled element each — the bail is at the top of the pop \
         loop, not after the search: {pass:?}"
    );

    // And the board is untouched: a stopped connection inserts nothing.
    assert_eq!(
        incomplete_count(&mut board),
        incomplete_before,
        "a budget bail must not leave copper behind"
    );
    board.assert_consistent();
}

/// The other half of the claim: the default policy is the *upstream constant*,
/// so it never binds on this suite. A regression that made the shipped budget
/// bind would show up as connections silently going unrouted, which is exactly
/// the failure mode the counter exists to make visible.
#[test]
fn the_shipped_budget_never_binds_on_this_fixture() {
    let path = fixture_dir().join(FIXTURE);
    let Ok(bytes) = std::fs::read(&path) else {
        eprintln!("skipping: {} is not on disk", path.display());
        return;
    };
    let design = import(&bytes).expect("the fixture imports");
    let settings = settings(design.rules.layer_structure.len());
    let mut board = design.board.clone();

    // Three passes, not the suite's twenty: the allowance *doubles* per pass, so
    // if it does not bind on pass 1 it cannot bind later, and twenty passes of a
    // real board in a debug build is a minute of test time for no extra claim.
    let mut router = BatchRouter::new(&design.rules, &settings);
    let outcome = router.run(&mut board, 3);
    let hit: usize = outcome
        .passes
        .iter()
        .map(|pass| pass.connections_hitting_budget)
        .sum();
    assert_eq!(
        hit, 0,
        "the shipped 100 s per-connection allowance must not bind on a board \
         the whole run finishes in well under a second"
    );
}
