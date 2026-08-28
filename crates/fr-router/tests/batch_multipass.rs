//! The batch loop over a real board: `Issue026-J2_reference` at `-mp 8`.
//!
//! The unit tests in `batch::schedule` prove the *scheduler* against a scripted
//! pass sequence — the stop rules are a function of the score sequence and need
//! no maze. This proves the two things a script cannot: that the loop's
//! contract holds when the passes are real, and that the machinery it hangs on
//! `fr_board`'s state model (score-ranked snapshots, board restore, the
//! invalidation callback) survives an actual routed board.
//!
//! # What is asserted, and what is deliberately not
//!
//! Asserted:
//!
//! * the run terminates within the pass cap and reports a stop reason;
//! * **the final board is never worse than the best board the run ever held** —
//!   `BatchAutorouter.java:1141-1159`, the final best-board restore. This is the
//!   loop's one hard guarantee and the reason the snapshot store exists;
//! * multi-pass ends with no more incompletes than one pass does, on the same
//!   board, with the same binary (the A/B ARCHITECTURE asks for);
//! * the referee's incomplete count agrees with the loop's.
//!
//! Not asserted: *monotone* non-increase of incompletes pass by pass. It is not
//! a property of the algorithm — a pass rips up copper at a rising budget and
//! may legitimately end worse than it started, which is precisely what the
//! restore machinery exists to undo, and the restore check does not even arm
//! before pass 8. Asserting it would encode a wish, not the contract.
//!
//! `0 unrouted` on this fixture is the M2c gate target (Java does it in under
//! 4 s); it is not asserted here because this crate's tests must not go red on
//! a shove/insert regression they do not own. The run prints its numbers, and
//! `bench/run_bench.py` is what judges them.

use std::path::PathBuf;

use fr_io::specctra;
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
    let path = fixture(name)?;
    let bytes = std::fs::read(&path).expect("the fixture is readable");
    let design = specctra::import(&bytes).expect("the fixture imports");

    // The same settings the pipeline builds: without
    // `apply_board_specific_optimizations` every layer costs 1.0/1.0 and the
    // maze has no preferred direction.
    let mut settings = RouterSettings::default();
    settings.set_layer_count(design.rules.layer_structure.len());
    settings.apply_board_specific_optimizations(
        BoardAspectRatio::from_bounding_box(
            f64::from(design.bounds.width()),
            f64::from(design.bounds.height()),
        ),
        &design.rules.layer_structure,
    );

    // `Unit::scale(1, unit, UM) / resolution` micrometres per board unit, inverted.
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

/// Routes `name` with `max_passes` and returns `(incompletes, score)` plus the
/// loop's own outcome.
fn route(name: &str, max_passes: i32) -> Option<(fr_router::RunOutcome, usize, f64)> {
    let mut loaded = load(name)?;
    let mut router = BatchRouter::new(&loaded.design.rules, &loaded.settings)
        .with_board_units_per_mm(loaded.board_units_per_mm);
    let outcome = router.run(&mut loaded.design.board, max_passes);

    // The referee re-derives the numbers from the finished board, independently
    // of whatever the loop believes.
    let mut referee = CheckScorer::new(&loaded.design.rules, loaded.board_units_per_mm);
    let judged = referee.score(&loaded.design.board);
    Some((outcome, judged.incomplete_count, judged.normalized))
}

#[test]
fn issue026_runs_eight_passes_and_never_ends_below_its_best_board() {
    let Some((outcome, incompletes, score)) =
        route("Issue026-J2_reference.dsn", 8)
    else {
        return;
    };

    eprintln!(
        "Issue026 -mp 8: {} passes, stop {:?}, {} restores, best-board restore {}, \
         final score {:.2}, {} incomplete",
        outcome.passes.len(),
        outcome.stop_reason,
        outcome.restores,
        outcome.best_board_restored,
        outcome.final_score,
        incompletes,
    );
    for pass in &outcome.passes {
        eprintln!(
            "  pass {:>2}: ripup {:>4}, queued {:>3}, routed {:>3}, failed {:>3}, \
             ripped {:>3}, incomplete {:>3}",
            pass.pass_no,
            pass.ripup_costs,
            pass.queued,
            pass.routed,
            pass.not_routed,
            pass.ripped_item_count,
            pass.incomplete_count,
        );
    }

    assert!(!outcome.passes.is_empty(), "the loop must run at least once");
    assert!(outcome.passes.len() <= 8, "the pass cap holds");
    assert!(outcome.stop_reason.is_some(), "every exit has a reason");

    // `:1280` — the ripup budget of pass n is `start_ripup_costs * n`, on a real
    // board as much as on a scripted one.
    for (index, pass) in outcome.passes.iter().enumerate() {
        assert_eq!(pass.pass_no, index as i32 + 1);
        assert_eq!(pass.ripup_costs, 100 * pass.pass_no, "{pass:?}");
    }

    // `:1141-1159` — the loop's one hard guarantee.
    assert!(
        outcome.final_score + 1e-9 >= outcome.best_history_score,
        "final {} < best retained {}",
        outcome.final_score,
        outcome.best_history_score
    );

    // The loop and the referee are different code over the same board; they
    // must not disagree in the router's favour.
    assert_eq!(
        outcome.incomplete_count, incompletes,
        "the loop's count must be the referee's"
    );
    assert!(score > 0.0, "a routed board scores above zero");
}

/// The A/B the pass loop exists to win: more passes must not leave more
/// connections open than one pass does. Same board, same build, so the only
/// variable is the schedule.
#[test]
fn more_passes_do_not_leave_more_incompletes_than_one() {
    let Some((_, one_pass, _)) = route("Issue026-J2_reference.dsn", 1) else {
        return;
    };
    let Some((_, eight_passes, _)) = route("Issue026-J2_reference.dsn", 8) else {
        return;
    };
    eprintln!("Issue026: -mp 1 -> {one_pass} incomplete, -mp 8 -> {eight_passes} incomplete");
    assert!(
        eight_passes <= one_pass,
        "-mp 8 left {eight_passes} incomplete against -mp 1's {one_pass}"
    );
}

/// A pass cap of 1 keeps the M2a divergence (the in-pass requeue); anything
/// above it runs upstream's no-retry-inside-a-pass rule
/// (`docs/PARITY-DIVERGENCES.md`, `RequeuePolicy::SinglePassOnly`).
#[test]
fn the_requeue_divergence_is_confined_to_single_pass_runs() {
    let Some(mut loaded) = load("Issue026-J2_reference.dsn") else {
        return;
    };
    let mut router = BatchRouter::new(&loaded.design.rules, &loaded.settings);
    let single = router.run(&mut loaded.design.board, 1);
    assert!(router.requeue_active(), "-mp 1 requeues");
    let requeued_at_one: usize = single.passes.iter().map(|pass| pass.requeued).sum();

    let Some(mut loaded) = load("Issue026-J2_reference.dsn") else {
        return;
    };
    let mut router = BatchRouter::new(&loaded.design.rules, &loaded.settings);
    let multi = router.run(&mut loaded.design.board, 4);
    assert!(!router.requeue_active(), "-mp 4 runs upstream's rule");
    assert_eq!(
        multi.passes.iter().map(|pass| pass.requeued).sum::<usize>(),
        0,
        "a multi-pass run must not requeue inside a pass"
    );
    eprintln!("Issue026 -mp 1 requeued {requeued_at_one} connections in-pass");
}
