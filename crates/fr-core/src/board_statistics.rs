//! Board statistics and objective scoring calculation.

use fr_board::BasicBoard;

/// Comprehensive board metrics and objective score breakdown.
#[derive(Debug, Clone, Default)]
pub struct BoardStatistics {
    pub unrouted_net_count: usize,
    pub via_count: usize,
    pub total_trace_length: f64,
    pub clearance_violation_count: usize,
}

impl BoardStatistics {
    pub fn compute(board: &BasicBoard, unrouted_nets: usize, clearance_violations: usize) -> Self {
        BoardStatistics {
            unrouted_net_count: unrouted_nets,
            via_count: board.via_count(),
            total_trace_length: board.total_trace_length(),
            clearance_violation_count: clearance_violations,
        }
    }

    /// Calculates normalized cost score (lower is better; 0 = perfect route).
    pub fn calculate_score(&self) -> f64 {
        const UNROUTED_PENALTY: f64 = 100_000.0;
        const DRC_VIOLATION_PENALTY: f64 = 500_000.0;
        const VIA_PENALTY: f64 = 100.0;

        (self.unrouted_net_count as f64) * UNROUTED_PENALTY
            + (self.clearance_violation_count as f64) * DRC_VIOLATION_PENALTY
            + (self.via_count as f64) * VIA_PENALTY
            + self.total_trace_length * 0.001
    }
}
