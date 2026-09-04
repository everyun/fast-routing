//! **T1b.** The portfolio, on a real board.
//!
//! The unit tests inside `batch::portfolio` prove the *shuffle* and the *tie
//! rule* on synthetic work lists. What they cannot prove is the one property a
//! user is entitled to rely on when they type `--portfolio N`:
//!
//! > the race never returns a board worse than the plain run would have.
//!
//! It holds because arm 0 always routes in the release's own item order, so the
//! plain run's board is one of the candidates and the referee picks between
//! them. This file checks it against a board the router does not close in one
//! pass — a board where the arms actually disagree is the only kind on which the
//! claim means anything.
//!
//! # What is *not* asserted
//!
//! That a shuffled arm wins. Whether item-order diversity buys connections is
//! the open question `ARCHITECTURE.md` poses ("does a diverse portfolio find
//! more connections within the same wall budget?"), it is a property of the
//! board rather than of the code, and `bench/` is where it gets answered. A test
//! that required a shuffle to win would be asserting a hope.

use std::path::PathBuf;

use fr_io::specctra;
use fr_router::batch::budget::BudgetPolicy;
use fr_router::batch::portfolio::{self, ArmScore, PortfolioConfig, WorkOrder};
use fr_router::batch::BatchRouter;
use fr_router::{BoardScorer, CheckScorer};
use fr_rules::router_settings::{BoardAspectRatio, RouterSettings};

/// The fixture corpus, per `bench/fetch_fixtures.sh`.
fn fixture(name: &str) -> Option<PathBuf> {
    let dir = std::env::var_os("FR_FIXTURE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/freerouting-upstream/fixtures"));
    let path = dir.join(name);
    if path.exists() {
        return Some(path);
    }
    eprintln!(
        "skipping: {} not found — run bench/fetch_fixtures.sh or set FR_FIXTURE_DIR",
        path.display()
    );
    None
}

struct Design {
    design: specctra::ImportedDesign,
    settings: RouterSettings,
    board_units_per_mm: f64,
}

fn load(name: &str) -> Option<Design> {
    let bytes = std::fs::read(fixture(name)?).expect("the fixture is readable");
    let design = specctra::import(&bytes).expect("the fixture imports");
    let mut settings = RouterSettings::default();
    settings.set_layer_count(design.rules.layer_structure.len());
    settings.apply_board_specific_optimizations(
        BoardAspectRatio::from_bounding_box(
            f64::from(design.bounds.width()),
            f64::from(design.bounds.height()),
        ),
        &design.rules.layer_structure,
    );
    let um_per_board_unit = specctra::Unit::scale(1.0, design.unit, specctra::Unit::Um)
        / f64::from(design.resolution.max(1));
    let board_units_per_mm = if um_per_board_unit > 0.0 {
        1000.0 / um_per_board_unit
    } else {
        1.0
    };
    Some(Design {
        design,
        settings,
        board_units_per_mm,
    })
}

/// The budget every race here runs under, and the reason it is not the default
/// one.
///
/// Two of these tests compare **two independent runs** of the same board and
/// require them to agree. That is a property of
/// [`BudgetPolicy::deterministic`] and of nothing else: `ARCHITECTURE.md`
/// "Budgets" keeps upstream's wall-clock constants in the default mode — the
/// per-connection `TimeLimit` and, since the M3 gate,
/// `TIME_LIMIT_TO_PREVENT_ENDLESS_LOOP` on the per-connection pull-tight — and
/// says `--deterministic` is what "replaces wall checks with effort-based
/// virtual time … so byte-reproducibility holds". Under a wall budget the two
/// runs legitimately differ, and on `Issue039` in a debug build they measurably
/// do (68 incompletes against 67 on arm 2), because the arms are racing on N
/// cores and a loaded core buys fewer pull-tight rounds.
///
/// What is under test here is the race's own machinery — arm order as a
/// function of arm index, the tie rule, "never worse than arm 0" — none of
/// which is about the clock. So the clock is taken out.
fn budget() -> BudgetPolicy {
    BudgetPolicy::deterministic()
}

fn config(loaded: &Design, arms: usize, max_passes: i32) -> PortfolioConfig {
    PortfolioConfig {
        arms,
        max_passes,
        budget: budget(),
        fanout: false,
        board_units_per_mm: loaded.board_units_per_mm,
    }
}

