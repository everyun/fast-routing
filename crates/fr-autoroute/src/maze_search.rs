//! 45-degree 3D Multi-Layer Maze Search and Path Expansion Algorithm (Modified A*).
//!
//! Supports:
//! - 8 planar 45° directions on the active layer
//! - 3D vertical layer-transition expansions (Via insertion) with configurable via costs
//! - Layer-specific obstacle collision checks (traces, pads, vias)
//! - Collinear segment reduction & path reconstruction

use fr_geometry::planar::{Direction, IntBox, IntPoint};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

/// A node in the 3D A* expansion priority queue.
#[derive(Debug, Clone, Copy, PartialEq)]
struct QueueElement {
    point: IntPoint,
    layer: i32,
    direction: Direction,
    cost_from_start: f64,
    estimated_total_cost: f64,
}

impl Eq for QueueElement {}

impl Ord for QueueElement {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse ordering for min-heap
        other
            .estimated_total_cost
            .partial_cmp(&self.estimated_total_cost)
            .unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for QueueElement {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Settings and cost factors for the 3D maze search algorithm.
#[derive(Debug, Clone)]
pub struct MazeSearchSettings {
    pub step_size: i32,
    pub bend_cost: f64,
    pub layer_change_cost: f64,
    pub max_expansion_nodes: usize,
}

impl Default for MazeSearchSettings {
    fn default() -> Self {
        MazeSearchSettings {
            step_size: 250, // 250 um
            bend_cost: 100.0,
            layer_change_cost: 600.0, // Via insertion penalty
            max_expansion_nodes: 60_000,
        }
    }
}

/// A trace segment on a single layer.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteSegment3D {
    pub layer: i32,
    pub points: Vec<IntPoint>,
}

/// A via connection between two layers.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteVia3D {
    pub point: IntPoint,
    pub from_layer: i32,
    pub to_layer: i32,
}

/// Result of a 3D multi-layer maze search connection attempt.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutePath3D {
    pub segments: Vec<RouteSegment3D>,
    pub vias: Vec<RouteVia3D>,
    pub total_cost: f64,
}

/// Backward-compatible single-layer route path representation.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutePath {
    pub points: Vec<IntPoint>,
    pub layer: i32,
    pub total_cost: f64,
}

/// 45-degree 3D maze search pathfinder.
pub struct MazeSearchAlgo {
    pub settings: MazeSearchSettings,
}

impl MazeSearchAlgo {
    pub fn new(settings: MazeSearchSettings) -> Self {
        MazeSearchAlgo { settings }
    }

    /// Finds a 3D multi-layer obstacle-avoiding path between `(start, start_layer)` and `(target, target_layer)`.
    pub fn find_path_3d(
        &self,
        start: IntPoint,
        start_layer: i32,
        target: IntPoint,
        target_layer: i32,
        layer_count: i32,
        obstacles_per_layer: &[Vec<IntBox>],
    ) -> Option<RoutePath3D> {
        let mut open_set = BinaryHeap::new();
        let mut cost_so_far: HashMap<(IntPoint, i32), f64> = HashMap::new();
        let mut came_from: HashMap<(IntPoint, i32), (IntPoint, i32)> = HashMap::new();

        let initial_dist = start.distance(&target);
        let initial_layer_diff = (start_layer - target_layer).abs() as f64 * self.settings.layer_change_cost;
        let initial_est = initial_dist + initial_layer_diff;

        open_set.push(QueueElement {
            point: start,
            layer: start_layer,
            direction: Direction::NULL,
            cost_from_start: 0.0,
            estimated_total_cost: initial_est,
        });
        cost_so_far.insert((start, start_layer), 0.0);

        let directions = [
            Direction::RIGHT,
            Direction::RIGHT45,
            Direction::UP,
            Direction::UP45,
            Direction::LEFT,
            Direction::LEFT45,
            Direction::DOWN,
            Direction::DOWN45,
        ];

        let mut nodes_visited = 0;

        while let Some(current) = open_set.pop() {
            nodes_visited += 1;
            if nodes_visited > self.settings.max_expansion_nodes {
                break;
            }

            // Target reached (within step size and matching target layer)
            if current.layer == target_layer && current.point.distance(&target) <= self.settings.step_size as f64 {
                // Reconstruct full 3D path
                let mut raw_nodes = vec![(target, target_layer), (current.point, current.layer)];
                let mut curr_key = (current.point, current.layer);
                while let Some(&prev_key) = came_from.get(&curr_key) {
                    raw_nodes.push(prev_key);
                    curr_key = prev_key;
                }
                raw_nodes.reverse();

                return Some(self.split_into_segments_and_vias(raw_nodes, current.cost_from_start));
            }

            // 1. Planar 45° step expansions on the current layer
            for dir in &directions {
                let step_vec = dir.get_int_vector();
                let next_pt = IntPoint::new(
                    current.point.x + step_vec.x * self.settings.step_size,
                    current.point.y + step_vec.y * self.settings.step_size,
                );

                // Collision check on current layer
                let layer_idx = current.layer as usize;
                let collides = if layer_idx < obstacles_per_layer.len() {
                    obstacles_per_layer[layer_idx].iter().any(|b| next_pt.is_contained_in(b))
                } else {
                    false
                };

                if collides {
                    continue;
                }

                let step_dist = current.point.distance(&next_pt);
                let bend_penalty = if current.direction != Direction::NULL && current.direction != *dir {
                    self.settings.bend_cost
                } else {
                    0.0
                };

                let new_cost = current.cost_from_start + step_dist + bend_penalty;
                let next_key = (next_pt, current.layer);

                if !cost_so_far.contains_key(&next_key) || new_cost < cost_so_far[&next_key] {
                    cost_so_far.insert(next_key, new_cost);
                    came_from.insert(next_key, (current.point, current.layer));
                    let layer_dist = (current.layer - target_layer).abs() as f64 * self.settings.layer_change_cost;
                    let priority = new_cost + next_pt.distance(&target) + layer_dist;
                    open_set.push(QueueElement {
                        point: next_pt,
                        layer: current.layer,
                        direction: *dir,
                        cost_from_start: new_cost,
                        estimated_total_cost: priority,
                    });
                }
            }

            // 2. Vertical layer-transition expansions (Via insertion)
            let adjacent_layers = [current.layer - 1, current.layer + 1];
            for &next_l in &adjacent_layers {
                if next_l >= 0 && next_l < layer_count {
                    let next_l_idx = next_l as usize;
                    let collides = if next_l_idx < obstacles_per_layer.len() {
                        obstacles_per_layer[next_l_idx].iter().any(|b| current.point.is_contained_in(b))
                    } else {
                        false
                    };

                    if collides {
                        continue;
                    }

                    let new_cost = current.cost_from_start + self.settings.layer_change_cost;
                    let next_key = (current.point, next_l);

                    if !cost_so_far.contains_key(&next_key) || new_cost < cost_so_far[&next_key] {
                        cost_so_far.insert(next_key, new_cost);
                        came_from.insert(next_key, (current.point, current.layer));
                        let layer_dist = (next_l - target_layer).abs() as f64 * self.settings.layer_change_cost;
                        let priority = new_cost + current.point.distance(&target) + layer_dist;
                        open_set.push(QueueElement {
                            point: current.point,
                            layer: next_l,
                            direction: Direction::NULL,
                            cost_from_start: new_cost,
                            estimated_total_cost: priority,
                        });
                    }
                }
            }
        }

        None
    }

