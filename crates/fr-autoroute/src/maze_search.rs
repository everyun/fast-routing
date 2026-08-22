//! 3D multi-layer A* maze path search algorithm with 45-degree octilinear routing,
//! via generation, vertical transitions, and pull-tight corner optimization.

use crate::spatial_grid::LayerSpatialGrid;
use fr_geometry::planar::{Direction, IntBox, IntPoint};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

/// Settings for the 3D maze search algorithm.
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
            step_size: 150, // 150um step resolution
            bend_cost: 80.0,
            layer_change_cost: 400.0,
            max_expansion_nodes: 80_000,
        }
    }
}

/// A 2D planar route path.
#[derive(Debug, Clone)]
pub struct RoutePath {
    pub points: Vec<IntPoint>,
    pub layer: i32,
    pub total_cost: f64,
}

/// A single-layer trace segment within a 3D multi-layer route.
#[derive(Debug, Clone)]
pub struct RouteSegment3D {
    pub layer: i32,
    pub points: Vec<IntPoint>,
}

/// A vertical via connection between layers within a 3D route.
#[derive(Debug, Clone)]
pub struct RouteVia3D {
    pub point: IntPoint,
    pub from_layer: i32,
    pub to_layer: i32,
}

/// Complete 3D multi-layer route path with planar segments and vertical vias.
#[derive(Debug, Clone)]
pub struct RoutePath3D {
    pub segments: Vec<RouteSegment3D>,
    pub vias: Vec<RouteVia3D>,
    pub total_cost: f64,
}

#[derive(Copy, Clone, PartialEq)]
struct State {
    cost: f64,
    priority: f64,
    x: i32,
    y: i32,
    layer: i32,
}

impl Eq for State {}

