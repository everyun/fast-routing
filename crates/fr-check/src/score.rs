//! The v2 board score — the number the batch log prints as
//! `"<score> (N unrouted and M violations)"`.
//!
//! `BoardStatistics.calculateScore` / `getNormalizedScore`
//! (`drc-connectivity.md` §3.4):
//!
//! ```text
//! maximum    = max_connections * unrouted_net_penalty
//! penalties  = incompletes * unrouted_net_penalty
//!            + violations  * clearance_violation_penalty
//!            + bends       * bend_penalty
//! costs      = trace_length_mm * trace_cost_per_mm
//!            + vias * via_cost + plane_vias * plane_via_cost
//! raw        = maximum - penalties - costs
//! normalized = maximum <= 0 ? 0 : max(0, raw / maximum) * 1000
//! ```
//!
//! # Do not compare across versions
//!
//! v1.9 used `unrouted = 1_000_000`, `clearance = 1_000`, `bend = 100`; v2 uses
//! `5_000_000`, `1_000_000`, `10`. A clearance violation went from a thousandth
//! of an unrouted connection to a fifth of one, and from a tenth of a bend to a
//! hundred thousand bends. `max_connections` was also redefined (§9 D4). Any
//! score comparison between the two trees is meaningless, which is why the
//! weights are a parameter and [`ScoreWeights::V2`] is named after the tree it
//! belongs to rather than being an anonymous `Default`.

/// The scoring weights, i.e. `settings/sources/DefaultSettings.java`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoreWeights {
    /// `DEFAULT_UNROUTED_NET_PENALTY`. Also the per-connection value of the
    /// maximum score, so it cancels out of the normalized score's leading term.
    pub unrouted_net_penalty: f64,
    /// `DEFAULT_CLEARANCE_VIOLATION_PENALTY`.
    pub clearance_violation_penalty: f64,
    /// `DEFAULT_BEND_PENALTY`.
    pub bend_penalty: f64,
    /// `DEFAULT_PREFERRED_DIRECTION_TRACE_COST`, per millimetre of trace.
    pub trace_cost_per_mm: f64,
    /// `DEFAULT_VIA_COSTS`.
    pub via_cost: f64,
    /// `DEFAULT_PLANE_VIA_COSTS`.
    pub plane_via_cost: f64,
}

impl ScoreWeights {
    /// The 2.3.0 defaults.
    pub const V2: ScoreWeights = ScoreWeights {
        unrouted_net_penalty: 5_000_000.0,
        clearance_violation_penalty: 1_000_000.0,
        bend_penalty: 10.0,
        trace_cost_per_mm: 1.0,
        via_cost: 50.0,
        plane_via_cost: 5.0,
    };

    /// The v1.9 weights, for reading historical logs only. Never mix them with
    /// [`ScoreWeights::V2`] numbers.
    pub const V19: ScoreWeights = ScoreWeights {
        unrouted_net_penalty: 1_000_000.0,
        clearance_violation_penalty: 1_000.0,
        bend_penalty: 100.0,
        trace_cost_per_mm: 1.0,
        via_cost: 50.0,
        plane_via_cost: 5.0,
    };
}

impl Default for ScoreWeights {
    fn default() -> Self {
        ScoreWeights::V2
    }
}

/// Everything the score is computed from.
///
/// Items 1–4 of the referee checklist (`drc-connectivity.md` §10.5):
/// `clearance_violation_count` comes from
/// [`check_clearance`](crate::check_clearance), `incomplete_count` and
/// `max_connections` from [`board_connectivity`](crate::board_connectivity), and
/// the last three are trivially derived from the item list by whoever owns the
/// board's unit resolution — `trace_length_mm` in particular must be converted
/// with the board's own resolution, not the hardcoded 10 000 units/mm the
/// upstream *log formatter* assumes (§1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BoardStats {
    /// `Σ_nets max(0, #(Pin | ConductionArea) - 1)`.
    pub max_connections: u64,
    /// The `N` in the log line.
    pub incomplete_count: u64,
    /// The `M` in the log line, deduplicated per `(min_id, max_id, layer)`.
    pub clearance_violation_count: u64,
    /// Corner count over all traces.
    pub bend_count: u64,
    /// Total trace length in **micrometres**. Upstream carries a `double` in
    /// millimetres; an integer keeps the whole struct comparable and hashable,
    /// and a micrometre is worth `0.001` score points against a maximum of five
    /// million per connection. See [`BoardStats::trace_length_mm`].
    pub trace_length_um: u64,
    /// Vias on signal layers.
    pub via_count: u64,
    /// Vias landing on a power plane, which upstream charges at a tenth.
    pub plane_via_count: u64,
}

