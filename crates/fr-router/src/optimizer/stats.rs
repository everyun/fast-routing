//! The board measurements the optimizer decides on — the slice of
//! `core/scoring/BoardStatistics.java` that `BatchOptimizer` actually reads.
//!
//! | this module | upstream |
//! |---|---|
//! | [`via_count`] | `BoardStatistics.items.viaCount` (`:137-143`) |
//! | [`trace_length`] | `traces.totalLength` — `Σ PolylineTrace.get_length()` (`:171-176`) |
//! | [`weighted_trace_length`] | `traces.totalWeightedLength` (`:222-241`) |
//! | [`bend_count`] | `bends.totalCount` (`:252-268`) |
//! | [`board_key`] | the four numbers `opt_route_item` compares (`:315-321`) |
//! | [`PassScore`] / [`pass_score`] | `getNormalizedScore` (`:552-586`) |
//!
//! # Why the scores are not implemented here
//!
//! Because there must be exactly one of them. [`crate::batch::scoring`] is the
//! home: [`CheckScorer`] assembles [`fr_check::BoardStats`] and hands it to
//! [`fr_check::score()`], and [`trace_geometry`] is the one walk over the items
//! that produces the four per-item sums. Everything in this module is a view of
//! those two — the optimizer's pass loop stops on a *score* delta
//! (`BatchOptimizer.java:157-177`), so it has to ask exactly the question the
//! batch log asks, and a second transcription of `BoardStatistics` living here
//! could only drift from it.
//!
//! # Cost
//!
//! [`pass_score`] is a whole-board DRC and is called three times per optimizer
//! pass — before the pass, and before/after in the batch loop — exactly where
//! the release calls `board.get_statistics()`. [`board_key`] is the per-*item*
//! measurement and deliberately does **not** run the DRC: upstream's per-item
//! statistics are built with `new BoardStatistics(routingBoard, null, false)`,
//! clearance checks off (`BatchOptimizer.java:315`). It is one item walk, not
//! the three the four sums used to cost.

use fr_board::board::Board;
use fr_check::{Score, ScoreWeights};
use fr_rules::board_rules::BoardRules;
use fr_rules::router_settings::ScoringSettings;

use crate::batch::scoring::{trace_geometry, CheckScorer};

use super::improvement::BoardKey;

/// `BoardStatistics.items.viaCount`.
pub fn via_count(board: &Board) -> usize {
    trace_geometry(board).via_count as usize
}

/// `BoardStatistics.traces.totalLength` — every trace, whatever its fixed
/// state, summed in raw board units (`:171-176`).
pub fn trace_length(board: &Board) -> f64 {
    trace_geometry(board).trace_length
}

/// `BoardStatistics.traces.totalWeightedLength` (`:222-241`) — see
/// [`TraceGeometry::weighted_trace_length`](crate::batch::scoring::TraceGeometry::weighted_trace_length).
pub fn weighted_trace_length(board: &Board) -> f64 {
    trace_geometry(board).weighted_trace_length
}

/// `BoardStatistics.bends.totalCount` — `Σ max(0, corner_count - 2)`; a
/// two-corner trace is straight and bends nowhere (`:252-268`).
pub fn bend_count(board: &Board) -> usize {
    trace_geometry(board).bend_count as usize
}

/// The four numbers one item's reroute is judged by.
///
/// `incomplete_count` is [`crate::batch::incomplete_count`], the ratsnest count
/// upstream reaches through `DesignRulesChecker.calculateAllIncompletes`
/// (`BatchOptimizer.java:459-463`); it needs `&mut Board` because the
/// connectivity index is rebuilt lazily.
pub fn board_key(board: &mut Board) -> BoardKey {
    let geometry = trace_geometry(board);
    BoardKey {
        incomplete_count: crate::batch::incomplete_count(board),
        via_count: geometry.via_count as usize,
        trace_length: geometry.trace_length,
        weighted_trace_length: geometry.weighted_trace_length,
    }
}

/// A pass boundary's score, with the counts it was computed from kept so a log
/// line or a test can attribute a regression.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PassScore {
    /// `getNormalizedScore`, in `[0, 1000]`.
    pub normalized: f64,
    /// `connections.incompleteCount`.
    pub incomplete_count: usize,
    /// `clearanceViolations.totalCount`.
    pub violation_count: usize,
    /// The full referee verdict, for callers that want the terms apart.
    pub score: Score,
}

/// `RoutingBoard.get_statistics().getNormalizedScore(scoring)` — the number
/// `BatchOptimizer.runBatchLoop` compares across passes (`:137`, `:157`).
///
/// `board_units_per_mm` converts the raw trace length into the millimetres the
/// cost term is denominated in; it is the design's own resolution, which
/// `fr_board::Board` does not carry. See
/// [`BatchOptimizer::board_units_per_mm`](super::BatchOptimizer::board_units_per_mm).
pub fn pass_score(
    board: &mut Board,
    rules: &BoardRules,
    scoring: &ScoringSettings,
    board_units_per_mm: f64,
) -> PassScore {
    let scorer = CheckScorer::new(rules, board_units_per_mm).with_weights(weights_from(scoring));
    // One item walk, reused for the score inputs — `board_key` runs the same
    // walk, and upstream's `BoardStatistics` constructor is likewise one pass.
    let stats = scorer.stats_from(board, &trace_geometry(board));
    let verdict = scorer.judge(&stats);
    PassScore {
        normalized: verdict.normalized,
        incomplete_count: verdict.incomplete_count,
        violation_count: verdict.violation_count,
        score: verdict.score,
    }
}

