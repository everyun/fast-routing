//! Batch autorouter engine with 3D multi-layer routing, via generation,
//! Power/Ground plane routing mode, Rip-up & Reroute, Rayon multi-core parallelism,
//! Delaunay MST, Disjoint-Set Connected Components analysis, and O(1) spatial grid.

use crate::maze_search::{MazeSearchAlgo, MazeSearchSettings, RoutePath3D};
use crate::net_connectivity::analyze_net_connectivity;
use crate::spatial_grid::LayerSpatialGrid;
use fr_board::{BasicBoard, FixedState, PolylineTrace, Via};
use fr_datastructures::planar_delaunay_triangulation::{PlanarDelaunayTriangulation, Point2D};
use fr_geometry::planar::IntBox;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};

/// Specific routing rules for an individual electrical net.
#[derive(Debug, Clone)]
pub struct NetRoutingRule {
    pub trace_half_width: i32,
    pub clearance_class: i32,
    pub via_padstack_name: String,
    pub via_pad_radius: i32,
    pub via_drill_radius: i32,
    pub is_plane: bool,
}

impl Default for NetRoutingRule {
    fn default() -> Self {
        NetRoutingRule {
            trace_half_width: 125, // 250um width -> 125um half-width
            clearance_class: 2,    // Trace clearance class
            via_padstack_name: "Via[0-1]_600:300_um".to_string(),
            via_pad_radius: 300,   // 600um diameter -> 300um radius
            via_drill_radius: 150, // 300um drill
            is_plane: false,
        }
    }
}

/// Configuration for the batch autorouter passes.
#[derive(Debug, Clone)]
pub struct BatchRouterSettings {
    pub max_passes: usize,
    pub default_rule: NetRoutingRule,
    pub net_rules: HashMap<i32, NetRoutingRule>,
    pub maze_settings: MazeSearchSettings,
}