    /// Backward-compatible 2D planar single-layer wrapper.
    pub fn find_path(
        &self,
        start: IntPoint,
        target: IntPoint,
        layer: i32,
        obstacle_boxes: &[IntBox],
    ) -> Option<RoutePath> {
        let obs_vec = vec![obstacle_boxes.to_vec()];
        let path_3d = self.find_path_3d(start, layer, target, layer, layer + 1, &obs_vec)?;
        if let Some(first_seg) = path_3d.segments.first() {
            Some(RoutePath {
                points: first_seg.points.clone(),
                layer: first_seg.layer,
                total_cost: path_3d.total_cost,
            })
        } else {
            None
        }
    }

    /// Splits a 3D node sequence into layer trace segments and via transitions.
    fn split_into_segments_and_vias(
        &self,
        raw_nodes: Vec<(IntPoint, i32)>,
        total_cost: f64,
    ) -> RoutePath3D {
        let mut segments = Vec::new();
        let mut vias = Vec::new();

        if raw_nodes.is_empty() {
            return RoutePath3D {
                segments,
                vias,
                total_cost,
            };
        }

        let mut current_segment_pts = vec![raw_nodes[0].0];
        let mut current_layer = raw_nodes[0].1;

        for i in 1..raw_nodes.len() {
            let (pt, lyr) = raw_nodes[i];
            if lyr == current_layer {
                current_segment_pts.push(pt);
            } else {
                // Layer transition -> finalize current segment & record via
                if current_segment_pts.len() >= 2 {
                    let simplified = self.simplify_path(current_segment_pts);
                    segments.push(RouteSegment3D {
                        layer: current_layer,
                        points: simplified,
                    });
                }
                vias.push(RouteVia3D {
                    point: pt,
                    from_layer: current_layer,
                    to_layer: lyr,
                });
                current_layer = lyr;
                current_segment_pts = vec![pt];
            }
        }

        if current_segment_pts.len() >= 2 {
            let simplified = self.simplify_path(current_segment_pts);
            segments.push(RouteSegment3D {
                layer: current_layer,
                points: simplified,
            });
        }

        RoutePath3D {
            segments,
            vias,
            total_cost,
        }
    }

    /// Merges consecutive collinear segments into single long traces.
    fn simplify_path(&self, points: Vec<IntPoint>) -> Vec<IntPoint> {
        if points.len() <= 2 {
            return points;
        }
        let mut result = vec![points[0]];
        let mut last_dir = Direction::from_int_points(&points[0], &points[1]);

        for i in 2..points.len() {
            let curr_dir = Direction::from_int_points(&points[i - 1], &points[i]);
            if curr_dir != last_dir {
                result.push(points[i - 1]);
                last_dir = curr_dir;
            }
        }
        result.push(*points.last().unwrap());
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_maze_search_straight_line() {
        let algo = MazeSearchAlgo::new(MazeSearchSettings {
            step_size: 100,
            ..Default::default()
        });
        let start = IntPoint::new(0, 0);
        let target = IntPoint::new(1000, 0);
        let route = algo.find_path(start, target, 0, &[]).unwrap();
        assert!(route.points.len() >= 2);
        assert_eq!(route.points[0], start);
        assert_eq!(*route.points.last().unwrap(), target);
    }

    #[test]
    fn test_maze_search_3d_layer_transition() {
        let algo = MazeSearchAlgo::new(MazeSearchSettings {
            step_size: 100,
            layer_change_cost: 50.0,
            ..Default::default()
        });
        let start = IntPoint::new(0, 0);
        let target = IntPoint::new(500, 500);
        let obs_l0 = vec![IntBox::new(200, 0, 400, 600)]; // Obstacle on layer 0 blocking direct path
        let obs_l1 = vec![];

        let obstacles = vec![obs_l0, obs_l1];
        let path = algo.find_path_3d(start, 0, target, 0, 2, &obstacles).unwrap();
        assert!(!path.vias.is_empty(), "Expected via to route around layer 0 obstacle");
    }
}