impl BoardStats {
    /// The trace length in millimetres.
    pub fn trace_length_mm(&self) -> f64 {
        self.trace_length_um as f64 / 1000.0
    }
}

/// The score, with its three terms kept apart so a regression can be attributed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Score {
    /// `max_connections * unrouted_net_penalty`.
    pub maximum: f64,
    /// The penalty term (unrouted, violations, bends).
    pub penalties: f64,
    /// The cost term (trace length, vias).
    pub costs: f64,
    /// `maximum - penalties - costs`. Can be very negative.
    pub raw: f64,
    /// `max(0, raw / maximum) * 1000`, or `0` when `maximum <= 0`.
    pub normalized: f64,
}

/// Computes the v2 score.
///
/// `maximum <= 0` — a board with no net having two or more pins — is defined to
/// score `0`, not `NaN` and not "perfect" (§7.15). That is upstream's guard, and
/// it is the reason v2 rewrote `max_connections`: the v1.9 formula could go
/// non-positive on ordinary boards and pin every score to zero.
pub fn score(stats: &BoardStats, weights: &ScoreWeights) -> Score {
    let maximum = stats.max_connections as f64 * weights.unrouted_net_penalty;
    let penalties = stats.incomplete_count as f64 * weights.unrouted_net_penalty
        + stats.clearance_violation_count as f64 * weights.clearance_violation_penalty
        + stats.bend_count as f64 * weights.bend_penalty;
    let costs = stats.trace_length_mm() * weights.trace_cost_per_mm
        + stats.via_count as f64 * weights.via_cost
        + stats.plane_via_count as f64 * weights.plane_via_cost;
    let raw = maximum - penalties - costs;
    let normalized = if maximum <= 0.0 {
        0.0
    } else {
        (raw / maximum).max(0.0) * 1000.0
    };
    Score {
        maximum,
        penalties,
        costs,
        raw,
        normalized,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_perfect_board_scores_one_thousand() {
        let stats = BoardStats {
            max_connections: 10,
            ..BoardStats::default()
        };
        assert_eq!(score(&stats, &ScoreWeights::V2).normalized, 1000.0);
    }

    #[test]
    fn an_empty_board_scores_zero_rather_than_nan() {
        let s = score(&BoardStats::default(), &ScoreWeights::V2);
        assert_eq!(s.maximum, 0.0);
        assert_eq!(s.normalized, 0.0);
    }

    #[test]
    fn the_score_is_floored_at_zero_not_at_minus_infinity() {
        let stats = BoardStats {
            max_connections: 1,
            incomplete_count: 100,
            ..BoardStats::default()
        };
        let s = score(&stats, &ScoreWeights::V2);
        assert!(s.raw < 0.0);
        assert_eq!(s.normalized, 0.0);
    }

    #[test]
    fn hand_computed_spot_value() {
        // maximum   = 10 * 5e6                     = 50_000_000
        // penalties = 1 * 5e6 + 2 * 1e6 + 3 * 10   =  7_000_030
        // costs     = 4.5 * 1.0 + 2 * 50 + 1 * 5   =        109.5
        // raw       = 42_999_860.5
        let stats = BoardStats {
            max_connections: 10,
            incomplete_count: 1,
            clearance_violation_count: 2,
            bend_count: 3,
            trace_length_um: 4_500,
            via_count: 2,
            plane_via_count: 1,
        };
        let s = score(&stats, &ScoreWeights::V2);
        assert_eq!(s.maximum, 50_000_000.0);
        assert_eq!(s.penalties, 7_000_030.0);
        assert_eq!(s.costs, 109.5);
        assert_eq!(s.raw, 42_999_860.5);
        assert!((s.normalized - 859.997_21).abs() < 1e-5, "{}", s.normalized);
    }

    #[test]
    fn one_violation_costs_a_fifth_of_an_unrouted_connection_in_v2() {
        let with_violation = BoardStats {
            max_connections: 10,
            clearance_violation_count: 5,
            ..BoardStats::default()
        };
        let with_unrouted = BoardStats {
            max_connections: 10,
            incomplete_count: 1,
            ..BoardStats::default()
        };
        assert_eq!(
            score(&with_violation, &ScoreWeights::V2).raw,
            score(&with_unrouted, &ScoreWeights::V2).raw
        );
        // ... and a thousandth of one in v1.9. Weights are not comparable.
        assert_ne!(
            score(&with_violation, &ScoreWeights::V19).raw,
            score(&with_unrouted, &ScoreWeights::V19).raw
        );
    }
}
