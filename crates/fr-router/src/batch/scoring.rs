//! The number every stop rule reads: `BoardStatistics.getNormalizedScore`.
//!
//! `core/scoring/BoardStatistics.java:552-586` (conformance appendix row 75 —
//! the only scoring formula that ships in 2.3.0), already implemented as
//! [`fr_check::score()`]. This module is the batch loop's adapter to it: a
//! [`BoardScorer`] trait so the loop can be driven by a scripted score in tests,
//! and [`CheckScorer`], the production implementation that assembles
//! [`fr_check::BoardStats`] from a [`Board`] and hands it to the referee.
//!
//! It is also the **single home** of that assembly. The optimizer decides on the
//! same number ([`BatchOptimizer::run_batch_loop`](crate::optimizer::BatchOptimizer::run_batch_loop)
//! compares `getNormalizedScore` across passes) and on the same board
//! measurements ([`board_key`](crate::optimizer::stats::board_key)), so
//! [`optimizer::stats`](crate::optimizer::stats) delegates here rather than
//! keeping a second transcription of `BoardStatistics` that could drift.
//! [`trace_geometry`] is the shared per-item walk: one pass over the store for
//! the four sums both callers need, including the
//! [`TraceGeometry::weighted_trace_length`] only the optimizer reads and the raw
//! [`TraceGeometry::via_count`] only it compares.
//!
//! Scoring through the referee rather than through a router-local
//! approximation is deliberate: the loop's improvement and stagnation decisions
//! must be made on the same number the gate is judged by, or the router can
//! "improve" its way into a worse board. It is also what upstream does — the
//! batch loop and the final report both call `getNormalizedScore`.
//!
//! # Cost
//!
//! One score is a full clearance DRC plus a full ratsnest, i.e. O(board). The
//! loop calls it **once per pass** (plus once per restore and once per fanout
//! recovery), and carries the result forward as the next pass's pre-pass score
//! rather than recomputing it — upstream computes it three times per pass, one
//! of which (`BatchAutorouter.java:943`) it then throws away unread
//! (conformance appendix row 71).

use fr_board::board::Board;
use fr_board::item::ItemKind;
use fr_board::ClearanceSource;
use fr_check::{
    board_connectivity, check_clearance, score, shape_store, BoardStats, Score, ScoreWeights,
    StoreView,
};
use fr_rules::board_rules::BoardRules;
use fr_rules::router_settings::ScoringSettings;

/// The verdict on a board with nothing to connect — `getNormalizedScore`'s
/// `maximum <= 0` guard, which is defined to score `0` rather than `NaN`
/// (`fr_check::score`, §7.15). [`BoardScore::default`]'s value.
const NO_SCORE: Score = Score {
    maximum: 0.0,
    penalties: 0.0,
    costs: 0.0,
    raw: 0.0,
    normalized: 0.0,
};

/// A scored board state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoardScore {
    /// `getNormalizedScore` — `max(0, raw / maximum) · 1000`, or `0` when the
    /// board has no connection to make.
    pub normalized: f64,
    /// Incomplete connections, the `N` of the upstream log line.
    pub incomplete_count: usize,
    /// Deduplicated clearance violations, the `M`.
    pub violation_count: usize,
    /// `Σ_nets max(0, #(Pin | ConductionArea) − 1)`.
    pub max_connections: usize,
    /// The referee's full verdict, for a caller that wants the terms apart.
    pub score: Score,
}

impl Default for BoardScore {
    fn default() -> BoardScore {
        BoardScore {
            normalized: 0.0,
            incomplete_count: 0,
            violation_count: 0,
            max_connections: 0,
            score: NO_SCORE,
        }
    }
}

