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
//! # Why the scores live here and not in `fr-check`
//!
//! They do not: [`pass_score`] delegates the arithmetic to
//! [`fr_check::score()`] and the two counts it cannot derive from the item list —
//! `max_connections` and the clearance-violation total — to
//! [`fr_check::board_connectivity`] and [`fr_check::check_clearance`]. The
//! referee is the only thing in this workspace allowed to define what a
//! violation is, and the optimizer's pass loop stops on a *score* delta
//! (`BatchOptimizer.java:157-177`), so it has to ask the referee the same
//! question the batch log asks.
//!
//! # Cost
//!
//! [`pass_score`] is a whole-board DRC and is called three times per optimizer
//! pass — before the pass, and before/after in the batch loop — exactly where
//! the release calls `board.get_statistics()`. [`board_key`] is the per-*item*
//! measurement and deliberately does **not** run the DRC: upstream's per-item
//! statistics are built with `new BoardStatistics(routingBoard, null, false)`,
//! clearance checks off (`BatchOptimizer.java:315`).

use fr_board::board::Board;
use fr_board::item::ItemKind;
use fr_board::ClearanceSource;
use fr_check::{
    board_connectivity, check_clearance, score, shape_store, BoardStats, Score, ScoreWeights,
    StoreView,
};
use fr_rules::board_rules::BoardRules;
use fr_rules::router_settings::ScoringSettings;

use super::improvement::BoardKey;

/// `BoardStatistics.items.viaCount`.
pub fn via_count(board: &Board) -> usize {
    board
        .store()
        .iter()
        .filter(|(_, item)| matches!(item.kind, ItemKind::Via(_)))
        .count()
}

/// `BoardStatistics.traces.totalLength` — every trace, whatever its fixed
/// state, summed in raw board units (`:171-176`).
pub fn trace_length(board: &Board) -> f64 {
    board
        .store()
        .iter()
        .filter_map(|(_, item)| match &item.kind {
            ItemKind::Trace(trace) => Some(trace.polyline.length_total()),
            _ => None,
        })
        .sum()
}

/// `BoardStatistics.traces.totalWeightedLength` (`:222-241`):
///
/// ```text
/// Σ over UNFIXED and SHOVE_FIXED traces of
///     length * (half_width + clearance(traceClass, defaultClass, layer))
/// halved for SHOVE_FIXED, "to produce less violations with pin exit directions"
/// ```
///
/// User-fixed and system-fixed copper is excluded outright: the optimizer
/// cannot move it, so counting it would only add a constant that hides the
/// change it can make.
pub fn weighted_trace_length(board: &Board) -> f64 {
    let default_class = BoardRules::default_clearance_class() as i32;
    let mut total = 0.0;
    for (_, item) in board.store().iter() {
        let ItemKind::Trace(trace) = &item.kind else {
            continue;
        };
        if item.is_user_fixed() {
            continue;
        }
        let clearance = board
            .clearances()
            .value(item.clearance_class, default_class, trace.layer);
        let weight = f64::from(trace.half_width) + f64::from(clearance);
        let mut weighted = trace.polyline.length_total() * weight;
        if item.fixed_state.is_shove_fixed() {
            weighted /= 2.0;
        }
        total += weighted;
    }
    total
}

/// `BoardStatistics.bends.totalCount` — `Σ max(0, corner_count - 2)`; a
/// two-corner trace is straight and bends nowhere (`:252-268`).
pub fn bend_count(board: &Board) -> usize {
    board
        .store()
        .iter()
        .filter_map(|(_, item)| match &item.kind {
            ItemKind::Trace(trace) => Some(trace.corner_count().saturating_sub(2)),
            _ => None,
        })
        .sum()
}

/// The four numbers one item's reroute is judged by.
///
/// `incomplete_count` is [`crate::batch::incomplete_count`], the ratsnest count
/// upstream reaches through `DesignRulesChecker.calculateAllIncompletes`
/// (`BatchOptimizer.java:459-463`); it needs `&mut Board` because the
/// connectivity index is rebuilt lazily.
pub fn board_key(board: &mut Board) -> BoardKey {
    BoardKey {
        incomplete_count: crate::batch::incomplete_count(board),
        via_count: via_count(board),
        trace_length: trace_length(board),
        weighted_trace_length: weighted_trace_length(board),
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
    let layer_count = board.layers().len();
    let shapes = shape_store(board);
    let view = StoreView::new(board.store(), &shapes, &rules.clearance_matrix, layer_count);
    let violations = check_clearance(&view);
    let connectivity = board_connectivity(&view);

    let scale = if board_units_per_mm > 0.0 {
        1_000.0 / board_units_per_mm
    } else {
        1.0
    };
    let stats = BoardStats {
        max_connections: connectivity.max_connections as u64,
        incomplete_count: connectivity.incomplete_count as u64,
        clearance_violation_count: violations.len() as u64,
        bend_count: bend_count(board) as u64,
        trace_length_um: (trace_length(board) * scale).round() as u64,
        via_count: via_count(board) as u64,
        // `BoardStatistics.calculateScore:566` charges every via at
        // `viaCosts`; `planeViaCosts` is a *router* cost and never a scoring
        // one (`batch-orchestration.md` §6.1, row 76).
        plane_via_count: 0,
    };
    let weights = weights_from(scoring);
    let score = score(&stats, &weights);
    PassScore {
        normalized: score.normalized,
        incomplete_count: connectivity.incomplete_count,
        violation_count: violations.len(),
        score,
    }
}

/// The referee's weights, read off the job's own `router.scoring.*` block
/// rather than hardcoded, because `-oit`-style overrides move them.
pub fn weights_from(scoring: &ScoringSettings) -> ScoreWeights {
    ScoreWeights {
        unrouted_net_penalty: f64::from(scoring.unrouted_net_penalty),
        clearance_violation_penalty: f64::from(scoring.clearance_violation_penalty),
        bend_penalty: f64::from(scoring.bend_penalty),
        trace_cost_per_mm: scoring
            .default_preferred_direction_trace_cost
            .unwrap_or(1.0),
        via_cost: f64::from(scoring.via_costs()),
        plane_via_cost: f64::from(scoring.plane_via_costs()),
    }
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