/// The referee's weights, read off the job's own `router.scoring.*` block
/// rather than hardcoded, because `-oit`-style overrides move them.
///
/// The batch loop's [`weights_from`](crate::batch::scoring::weights_from) is the
/// same function over the same block; both stay so each stage's docs can name
/// the one it reads.
pub fn weights_from(scoring: &ScoringSettings) -> ScoreWeights {
    crate::batch::scoring::weights_from(scoring)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestFixture, TEST_CLEARANCE, TEST_HALF_WIDTH, TEST_NET};
    use fr_board::item::FixedState;
    use fr_board::net_nos::NetNos;
    use fr_geometry::planar::IntPoint;

    fn board_with_trace(fixed: FixedState) -> TestFixture {
        let mut fixture = TestFixture::two_layer_with_pads();
        fixture
            .board
            .insert_trace_points(
                &[IntPoint::new(-2000, 0), IntPoint::new(-1000, 0)],
                0,
                TEST_HALF_WIDTH,
                NetNos::of(TEST_NET),
                1,
                fixed,
                None,
            )
            .expect("the wire inserts");
        fixture
    }

    #[test]
    fn the_raw_length_is_the_polyline_sum() {
        let fixture = board_with_trace(FixedState::Unfixed);
        assert!((trace_length(&fixture.board) - 1_000.0).abs() < 1e-6);
    }

    #[test]
    fn the_weight_is_half_width_plus_clearance() {
        let fixture = board_with_trace(FixedState::Unfixed);
        let expected = 1_000.0 * f64::from(TEST_HALF_WIDTH + TEST_CLEARANCE);
        assert!(
            (weighted_trace_length(&fixture.board) - expected).abs() < 1e-6,
            "{}",
            weighted_trace_length(&fixture.board)
        );
    }

    #[test]
    fn a_shove_fixed_trace_weighs_half() {
        let unfixed = weighted_trace_length(&board_with_trace(FixedState::Unfixed).board);
        let shove = weighted_trace_length(&board_with_trace(FixedState::ShoveFixed).board);
        assert!((shove * 2.0 - unfixed).abs() < 1e-6);
    }

    #[test]
    fn user_fixed_copper_is_outside_the_weighted_sum_but_inside_the_raw_one() {
        let fixture = board_with_trace(FixedState::UserFixed);
        assert!((trace_length(&fixture.board) - 1_000.0).abs() < 1e-6);
        assert_eq!(weighted_trace_length(&fixture.board), 0.0);
    }

    #[test]
    fn a_straight_two_corner_trace_has_no_bends() {
        let fixture = board_with_trace(FixedState::Unfixed);
        assert_eq!(bend_count(&fixture.board), 0);
    }

    /// The dedup's contract: the optimizer's pass score is the batch loop's
    /// score, to the bit. Two transcriptions of `getNormalizedScore` that agree
    /// today can drift; one cannot.
    #[test]
    fn the_pass_score_is_the_batch_loops_score() {
        use crate::batch::scoring::{BoardScorer, CheckScorer};

        let mut fixture = board_with_trace(FixedState::Unfixed);
        let rules = fixture.rules.clone();
        let scoring = fixture.settings.scoring.clone();

        let pass = pass_score(&mut fixture.board, &rules, &scoring, 10_000.0);
        let mut scorer = CheckScorer::new(&rules, 10_000.0).with_weights(weights_from(&scoring));
        let batch = scorer.score(&fixture.board);

        assert_eq!(pass.normalized, batch.normalized);
        assert_eq!(pass.incomplete_count, batch.incomplete_count);
        assert_eq!(pass.violation_count, batch.violation_count);
        assert_eq!(pass.score, batch.score);
    }

    /// [`board_key`] and the score read the same item walk, so the numbers a
    /// reroute is judged by are the ones the score was computed from.
    #[test]
    fn the_board_key_agrees_with_the_shared_item_walk() {
        use crate::batch::scoring::trace_geometry;

        let mut fixture = board_with_trace(FixedState::Unfixed);
        let geometry = trace_geometry(&fixture.board);
        let key = board_key(&mut fixture.board);
        assert_eq!(key.via_count, geometry.via_count as usize);
        assert_eq!(key.trace_length, geometry.trace_length);
        assert_eq!(key.weighted_trace_length, geometry.weighted_trace_length);
        assert_eq!(bend_count(&fixture.board), geometry.bend_count as usize);
    }

    #[test]
    fn an_unrouted_board_scores_below_a_routed_one() {
        let mut open = TestFixture::two_layer_with_pads();
        let open_score = pass_score(
            &mut open.board,
            &open.rules,
            &open.settings.scoring,
            10_000.0,
        );
        assert_eq!(open_score.incomplete_count, 1);

        let mut joined = TestFixture::two_layer_with_pads();
        let centers = joined.pad_centers();
        joined
            .board
            .insert_trace_points(
                &centers,
                0,
                TEST_HALF_WIDTH,
                NetNos::of(TEST_NET),
                1,
                FixedState::Unfixed,
                None,
            )
            .expect("the straight wire inserts");
        let joined_score = pass_score(
            &mut joined.board,
            &joined.rules,
            &joined.settings.scoring,
            10_000.0,
        );
        assert_eq!(joined_score.incomplete_count, 0);
        assert!(
            joined_score.normalized > open_score.normalized,
            "{joined_score:?} vs {open_score:?}"
        );
    }
}