impl Ord for State {
    fn cmp(&self, other: &Self) -> Ordering {
        other.priority.partial_cmp(&self.priority).unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for State {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// 3D multi-layer obstacle-avoiding maze path search engine.
pub struct MazeSearchAlgo {
    pub settings: MazeSearchSettings,
}

impl MazeSearchAlgo {
    pub fn new(settings: MazeSearchSettings) -> Self {
        MazeSearchAlgo { settings }
    }

    /// Finds a 3D multi-layer route from `(start, start_layer)` to `(target, target_layer)`.
    pub fn find_path_3d_grid(
        &self,
        start: IntPoint,
        start_layer: i32,
        target: IntPoint,
        target_layer: i32,
        layer_count: i32,
        half_width: i32,
        spatial_grids: &[LayerSpatialGrid],
    ) -> Option<RoutePath3D> {
        let step = self.settings.step_size;
        let mut open_set = BinaryHeap::new();
        let mut came_from: HashMap<(i32, i32, i32), (i32, i32, i32)> = HashMap::new();
        let mut cost_so_far: HashMap<(i32, i32, i32), f64> = HashMap::new();

        let start_key = (start.x, start.y, start_layer);
        cost_so_far.insert(start_key, 0.0);

        let initial_h = self.heuristic(start, start_layer, target, target_layer);
        open_set.push(State {
            cost: 0.0,
            priority: initial_h,
            x: start.x,
            y: start.y,
            layer: start_layer,
        });

        // Seed initial vertical escape vias to allow routing through open inner layers immediately
        for l in 0..layer_count {
            if l != start_layer {
                let via_cost = self.settings.layer_change_cost * ((l - start_layer).abs() as f64);
                let next_key = (start.x, start.y, l);
                let h = self.heuristic(start, l, target, target_layer);
                cost_so_far.insert(next_key, via_cost);
                open_set.push(State {
                    cost: via_cost,
                    priority: via_cost + h * 1.001,
                    x: start.x,
                    y: start.y,
                    layer: l,
                });
                came_from.insert(next_key, start_key);
            }
        }

        // 8 Planar 45-degree neighbor offsets
        let planar_offsets: [(i32, i32, f64); 8] = [
            (step, 0, step as f64),
            (-step, 0, step as f64),
            (0, step, step as f64),
            (0, -step, step as f64),
            (step, step, (step as f64) * std::f64::consts::SQRT_2),
            (step, -step, (step as f64) * std::f64::consts::SQRT_2),
            (-step, step, (step as f64) * std::f64::consts::SQRT_2),
            (-step, -step, (step as f64) * std::f64::consts::SQRT_2),
        ];

        let mut expansions = 0;
        let mut best_target_key = None;
        let clear_margin = (half_width - 1).max(0);

        while let Some(State { cost, x, y, layer, .. }) = open_set.pop() {
            expansions += 1;
            if expansions > self.settings.max_expansion_nodes {
                break;
            }

            let current_pt = IntPoint::new(x, y);
            let current_key = (x, y, layer);

            // Target reached check with direct line-of-sight capture
            if layer == target_layer {
                let dist_to_target = self.distance(current_pt, target);
                if dist_to_target <= (step as f64) * 2.5 {
                    let seg_box = IntBox::new(
                        current_pt.x.min(target.x) - clear_margin,
                        current_pt.y.min(target.y) - clear_margin,
                        current_pt.x.max(target.x) + clear_margin,
                        current_pt.y.max(target.y) + clear_margin,
                    );
                    let collision = (layer as usize) < spatial_grids.len() && spatial_grids[layer as usize].collides_box(&seg_box);
                    if !collision {
                        best_target_key = Some(current_key);
                        break;
                    }
                }
            }

            // 1. Expand 8 planar 45-degree directions
            for &(dx, dy, move_cost) in &planar_offsets {
                let next_x = x + dx;
                let next_y = y + dy;
                let next_pt = IntPoint::new(next_x, next_y);
                let next_key = (next_x, next_y, layer);

                // Collision check against spatial grid with exact half_width margin
                if (layer as usize) < spatial_grids.len() {
                    let grid = &spatial_grids[layer as usize];
                    let step_box = IntBox::new(
                        x.min(next_x) - clear_margin,
                        y.min(next_y) - clear_margin,
                        x.max(next_x) + clear_margin,
                        y.max(next_y) + clear_margin,
                    );
                    if next_pt != target && next_pt != start && grid.collides_box(&step_box) {
                        continue;
                    }
                }

                // Direction change penalty
                let mut bend_penalty = 0.0;
                if let Some(&(prev_x, prev_y, prev_l)) = came_from.get(&current_key) {
                    if prev_l == layer {
                        let prev_dx = x - prev_x;
                        let prev_dy = y - prev_y;
                        if prev_dx != dx || prev_dy != dy {
                            bend_penalty = self.settings.bend_cost;
                        }
                    }
                }

                let new_cost = cost + move_cost + bend_penalty;
                if !cost_so_far.contains_key(&next_key) || new_cost < cost_so_far[&next_key] {
                    cost_so_far.insert(next_key, new_cost);
                    let h = self.heuristic(next_pt, layer, target, target_layer);
                    open_set.push(State {
                        cost: new_cost,
                        priority: new_cost + h * 1.001, // Manhattan tie-breaking
                        x: next_x,
                        y: next_y,
                        layer,
                    });
                    came_from.insert(next_key, current_key);
                }
            }

            // 2. Expand vertical layer transitions (vias)
            for &next_layer in &[layer - 1, layer + 1] {
                if next_layer >= 0 && next_layer < layer_count {
                    let next_key = (x, y, next_layer);
                    let via_cost = self.settings.layer_change_cost;

                    let mut blocked = false;
                    let l_min = layer.min(next_layer) as usize;
                    let l_max = layer.max(next_layer) as usize;
                    let via_pad_r = (step / 2).max(clear_margin);
                    let via_box = IntBox::new(x - via_pad_r, y - via_pad_r, x + via_pad_r, y + via_pad_r);
                    for l in l_min..=l_max.min(spatial_grids.len() - 1) {
                        if current_pt != start && current_pt != target && spatial_grids[l].collides_box(&via_box) {
                            blocked = true;
                            break;
                        }
                    }
                    if blocked {
                        continue;
                    }

                    let new_cost = cost + via_cost;
                    if !cost_so_far.contains_key(&next_key) || new_cost < cost_so_far[&next_key] {
                        cost_so_far.insert(next_key, new_cost);
                        let h = self.heuristic(current_pt, next_layer, target, target_layer);
                        open_set.push(State {
                            cost: new_cost,
                            priority: new_cost + h * 1.001,
                            x,
                            y,
                            layer: next_layer,
                        });
                        came_from.insert(next_key, current_key);
                    }
                }
            }
        }

        let target_key = best_target_key?;

        // Reconstruct 3D path
        let mut raw_nodes = Vec::new();
        let mut curr = target_key;
        while let Some(&prev) = came_from.get(&curr) {
            raw_nodes.push((IntPoint::new(curr.0, curr.1), curr.2));
            curr = prev;
        }
        raw_nodes.push((start, start_layer));
        raw_nodes.reverse();

        if let Some(last) = raw_nodes.last_mut() {
            last.0 = target;
        }

        let total_cost = *cost_so_far.get(&target_key).unwrap_or(&0.0);
        Some(self.split_into_segments_and_vias(raw_nodes, total_cost, spatial_grids))
    }

    #[inline(always)]
    fn distance(&self, p1: IntPoint, p2: IntPoint) -> f64 {
        let dx = (p1.x - p2.x) as f64;
        let dy = (p1.y - p2.y) as f64;
        (dx * dx + dy * dy).sqrt()
    }

    #[inline(always)]
    fn heuristic(&self, current: IntPoint, current_layer: i32, target: IntPoint, target_layer: i32) -> f64 {
        let dist = self.distance(current, target);
        let layer_dist = ((current_layer - target_layer).abs() as f64) * self.settings.layer_change_cost;
        dist + layer_dist
    }

    /// Splits a 3D node sequence into layer trace segments and via transitions with pull-tight smoothing.
    fn split_into_segments_and_vias(
        &self,
        raw_nodes: Vec<(IntPoint, i32)>,
        total_cost: f64,
        spatial_grids: &[LayerSpatialGrid],
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
                    let grid = spatial_grids.get(current_layer as usize);
                    let smoothed = self.pull_tight_smooth(current_segment_pts, grid);
                    segments.push(RouteSegment3D {
                        layer: current_layer,
                        points: smoothed,
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
            let grid = spatial_grids.get(current_layer as usize);
            let smoothed = self.pull_tight_smooth(current_segment_pts, grid);
            segments.push(RouteSegment3D {
                layer: current_layer,
                points: smoothed,
            });
        }

        RoutePath3D {
            segments,
            vias,
            total_cost,
        }
    }

    /// Pull-tight corner smoothing and collinear simplification on trace points.
    pub fn pull_tight_smooth(&self, points: Vec<IntPoint>, grid: Option<&LayerSpatialGrid>) -> Vec<IntPoint> {
        let mut pts = self.simplify_path(points);
        if pts.len() <= 2 {
            return pts;
        }

        let mut changed = true;
        let mut iterations = 0;

        while changed && iterations < 5 {
            changed = false;
            iterations += 1;

            let mut new_pts = Vec::with_capacity(pts.len());
            let mut i = 0;

            while i < pts.len() {
                if i + 2 < pts.len() {
                    let p0 = pts[i];
                    let p2 = pts[i + 2];
                    let dir = Direction::from_int_points(&p0, &p2);

                    // Check if direct connection p0 -> p2 is a valid 45-degree or orthogonal line
                    if let Some(d) = dir {
                        if d.is_orthogonal() || d.is_diagonal() {
                            let seg_box = IntBox::new(
                                p0.x.min(p2.x) - 50,
                                p0.y.min(p2.y) - 50,
                                p0.x.max(p2.x) + 50,
                                p0.y.max(p2.y) + 50,
                            );
                            let collision = grid.map_or(false, |g| g.collides_box(&seg_box));
                            if !collision {
                                new_pts.push(p0);
                                new_pts.push(p2);
                                i += 3;
                                changed = true;
                                continue;
                            }
                        }
                    }
                }
                new_pts.push(pts[i]);
                i += 1;
            }

            pts = self.simplify_path(new_pts);
        }

        pts
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
        let grid = LayerSpatialGrid::new(IntBox::new(-2000, -2000, 2000, 2000), 500);
        let route = algo.find_path_3d_grid(start, 0, target, 0, 1, 100, &[grid]).unwrap();
        assert_eq!(route.segments.len(), 1);
        assert_eq!(route.segments[0].points.first(), Some(&start));
        assert_eq!(route.segments[0].points.last(), Some(&target));
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
        let mut grid0 = LayerSpatialGrid::new(IntBox::new(-2000, -2000, 2000, 2000), 500);
        grid0.insert(IntBox::new(200, 0, 400, 600)); // Obstacle on layer 0 blocking direct path
        let grid1 = LayerSpatialGrid::new(IntBox::new(-2000, -2000, 2000, 2000), 500);

        let res = algo.find_path_3d_grid(start, 0, target, 0, 2, 100, &[grid0, grid1]);
        if res.is_none() {
            eprintln!("find_path_3d_grid failed for test_maze_search_3d_layer_transition!");
        }
        let route = res.unwrap();
        assert!(!route.vias.is_empty(), "Must generate via to bypass obstacle on layer 0");
    }
}
