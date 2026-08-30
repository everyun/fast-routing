//! The two corpus fixtures that carry an `(autoroute_settings …)` scope, and what the pipeline
//! now does with it.
//!
//! `Issue103-Board-Routed.dsn` and `Issue187-processor.Z80.dsn` are the whole of the corpus's
//! `with_autoroute_settings` coverage (`bench/fixtures-manifest.json`), and between them they
//! cover both states of the switch this wave is about: Issue103 says `(postroute off)` and
//! Issue187 says `(postroute on)`. Both put the scope ahead of their keepouts, so the
//! `layerStructure == null` guard lets both through — a fixture that hit the guard would prove
//! nothing about the application path.
//!
//! # Why this is a wall-clock bug and not a cosmetic one
//!
//! Issue103 arrives fully routed with 63 clearance violations. The optimizer sweeps every routed
//! item on such a board and improves almost none of them, so it runs until something stops it —
//! and with the file's `postroute off` ignored, the only thing that stopped it was the job
//! budget. Upstream reads the switch, never starts the stage, and reports the job finished in
//! 1.2 s (measured: `-mp 20`, release jar 2.3.0, `Job … finished with state: COMPLETED (elapsed:
//! 1.23 seconds)`; 4.8 s wall including JVM start).

mod support;

use fr_core::{MazeOptions, RouterStage, RoutingJob};

/// A fixture by name, or `None` with the loud skip `support` prints.
fn fixture(name: &str) -> Option<RoutingJob> {
    let path = support::fixtures_dir()?.join(name);
    if !path.is_file() {
        eprintln!("SKIP: {} is not present", path.display());
        return None;
    }
    Some(RoutingJob::from_path(path).expect("the fixture reads"))
}

/// The headline: the file says `(postroute off)` and the optimization stage does not run.
///
/// Asserted on [`fr_core::RouterReport::optimizer`] rather than on a wall-clock threshold,
/// because a timing assertion in a test suite is a flake generator; the wall-clock effect is what
/// the bench measures. The router stage itself still runs — `postroute` is not `autoroute`.
#[test]
fn issue103_postroute_off_skips_the_optimizer() {
    let Some(job) = fixture("Issue103-Board-Routed.dsn") else {
        return;
    };
    let design = job.import().expect("the fixture imports");
    let dsn = design
        .autoroute_settings
        .as_ref()
        .expect("the scope is ahead of the keepouts, so the guard lets it through");
    assert_eq!(dsn.run_optimizer, Some(false), "(postroute off)");
    // `(eu.mihosoft.freerouting.autoroute on)` is not the `autoroute` keyword, so it is skipped
    // as an unknown sub-scope and the router stays on by default.
    assert_eq!(dsn.run_router, None);
    drop(design);

    let report = fixture("Issue103-Board-Routed.dsn")
        .expect("the fixture is still there")
        .with_stage(RouterStage::Maze(MazeOptions::with_passes(2)))
        .run()
        .expect("the fixture routes");
    let router = report.router.as_ref().expect("the router stage ran");
    assert!(router.passes >= 1, "the batch loop still runs: {router:?}");
    assert!(
        router.optimizer.is_none(),
        "the file switched the optimization stage off: {:?}",
        router.optimizer
    );
}

/// The other state of the same switch, on the corpus's only other carrier. Without this the
/// test above is satisfied by an optimizer that never runs at all.
#[test]
fn issue187_postroute_on_still_runs_the_optimizer() {
    let Some(job) = fixture("Issue187-processor.Z80.dsn") else {
        return;
    };
    let design = job.import().expect("the fixture imports");
    let dsn = design
        .autoroute_settings
        .as_ref()
        .expect("this scope is ahead of its keepouts too");
    assert_eq!(dsn.run_optimizer, Some(true), "(postroute on)");
    drop(design);

    let report = fixture("Issue187-processor.Z80.dsn")
        .expect("the fixture is still there")
        .with_stage(RouterStage::Maze(MazeOptions::with_passes(1)))
        .run()
        .expect("the fixture routes");
    let router = report.router.as_ref().expect("the router stage ran");
    assert!(
        router.optimizer.is_some(),
        "an `on` in the file must not read as an `off`: {router:?}"
    );
}

/// `--no-optimizer` and `(postroute on)` disagree; the flag wins, as the API layer wins upstream
/// (`RoutingJobScheduler:100-104` re-applies the job's own settings as the highest-priority
/// source, after the DSN file's).
#[test]
fn the_command_line_still_overrides_a_files_postroute_on() {
    let Some(job) = fixture("Issue187-processor.Z80.dsn") else {
        return;
    };
    let report = job
        .with_stage(RouterStage::Maze(
            MazeOptions::with_passes(1).with_optimizer(false),
        ))
        .run()
        .expect("the fixture routes");
    let router = report.router.as_ref().expect("the router stage ran");
    assert!(router.optimizer.is_none());
}

/// The census on [`fr_core::JobReport`] used to be a single post-routing number that `fr-cli`
/// printed under the word "Imported". Both now exist and they mean different things.
#[test]
fn the_import_census_predates_the_router_and_the_board_census_does_not() {
    let Some(job) = fixture("Issue103-Board-Routed.dsn") else {
        return;
    };
    let imported = fr_core::BoardCensus::of(&job.import().expect("imports").board);

    let report = job
        .with_stage(RouterStage::Maze(MazeOptions::with_passes(2)))
        .run()
        .expect("the fixture routes");
    assert_eq!(
        report.import_census, imported,
        "the import census is the board the importer produced, whatever the router did next"
    );
    // Not an inequality: Issue103 arrives fully routed, and a pass that normalizes and drops
    // tails can leave *fewer* trace items than it found. That the two numbers are free to
    // disagree in either direction is the reason the "Imported" line may not print the second
    // one.
    assert_eq!(
        report.import_census.pins, report.census.pins,
        "no pass adds pins"
    );
    assert_eq!(
        report.import_census.board_outlines, report.census.board_outlines,
        "nor outlines"
    );
}