/// The per-item sums of `core/scoring/BoardStatistics`, in **raw board units**.
///
/// One walk over the store for all four, because both consumers want more than
/// one of them and the walk is the expensive part.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TraceGeometry {
    /// `bends.totalCount` — `Σ max(0, corner_count − 2)`; a two-corner trace is
    /// straight and bends nowhere (`:252-268`).
    pub bend_count: u64,
    /// `items.viaCount` (`:137-143`).
    pub via_count: u64,
    /// `traces.totalLength` — every trace, whatever its fixed state (`:171-176`).
    pub trace_length: f64,
    /// `traces.totalWeightedLength` (`:222-241`):
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
    pub weighted_trace_length: f64,
}

/// [`TraceGeometry`] of one board.
pub fn trace_geometry(board: &Board) -> TraceGeometry {
    let default_class = BoardRules::default_clearance_class() as i32;
    let mut geometry = TraceGeometry::default();
    for (_, item) in board.store().iter() {
        match &item.kind {
            ItemKind::Trace(trace) => {
                let corners = trace.corner_count();
                if corners >= 3 {
                    geometry.bend_count += (corners - 2) as u64;
                }
                let length = trace.polyline.length_total();
                geometry.trace_length += length;
                if item.is_user_fixed() {
                    continue;
                }
                let clearance =
                    board
                        .clearances()
                        .value(item.clearance_class, default_class, trace.layer);
                let mut weighted = length * (f64::from(trace.half_width) + f64::from(clearance));
                if item.fixed_state.is_shove_fixed() {
                    weighted /= 2.0;
                }
                geometry.weighted_trace_length += weighted;
            }
            ItemKind::Via(_) => geometry.via_count += 1,
            _ => {}
        }
    }
    geometry
}

/// What the batch loop needs from a board to decide whether it improved.
///
/// A trait rather than a function so the pass-loop tests can drive the stop
/// machinery from a scripted score without routing anything — the scheduler's
/// behaviour is a function of the score sequence, and that is what those tests
/// are about.
pub trait BoardScorer {
    /// Scores the board as it stands.
    fn score(&mut self, board: &Board) -> BoardScore;
}

impl<S: BoardScorer + ?Sized> BoardScorer for &mut S {
    fn score(&mut self, board: &Board) -> BoardScore {
        (**self).score(board)
    }
}

/// The referee's weights, read off the job's own `router.scoring.*` block
/// rather than hardcoded, because a settings override moves them.
///
/// The shipped values are `ScoreWeights::V2` (`DefaultSettings.java:27-67,
/// 145-151`, jar-verified).
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

/// The production scorer: `fr_check`'s DRC, ratsnest and v2 score.
#[derive(Debug, Clone, Copy)]
pub struct CheckScorer<'r> {
    rules: &'r BoardRules,
    weights: ScoreWeights,
    board_units_per_mm: f64,
}

impl<'r> CheckScorer<'r> {
    /// A scorer over these rules, with [`ScoreWeights::V2`].
    ///
    /// `board_units_per_mm` converts trace length to the millimetres the
    /// score's cost term is denominated in — upstream's
    /// `Unit.scale(1, boardUnit, MM) / max(resolution, 1)`
    /// (`BoardStatistics.java:177-186`), whose comment records that
    /// un-normalized KiCad nanometres used to force the score to 0. It is a
    /// property of the *design file*, not of the board, which is why it is a
    /// parameter: the importer knows it and nothing downstream can recover it.
    /// Getting it wrong moves the score by micro-points against a
    /// per-connection weight of five million, so it cannot change a topology
    /// decision — but it does change the printed number.
    pub fn new(rules: &'r BoardRules, board_units_per_mm: f64) -> CheckScorer<'r> {
        CheckScorer {
            rules,
            weights: ScoreWeights::V2,
            board_units_per_mm,
        }
    }

    /// Overrides the weights; see [`weights_from`]. Only [`ScoreWeights::V2`]
    /// ships in 2.3.0, the v1.9 set exists for reading historical logs.
    pub fn with_weights(mut self, weights: ScoreWeights) -> CheckScorer<'r> {
        self.weights = weights;
        self
    }

