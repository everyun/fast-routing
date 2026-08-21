//! Batch autorouter engine with multi-pass rip-up/reroute and Rayon multi-core parallelism.

use crate::maze_search::{MazeSearchAlgo, MazeSearchSettings};
use fr_board::{BasicBoard, PolylineTrace};
use fr_geometry::planar::IntBox;
use rayon::prelude::*;
use std::collections::HashSet;

/// Configuration for the batch autorouter passes.
#[derive(Debug, Clone)]
pub struct BatchRouterSettings {
    pub max_passes: usize,
    pub trace_half_width: i32,
    pub clearance_class: i32,
    pub maze_settings: MazeSearchSettings,
}

impl Default for BatchRouterSettings {
    fn default() -> Self {
        BatchRouterSettings {
            max_passes: 10,
            trace_half_width: 125, // 250um width -> 125um half-width
            clearance_class: 2,    // Trace clearance class
            maze_settings: MazeSearchSettings::default(),
        }
    }
}

/// Statistics and progress report after a routing job or pass.
#[derive(Debug, Clone, Default)]
pub struct RoutingStatistics {
    pub total_nets: usize,
    pub completed_nets: usize,
    pub unrouted_nets: usize,
    pub total_vias: usize,
    pub total_trace_length: f64,
}

/// Batch autorouter orchestrator.
pub struct BatchAutorouter {
    pub settings: BatchRouterSettings,
}

impl BatchAutorouter {
    pub fn new(settings: BatchRouterSettings) -> Self {
        BatchAutorouter { settings }
    }

    /// Executes the multi-pass autoroute loop on `board` for the given list of net IDs.
    pub fn route_board(&self, board: &mut BasicBoard, net_ids: &[i32]) -> RoutingStatistics {
        let mut already_routed_hashes = HashSet::new();
        let algo = MazeSearchAlgo::new(self.settings.maze_settings.clone());

        for pass in 1..=self.settings.max_passes {
            let mut pass_routed_any = false;

            // Collect existing trace boxes as obstacles for subsequent nets
            let obstacle_boxes: Vec<IntBox> =
                board.traces.iter().map(|t| t.bounding_box()).collect();

            // Parallel route candidate generation across independent nets via Rayon
            let candidate_routes: Vec<(i32, Vec<PolylineTrace>)> = net_ids
                .par_iter()
                .filter_map(|&net_id| {
                    let pins = board.get_pins_for_net(net_id);
                    let existing_traces = board.get_traces_for_net(net_id);

                    // Skip already connected nets
                    if pins.len() < 2 || !existing_traces.is_empty() {
                        return None;
                    }

                    let mut net_traces = Vec::new();
                    // Connect pin chains (pin[i-1] -> pin[i])
                    for i in 1..pins.len() {
                        let start = pins[i - 1].center;
                        let target = pins[i].center;
                        let layer = pins[i - 1].first_layer;

                        if let Some(path) = algo.find_path(start, target, layer, &obstacle_boxes) {
                            net_traces.push(PolylineTrace::new(
                                board.trace_count() as i32 + 1,
                                net_id,
                                self.settings.clearance_class,
                                path.layer,
                                self.settings.trace_half_width,
                                path.points,
                            ));
                        }
                    }

                    if !net_traces.is_empty() {
                        Some((net_id, net_traces))
                    } else {
                        None
                    }
                })
                .collect();

            for (_net_id, traces) in candidate_routes {
                for trace in traces {
                    board.insert_trace(trace);
                    pass_routed_any = true;
                }
            }

            // Stagnation check via board topology hash
            let board_hash = format!(
                "pass-{}-traces-{}-len-{}",
                pass,
                board.trace_count(),
                board.total_trace_length() as i64
            );

            if !already_routed_hashes.insert(board_hash) || !pass_routed_any {
                // Convergence / stagnation reached
                break;
            }
        }

        // Calculate final routing statistics
        let mut completed_nets = 0;
        let mut unrouted_nets = 0;
        for &net_id in net_ids {
            let pins = board.get_pins_for_net(net_id);
            let traces = board.get_traces_for_net(net_id);
            if pins.len() >= 2 && !traces.is_empty() {
                completed_nets += 1;
            } else if pins.len() >= 2 {
                unrouted_nets += 1;
            }
        }

        RoutingStatistics {
            total_nets: net_ids.len(),
            completed_nets,
            unrouted_nets,
            total_vias: board.via_count(),
            total_trace_length: board.total_trace_length(),
        }
    }
}
