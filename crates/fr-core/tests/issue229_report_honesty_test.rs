//! `Issue229`: the board where the router reported 702 routes and moved no copper.
//!
//! # What the board is
//!
//! `Issue229-display-8-digit-hc595.dsn` arrives 218 wires and 24 vias into a KiCad routing job,
//! and **every one of them is `(type protect)`** — the only fixture in the corpus of which that
//! is true. `protect` imports as `USER_FIXED` (`Wiring.calc_fixed`, and `fr_io`'s
//! `specctra::ast` maps it the same way), and a `USER_FIXED` trace cannot be split:
//! `PolylineTrace.split` refuses at `:707-709`, and so does this port's
//! `Board::split_trace_at_line`, because splitting an item whose deletion is forbidden would
//! leave the original *and* both halves on the board.
//!
//! Nothing else on this board can therefore ever become connected to that copper. Freerouting's
//! contact rule is exact endpoint equality — a trace's contacts are the items touching its
//! **first or last corner** (`Trace.get_normal_contacts:172-176`), a drill's are the items at
//! its **centre** (`DrillItem:266-283`) — so a route that lands in the middle of a protected
//! trace is a contact only once that trace has been split at the landing point. It cannot be.
//! Java agrees, in its way: the manifest classifies this fixture `java_degenerate`, "finished
//! with final score 0.00 (nothing to route)", 0.03 s of tool time.
//!
//! # What went wrong, and what this test pins
//!
//! Our board came out identical too — correctly — but the *report* claimed **702 routed
//! connections** at `-mp 20`, because three upstream code paths each say "routed" without
//! leaving copper:
//!
//! * a located connection of a single corner is "already at the target" and inserts nothing
//!   (`InsertFoundConnectionAlgo:95-103`);
//! * `connect_to_trace` returns `true` for a corner already on the target trace's polyline
//!   (`RoutingBoard:1100-1105`), leaving the contact to the `normalize_traces` that follows —
//!   the split that is refused here;
//! * what does get inserted is a sub-unit stub that the end-of-pass `remove_tails` deletes.
//!
//! So the assertions below are about the *report*, and about the copper, together: they are
//! only meaningful as a pair, because either alone is satisfied by the bug. A run that claims
//! routes must have copper to show for them.

mod support;

use fr_core::{MazeOptions, RouterStage, RoutingJob};

/// The fixture, or `None` with the loud skip `support` prints.
fn issue229() -> Option<RoutingJob> {
    let path = support::fixtures_dir()?.join("Issue229-display-8-digit-hc595.dsn");
    if !path.is_file() {
        eprintln!("SKIP: {} is not present", path.display());
        return None;
    }
    Some(RoutingJob::from_path(path).expect("the fixture reads"))
}

/// The invariant, on the board that used to break it: **a pass may not report more routed
/// connections than it left copper on the board.**
///
/// Stated as an inequality rather than as "0 routes", because the honest number for this board
/// is a property of the router's search, not of this fix — a future locator that reaches the
/// protected copper's *end corners* would legitimately route some of these and would leave the
/// copper to prove it. What may never come back is the pairing that made the first full-suite
/// G2.1 read unreadable: hundreds of reported routes over an unchanged board.
#[test]
fn a_reported_route_has_copper_to_show_for_it() {
    let Some(job) = issue229() else { return };

    let imported = job.import().expect("the fixture imports");
    let before = fr_core::BoardCensus::of(&imported.board);
    // The premise: this is a pre-routed board, and its wiring is all protected.
    assert_eq!((before.traces, before.vias), (218, 24));

    for passes in [1, 3] {
        // `RoutingJob` is deliberately not `Clone` — it owns the design bytes — so each pass
        // count gets its own job.
        let report = issue229()
            .expect("the fixture is still there")
            .with_stage(RouterStage::Maze(MazeOptions::with_passes(passes)))
            .run()
            .expect("the fixture routes");
        let router = report.router.clone().expect("the stage ran");
        let after = report.census;
        let copper_added =
            (after.traces + after.vias) as i64 - (before.traces + before.vias) as i64;

        assert!(
            router.routed as i64 <= copper_added.max(0),
            "-mp {passes}: {} routed connections but {copper_added} copper items added \
             (traces {} -> {}, vias {} -> {})",
            router.routed,
            before.traces,
            after.traces,
            before.vias,
            after.vias,
        );
    }
}

/// The same board, spelled out as the numbers this fix produced, so a change of behaviour shows
/// up as a diff rather than as a silently weaker inequality above.
///
/// Every connection on the work list fails, and the connectivity the referee reports is exactly
/// the imported board's. Three passes rather than the twenty the bench runs: the behaviour is
/// per-pass, and twenty of them cost a minute in a debug build for no extra coverage.
#[test]
fn the_protected_board_reports_no_routes_and_changes_no_copper() {
    let Some(job) = issue229() else { return };

    let imported = job.import().expect("the fixture imports");
    let before = fr_core::BoardCensus::of(&imported.board);
    let baseline = fr_core::Referee::judge(&imported, &fr_check::score::ScoreWeights::V2);

    let report = job
        .with_stage(RouterStage::Maze(MazeOptions::with_passes(3)))
        .run()
        .expect("the fixture routes");
    let router = report.router.clone().expect("the stage ran");

    assert_eq!(router.routed, 0, "no connection can be made on this board");
    assert!(
        router.not_routed > 0,
        "and the failures are reported: {router:?}"
    );
    assert_eq!(
        report.census, before,
        "the copper is the imported copper, item for item"
    );
    assert_eq!(
        report.stats.incomplete_count, baseline.stats.incomplete_count,
        "and so is the connectivity"
    );
    assert_eq!(
        report.stats.clearance_violation_count, 0,
        "a board we did not touch cannot have gained a violation"
    );
}