    /// The weights this scorer judges with.
    pub fn weights(&self) -> &ScoreWeights {
        &self.weights
    }

    /// Micrometres per board unit — the inverse of what the caller passed.
    fn um_per_board_unit(&self) -> f64 {
        if self.board_units_per_mm > 0.0 {
            1_000.0 / self.board_units_per_mm
        } else {
            1.0
        }
    }

    /// The score inputs, without scoring them.
    pub fn stats(&self, board: &Board) -> BoardStats {
        self.stats_from(board, &trace_geometry(board))
    }

    /// [`stats`](CheckScorer::stats) for a caller that already walked the items
    /// — the optimizer, which needs the raw sums for its improvement lattice.
    pub fn stats_from(&self, board: &Board, geometry: &TraceGeometry) -> BoardStats {
        let shapes = shape_store(board);
        let view = StoreView::new(
            board.store(),
            &shapes,
            &self.rules.clearance_matrix,
            board.layers().len(),
        );
        let violations = check_clearance(&view);
        let connectivity = board_connectivity(&view);

        BoardStats {
            max_connections: connectivity.max_connections as u64,
            incomplete_count: connectivity.incomplete_count as u64,
            clearance_violation_count: violations.len() as u64,
            bend_count: geometry.bend_count,
            trace_length_um: (geometry.trace_length * self.um_per_board_unit()).round() as u64,
            via_count: geometry.via_count,
            // Row 76: `planeViaCosts` is a *router* cost, never a scoring one —
            // upstream charges every via at `viaCosts` here.
            plane_via_count: 0,
        }
    }

    /// The verdict for a set of inputs this scorer already assembled.
    pub fn judge(&self, stats: &BoardStats) -> BoardScore {
        let score = score(stats, &self.weights);
        BoardScore {
            normalized: score.normalized,
            incomplete_count: stats.incomplete_count as usize,
            violation_count: stats.clearance_violation_count as usize,
            max_connections: stats.max_connections as usize,
            score,
        }
    }
}

impl BoardScorer for CheckScorer<'_> {
    fn score(&mut self, board: &Board) -> BoardScore {
        let stats = self.stats(board);
        self.judge(&stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestFixture, TEST_HALF_WIDTH, TEST_NET};
    use fr_board::item::FixedState;
    use fr_board::net_nos::NetNos;

    #[test]
    fn an_unrouted_two_pad_net_scores_below_a_routed_one() {
        let mut fixture = TestFixture::two_layer_with_pads();
        let rules = fixture.rules.clone();
        let mut scorer = CheckScorer::new(&rules, 1.0);

        let before = scorer.score(&fixture.board);
        assert_eq!(before.incomplete_count, 1);
        assert_eq!(before.max_connections, 1);

        let centers = fixture.pad_centers();
        fixture
            .board
            .insert_trace_points(
                &[centers[0], centers[1]],
                0,
                TEST_HALF_WIDTH,
                NetNos::of(TEST_NET),
                1,
                FixedState::Unfixed,
                None,
            )
            .expect("the straight wire inserts");

        let after = scorer.score(&fixture.board);
        assert_eq!(after.incomplete_count, 0);
        assert!(
            after.normalized > before.normalized,
            "{after:?} must beat {before:?}"
        );
        // One connection made, no violations: the score is the maximum less the
        // trace's own length cost.
        assert!(after.normalized > 999.0, "{after:?}");
    }

    #[test]
    fn the_scorer_trait_is_object_safe_enough_to_borrow() {
        let fixture = TestFixture::two_layer_with_pads();
        let rules = fixture.rules.clone();
        let mut scorer = CheckScorer::new(&rules, 1.0);
        fn take(scorer: &mut impl BoardScorer, board: &Board) -> BoardScore {
            scorer.score(board)
        }
        let direct = scorer.score(&fixture.board);
        assert_eq!(take(&mut scorer, &fixture.board), direct);
    }
}