fn judge(loaded: &Design) -> ArmScore {
    let mut scorer = CheckScorer::new(&loaded.design.rules, loaded.board_units_per_mm);
    let score = scorer.score(&loaded.design.board);
    ArmScore {
        incomplete_count: score.incomplete_count,
        normalized: score.normalized,
        violation_count: score.violation_count,
    }
}

const FIXTURE: &str = "Issue039-bug-design.dsn";
const PASSES: i32 = 1;

// ---------------------------------------------------------------------------

/// A portfolio of one is the plain run — same board, no clone, no thread.
#[test]
#[ignore = "long test: routes Issue039 twice in a debug build; run with `cargo test -- --include-ignored` or ci/run-local.sh --long (FR_LONG_TESTS=1)"]
fn a_portfolio_of_one_is_the_default_run() {
    let (Some(mut raced), Some(mut plain)) = (load(FIXTURE), load(FIXTURE)) else {
        return;
    };

    let cfg = config(&raced, 1, PASSES);
    let outcome = portfolio::race(
        &mut raced.design.board,
        &raced.design.rules,
        &raced.settings,
        cfg,
    );
    assert_eq!(outcome.arms.len(), 1);
    assert_eq!(outcome.winner, 0);
    assert_eq!(outcome.arms[0].order, WorkOrder::Scan);

    let mut router = BatchRouter::new(&plain.design.rules, &plain.settings)
        .with_board_units_per_mm(plain.board_units_per_mm)
        // The same budget the race ran under, or the comparison is between two
        // different stopping rules rather than between two work orders.
        .with_budget(budget());
    router.run(&mut plain.design.board, PASSES);

    assert_eq!(judge(&raced), judge(&plain), "a race of one is not a race");
}

/// The property `--portfolio` rests on: the race is never worse than arm 0, and
/// arm 0 is the plain run.
#[test]
#[ignore = "long test: routes Issue039 on 3 arms in a debug build; run with `cargo test -- --include-ignored` or ci/run-local.sh --long (FR_LONG_TESTS=1)"]
fn the_race_never_returns_a_board_worse_than_the_default_order() {
    let Some(mut loaded) = load(FIXTURE) else {
        return;
    };
    let cfg = config(&loaded, 3, PASSES);
    let outcome = portfolio::race(
        &mut loaded.design.board,
        &loaded.design.rules,
        &loaded.settings,
        cfg,
    );
    assert_eq!(outcome.arms.len(), 3);
    assert_eq!(outcome.arms[0].order, WorkOrder::Scan);

    let default_arm = &outcome.arms[0].score;
    let winner = &outcome.winning_arm().score;
    assert!(
        !default_arm.beats(winner),
        "the default arm scored {default_arm:?} and the race adopted {winner:?}"
    );
    // The board that came back is the winner's, judged independently.
    assert_eq!(&judge(&loaded), winner);
    // ... and the race is only interesting if the arms disagreed about
    // something. They may legitimately all tie, so this is a note, not an
    // assertion.
    let scores: Vec<usize> = outcome
        .arms
        .iter()
        .map(|arm| arm.score.incomplete_count)
        .collect();
    eprintln!(
        "{FIXTURE}: incompletes per arm {scores:?}, winner {}",
        outcome.winner
    );
}

/// Two races leave the same board: an arm's order is a function of its index,
/// and a tie goes to the lowest index, so nothing about scheduling gets in.
#[test]
#[ignore = "long test: routes Issue039 twice on 3 arms in a debug build; run with `cargo test -- --include-ignored` or ci/run-local.sh --long (FR_LONG_TESTS=1)"]
fn a_race_is_reproducible() {
    let (Some(mut first), Some(mut second)) = (load(FIXTURE), load(FIXTURE)) else {
        return;
    };
    let (cfg_a, cfg_b) = (config(&first, 3, PASSES), config(&second, 3, PASSES));
    let a = portfolio::race(
        &mut first.design.board,
        &first.design.rules,
        &first.settings,
        cfg_a,
    );
    let b = portfolio::race(
        &mut second.design.board,
        &second.design.rules,
        &second.settings,
        cfg_b,
    );
    assert_eq!(a.winner, b.winner);
    for (x, y) in a.arms.iter().zip(&b.arms) {
        assert_eq!(x.order, y.order, "arm {}", x.index);
        assert_eq!(x.score, y.score, "arm {}", x.index);
    }
    assert_eq!(judge(&first), judge(&second));
}