impl Default for BatchRouterSettings {
    fn default() -> Self {
        BatchRouterSettings {
            max_passes: 10,
            default_rule: NetRoutingRule::default(),
            net_rules: HashMap::new(),
            maze_settings: MazeSearchSettings {
                step_size: 150, // 150um step resolution
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
    rule: NetRoutingRule,
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

    #[inline(always)]
    pub fn get_net_rule(&self, net_id: i32) -> NetRoutingRule {
        self.settings
            .net_rules
            .get(&net_id)
            .cloned()
            .unwrap_or_else(|| self.settings.default_rule.clone())
    }

    /// Executes the multi-pass autoroute loop on `board` for the given list of net IDs.
    pub fn route_board(&self, board: &mut BasicBoard, net_ids: &[i32]) -> RoutingStatistics {
        let mut already_routed_hashes = HashSet::new();
        let algo = MazeSearchAlgo::new(self.settings.maze_settings.clone());
        let layer_count = board.layer_count as i32;
        let cell_size = (self.settings.maze_settings.step_size * 6).max(800);

        // Phase 1: Fast Plane Net Stub Connection (GND / VCC / Power Planes)
        for &net_id in net_ids {
            let rule = self.get_net_rule(net_id);
            let pins = board.get_pins_for_net(net_id);
            if rule.is_plane || pins.len() >= 15 {
                let status = analyze_net_connectivity(board, net_id);
                if !status.is_fully_connected && status.component_anchors.len() > 1 {
                    for &(anchor, _) in &status.component_anchors {
                        let via_item = Via::new(
                            board.via_count() as i32 + 1,
                            net_id,
                            rule.clearance_class,
                            anchor,
                            &rule.via_padstack_name,
                            0,
                            layer_count - 1,
                            rule.via_pad_radius,
                            rule.via_drill_radius,
                        );
                        board.insert_via(via_item);
                    }
                }
            }
        }

        // Phase 2: Multi-Pass 3D Maze Search for Signal Nets with Rip-up & Reroute
        for pass in 1..=self.settings.max_passes {
            let mut pass_routed_any = false;

            // 1. Parallel candidate route generation across unconnected component anchors via Rayon
            let mut candidates: Vec<CandidateNetRoute> = net_ids
                .par_iter()
                .filter_map(|&net_id| {
                    let pins = board.get_pins_for_net(net_id);
                    let rule = self.get_net_rule(net_id);

                    // Plane nets are already connected in Phase 1
                    if rule.is_plane || pins.len() >= 15 {
                        return None;
                    }

                    let status = analyze_net_connectivity(board, net_id);

                    // Skip already fully connected nets
                    if status.is_fully_connected || status.component_anchors.len() < 2 {
                        return None;
                    }

                    // Build net-specific spatial grid: foreign fixed traces, foreign vias, and foreign pin pads
                    let mut net_grids: Vec<LayerSpatialGrid> = (0..board.layer_count)
                        .map(|_| LayerSpatialGrid::new(board.bounding_box, cell_size))
                        .collect();

                    // Insert foreign fixed traces (unfixed traces can be ripped up / crossed)
                    for trace in &board.traces {
                        if trace.header.net_no_arr.first() != Some(&net_id) {
                            if (trace.layer as usize) < net_grids.len() {
                                net_grids[trace.layer as usize].insert(trace.bounding_box());
                            }
                        }
                    }

                    // Insert foreign vias
                    for via in &board.vias {
                        if via.header.net_no_arr.first() != Some(&net_id) {
                            let min_l = via.first_layer.min(via.last_layer) as usize;
                            let max_l = via.first_layer.max(via.last_layer) as usize;
                            let pad_box = IntBox::new(
                                via.center.x - via.pad_radius,
                                via.center.y - via.pad_radius,
                                via.center.x + via.pad_radius,
                                via.center.y + via.pad_radius,
                            );
                            for l in min_l..=max_l.min(net_grids.len() - 1) {
                                net_grids[l].insert(pad_box);
                            }
                        }
                    }

                    // Insert foreign pin pads
                    for other_pin in &board.pins {
                        if other_pin.header.net_no_arr.first() != Some(&net_id) {
                            let min_l = other_pin.first_layer.max(0) as usize;
                            let max_l = other_pin.last_layer.min(layer_count - 1) as usize;
                            for l in min_l..=max_l.min(net_grids.len() - 1) {
                                net_grids[l].insert(other_pin.pad_bounding_box);
                            }
                        }
                    }

                    // Compute connection airlines between disconnected component clusters
                    let pin_pairs: Vec<(usize, usize)> = if status.component_anchors.len() == 2 {
                        vec![(0, 1)]
                    } else {
                        let pts: Vec<(Point2D, usize)> = status
                            .component_anchors
                            .iter()
                            .enumerate()
                            .map(|(idx, &(p, _))| (Point2D::from_i32(p.x, p.y), idx))
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
                    for (u, v) in pin_pairs {
                        if u < status.component_anchors.len() && v < status.component_anchors.len() {
                            let (start, start_layer) = status.component_anchors[u];
                            let (target, target_layer) = status.component_anchors[v];

                            let res = algo.find_path_3d_grid(
                                start,
                                start_layer,
                                target,
                                target_layer,
                                layer_count,
                                rule.trace_half_width,
                                &net_grids,
                            );
                            if let Some(path_3d) = res {
                                net_paths.push(path_3d);
                            }
                        }
                    }

                    if !net_paths.is_empty() {
                        Some(CandidateNetRoute {
                            net_id,
                            rule,
                            paths: net_paths,
                        })
                    } else {
                        None
                    }
                })
                .collect();

            // Deterministic sort by net_id before commit
            candidates.sort_by_key(|c| c.net_id);

            // 2. Transactional Spatial Commit with Rip-up of conflicting non-fixed traces
            for candidate in candidates {
                let mut has_unresolvable_conflict = false;
                let mut conflicting_trace_ids = Vec::new();

                // Check candidate segments against foreign traces, vias, and pins
                for path in &candidate.paths {
                    for seg in &path.segments {
                        let seg_boxes = self.segment_to_boxes(&seg.points, candidate.rule.trace_half_width);
                        for s_box in seg_boxes {
                            if (seg.layer as usize) < board.layer_count {
                                // Check foreign traces
                                for t in &board.traces {
                                    if t.header.net_no_arr.first() != Some(&candidate.net_id)
                                        && t.layer == seg.layer
                                        && t.bounding_box().intersects(&s_box)
                                    {
                                        if t.header.fixed_state == FixedState::Unfixed {
                                            conflicting_trace_ids.push(t.header.id_no);
                                        } else {
                                            has_unresolvable_conflict = true;
                                            break;
                                        }
                                    }
                                }
                                if has_unresolvable_conflict {
                                    break;
                                }

                                // Check foreign vias
                                let collides_foreign_via = board.vias.iter().any(|v| {
                                    v.header.net_no_arr.first() != Some(&candidate.net_id)
                                        && seg.layer >= v.first_layer
                                        && seg.layer <= v.last_layer
                                        && IntBox::new(
                                            v.center.x - v.pad_radius,
                                            v.center.y - v.pad_radius,
                                            v.center.x + v.pad_radius,
                                            v.center.y + v.pad_radius,
                                        )
                                        .intersects(&s_box)
                                });

                                // Check foreign pins
                                let collides_foreign_pin = board.pins.iter().any(|p| {
                                    p.header.net_no_arr.first() != Some(&candidate.net_id)
                                        && seg.layer >= p.first_layer
                                        && seg.layer <= p.last_layer
                                        && p.pad_bounding_box.intersects(&s_box)
                                });

                                if collides_foreign_via || collides_foreign_pin {
                                    has_unresolvable_conflict = true;
                                    break;
                                }
                            }
                        }
                        if has_unresolvable_conflict {
                            break;
                        }
                    }
                    if has_unresolvable_conflict {
                        break;
                    }
                }

                if !has_unresolvable_conflict {
                    // Rip up conflicting unfixed traces
                    if !conflicting_trace_ids.is_empty() {
                        board.traces.retain(|t| !conflicting_trace_ids.contains(&t.header.id_no));
                    }

                    // Commit candidate route to board
                    for path in candidate.paths {
                        for seg in path.segments {
                            let trace = PolylineTrace::new(
                                board.trace_count() as i32 + 1,
                                candidate.net_id,
                                candidate.rule.clearance_class,
                                seg.layer,
                                candidate.rule.trace_half_width,
                                seg.points,
                            );
                            board.insert_trace(trace);
                        }

                        for via in path.vias {
                            let via_item = Via::new(
                                board.via_count() as i32 + 1,
                                candidate.net_id,
                                candidate.rule.clearance_class,
                                via.point,
                                &candidate.rule.via_padstack_name,
                                via.from_layer.min(via.to_layer),
                                via.from_layer.max(via.to_layer),
                                candidate.rule.via_pad_radius,
                                candidate.rule.via_drill_radius,
                            );
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

        // 3. Authoritative electrical connectivity evaluation
        let mut completed_nets = 0;
        let mut unrouted_nets = 0;
        for &net_id in net_ids {
            let pins = board.get_pins_for_net(net_id);
            if pins.len() <= 1 {
                completed_nets += 1; // 1-pin or NC nets are complete by definition
            } else {
                let status = analyze_net_connectivity(board, net_id);
                if status.is_fully_connected {
                    completed_nets += 1;
                } else {
                    unrouted_nets += 1;
                }
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

    /// Converts a polyline trace into fine-grained bounding boxes along the path for accurate collision checks.
    fn segment_to_boxes(&self, points: &[fr_geometry::planar::IntPoint], half_width: i32) -> Vec<IntBox> {
        let mut boxes = Vec::new();
        let step = 1000.max(half_width * 2);
        for i in 1..points.len() {
            let p1 = points[i - 1];
            let p2 = points[i];
            let dx = p2.x - p1.x;
            let dy = p2.y - p1.y;
            let dist = ((dx as f64) * (dx as f64) + (dy as f64) * (dy as f64)).sqrt();
            let num_steps = (dist / step as f64).ceil().max(1.0) as usize;
            for s in 0..=num_steps {
                let t = (s as f64) / (num_steps as f64);
                let cx = (p1.x as f64 + (dx as f64) * t).round() as i32;
                let cy = (p1.y as f64 + (dy as f64) * t).round() as i32;
                boxes.push(IntBox::new(
                    cx - half_width,
                    cy - half_width,
                    cx + half_width,
                    cy + half_width,
                ));
            }
        }
        boxes
    }
}
