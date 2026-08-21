//! Comprehensive Design Rules Checker (DRC).

use crate::clearance_violation::ClearanceViolation;
use crate::net_incompletes::{AirLine, NetIncompletes};
use fr_board::BasicBoard;
use std::collections::HashSet;

/// Design Rules Checker responsible for verifying that no clearance rules are violated
/// and detecting unrouted nets.
pub struct DesignRulesChecker<'a> {
    pub board: &'a BasicBoard,
}

impl<'a> DesignRulesChecker<'a> {
    pub fn new(board: &'a BasicBoard) -> Self {
        DesignRulesChecker { board }
    }

    /// Authoritative method: collects all unique clearance violations across all board item pairs.
    pub fn get_all_clearance_violations(&self, default_clearance: f64) -> Vec<ClearanceViolation> {
        let mut violations = Vec::new();
        let mut seen_keys = HashSet::new();

        // Check trace vs trace clearance
        for i in 0..self.board.traces.len() {
            let t1 = &self.board.traces[i];
            for j in (i + 1)..self.board.traces.len() {
                let t2 = &self.board.traces[j];
                if t1.layer != t2.layer {
                    continue;
                }
                // If same net, no foreign clearance check needed
                if t1.header.net_no_arr == t2.header.net_no_arr && !t1.header.net_no_arr.is_empty() {
                    continue;
                }

                // Check distance between trace bounding boxes
                let b1 = t1.bounding_box();
                let b2 = t2.bounding_box();
                if b1.intersects(&b2.offset(default_clearance)) {
                    // Check segment-level distance
                    for p1 in &t1.corner_points {
                        for p2 in &t2.corner_points {
                            let dist = p1.distance(p2) - (t1.half_width + t2.half_width) as f64;
                            if dist < default_clearance {
                                let id1 = t1.header.id_no;
                                let id2 = t2.header.id_no;
                                let key = if id1 < id2 {
                                    format!("{}-{}-{}", id1, id2, t1.layer)
                                } else {
                                    format!("{}-{}-{}", id2, id1, t1.layer)
                                };
                                if seen_keys.insert(key) {
                                    violations.push(ClearanceViolation::new(
                                        id1,
                                        id2,
                                        t1.layer,
                                        default_clearance,
                                        dist.max(0.0),
                                        *p1,
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }

        violations
    }

    /// Evaluates ratsnest completeness for all nets.
    pub fn get_all_net_incompletes(&self, net_ids: &[i32]) -> Vec<NetIncompletes> {
        let mut result = Vec::new();

        for &net_id in net_ids {
            let mut inc = NetIncompletes::new(net_id);
            let pins = self.board.get_pins_for_net(net_id);
            let traces = self.board.get_traces_for_net(net_id);

            // If we have >= 2 pins and 0 traces, all pin pairs are incomplete
            if pins.len() >= 2 && traces.is_empty() {
                for i in 1..pins.len() {
                    inc.unrouted_air_lines.push(AirLine {
                        from: pins[i - 1].center,
                        to: pins[i].center,
                        net_no: net_id,
                    });
                }
            }

            result.push(inc);
        }

        result
    }
}
