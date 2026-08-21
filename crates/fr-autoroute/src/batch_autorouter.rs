//! Batch autorouter engine with 3D multi-layer routing, via generation,
//! Rayon multi-core parallelism, Delaunay Minimum Spanning Tree (MST) air-lines,
//! and transactional spatial conflict resolution.

use crate::maze_search::{MazeSearchAlgo, MazeSearchSettings, RoutePath3D};
use fr_board::{BasicBoard, PolylineTrace, Via};
use fr_datastructures::planar_delaunay_triangulation::{PlanarDelaunayTriangulation, Point2D};
use fr_geometry::planar::IntBox;
use rayon::prelude::*;
use std::collections::HashSet;

/// Configuration for the batch autorouter passes.
#[derive(Debug, Clone)]
pub struct BatchRouterSettings {
    pub max_passes: usize,
    pub trace_half_width: i32,
    pub clearance_class: i32,
    pub via_padstack_name: String,
    pub via_pad_radius: i32,
    pub via_drill_radius: i32,
    pub maze_settings: MazeSearchSettings,
}

impl Default for BatchRouterSettings {
    fn default() -> Self {
        BatchRouterSettings {
            max_passes: 10,
            trace_half_width: 125, // 250um width -> 125um half-width
            clearance_class: 2,    // Trace clearance class
            via_padstack_name: "Via[0-1]_600:300_um".to_string(),
            via_pad_radius: 300,   // 600um diameter -> 300um radius
            via_drill_radius: 150, // 300um drill
            maze_settings: MazeSearchSettings {
                step_size: 150, // 150um step resolution for dense routing
                bend_cost: 80.0,
                layer_change_cost: 400.0,
                max_expansion_nodes: 80_000,
            },
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

/// Candidate 3D route for a net waiting for transactional commit.
#[derive(Debug, Clone)]
struct CandidateNetRoute {
    net_id: i32,
    paths: Vec<RoutePath3D>,
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
        let layer_count = board.layer_count as i32;

        for pass in 1..=self.settings.max_passes {
            let mut pass_routed_any = false;

            // 1. Build layer-specific obstacle spatial boxes from existing traces, vias, and foreign pins
            let mut obstacles_per_layer: Vec<Vec<IntBox>> = vec![Vec::new(); board.layer_count];
            for trace in &board.traces {
                if (trace.layer as usize) < obstacles_per_layer.len() {
                    obstacles_per_layer[trace.layer as usize].push(trace.bounding_box());
                }
            }
            for via in &board.vias {
                let min_l = via.first_layer.min(via.last_layer) as usize;
                let max_l = via.first_layer.max(via.last_layer) as usize;
                let pad_box = IntBox::new(
                    via.center.x - via.pad_radius,
                    via.center.y - via.pad_radius,
                    via.center.x + via.pad_radius,
                    via.center.y + via.pad_radius,
                );
                for l in min_l..=max_l.min(obstacles_per_layer.len() - 1) {
                    obstacles_per_layer[l].push(pad_box);
                }
            }

            // 2. Parallel candidate route generation across unconnected nets via Rayon
            let candidates: Vec<CandidateNetRoute> = net_ids
                .par_iter()
                .filter_map(|&net_id| {
                    let pins = board.get_pins_for_net(net_id);
                    let existing_traces = board.get_traces_for_net(net_id);

                    // Skip already connected nets
                    if pins.len() < 2 || !existing_traces.is_empty() {
                        return None;
                    }

                    // Clone layer obstacles and add foreign pin pads
                    let mut net_obstacles = obstacles_per_layer.clone();
                    for other_pin in &board.pins {
                        if other_pin.header.net_no_arr.first() != Some(&net_id) {
                            let min_l = other_pin.first_layer.max(0) as usize;
                            let max_l = other_pin.last_layer.min(layer_count - 1) as usize;
                            for l in min_l..=max_l.min(net_obstacles.len() - 1) {
                                net_obstacles[l].push(other_pin.pad_bounding_box);
                            }
                        }
                    }

                    // Compute connection airlines: 2-pin direct or Delaunay MST for multi-pin nets
                    let pin_pairs: Vec<(usize, usize)> = if pins.len() == 2 {
                        vec![(0, 1)]
                    } else {
                        let pts: Vec<(Point2D, usize)> = pins
                            .iter()
                            .enumerate()
                            .map(|(idx, p)| (Point2D::from_i32(p.center.x, p.center.y), idx))
                            .collect();
                        let tri = PlanarDelaunayTriangulation::new(pts);
                        let mst = tri.minimum_spanning_tree();
                        mst.into_iter()
                            .filter_map(|e| match (e.start_data, e.end_data) {
                                (Some(u), Some(v)) => Some((u, v)),
                                _ => None,
                            })
                            .collect()
                    };

                    let mut net_paths = Vec::new();
                    // Connect each MST pair in 3D multi-layer space
                    for (u, v) in pin_pairs {
                        let start = pins[u].center;
                        let start_layer = pins[u].first_layer;
                        let target = pins[v].center;
                        let target_layer = pins[v].first_layer;

                        if let Some(path_3d) = algo.find_path_3d(
                            start,
                            start_layer,
                            target,
                            target_layer,
                            layer_count,
                            &net_obstacles,
                        ) {
                            net_paths.push(path_3d);
                        } else {
                            // Incomplete chain connection
                            return None;
                        }
                    }

                    if !net_paths.is_empty() {
                        Some(CandidateNetRoute {
                            net_id,
                            paths: net_paths,
                        })
                    } else {
                        None
                    }
                })
                .collect();

            // 3. Transactional Spatial Commit & Collision Check against previously committed routes
            for candidate in candidates {
                let mut has_conflict = false;

                // Check candidate segments and vias against current board state
                for path in &candidate.paths {
                    for seg in &path.segments {
                        let seg_boxes = self.segment_to_boxes(&seg.points, self.settings.trace_half_width);
                        for s_box in seg_boxes {
                            if (seg.layer as usize) < obstacles_per_layer.len() {
                                if obstacles_per_layer[seg.layer as usize].iter().any(|b| s_box.intersects(b)) {
                                    has_conflict = true;
                                    break;
                                }
                            }
                        }
                        if has_conflict {
                            break;
                        }
                    }
                    if has_conflict {
                        break;
                    }
                }

                if !has_conflict {
                    // Commit candidate route to board
                    for path in candidate.paths {
                        for seg in path.segments {
                            let trace = PolylineTrace::new(
                                board.trace_count() as i32 + 1,
                                candidate.net_id,
                                self.settings.clearance_class,
                                seg.layer,
                                self.settings.trace_half_width,
                                seg.points,
                            );
                            if (seg.layer as usize) < obstacles_per_layer.len() {
                                obstacles_per_layer[seg.layer as usize].push(trace.bounding_box());
                            }
                            board.insert_trace(trace);
                        }

                        for via in path.vias {
                            let via_item = Via::new(
                                board.via_count() as i32 + 1,
                                candidate.net_id,
                                self.settings.clearance_class,
                                via.point,
                                &self.settings.via_padstack_name,
                                via.from_layer.min(via.to_layer),
                                via.from_layer.max(via.to_layer),
                                self.settings.via_pad_radius,
                                self.settings.via_drill_radius,
                            );
                            let pad_box = IntBox::new(
                                via.point.x - self.settings.via_pad_radius,
                                via.point.y - self.settings.via_pad_radius,
                                via.point.x + self.settings.via_pad_radius,
                                via.point.y + self.settings.via_pad_radius,
                            );
                            let min_l = via.from_layer.min(via.to_layer) as usize;
                            let max_l = via.from_layer.max(via.to_layer) as usize;
                            for l in min_l..=max_l.min(obstacles_per_layer.len() - 1) {
                                obstacles_per_layer[l].push(pad_box);
                            }
                            board.insert_via(via_item);
                        }
                    }
                    pass_routed_any = true;
                }
            }

            // Stagnation check via board topology hash
            let board_hash = format!(
                "pass-{}-traces-{}-vias-{}-len-{}",
                pass,
                board.trace_count(),
                board.via_count(),
                board.total_trace_length() as i64
            );

            if !already_routed_hashes.insert(board_hash) || !pass_routed_any {
                // Convergence reached
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

    /// Converts a polyline trace points into bounding boxes for collision checks.
    fn segment_to_boxes(&self, points: &[fr_geometry::planar::IntPoint], half_width: i32) -> Vec<IntBox> {
        let mut boxes = Vec::new();
        for i in 1..points.len() {
            let min_x = points[i - 1].x.min(points[i].x) - half_width;
            let min_y = points[i - 1].y.min(points[i].y) - half_width;
            let max_x = points[i - 1].x.max(points[i].x) + half_width;
            let max_y = points[i - 1].y.max(points[i].y) + half_width;
            boxes.push(IntBox::new(min_x, min_y, max_x, max_y));
        }
        boxes
    }
}
