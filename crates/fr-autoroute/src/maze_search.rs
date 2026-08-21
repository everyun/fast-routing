//! 45-degree Maze Search and Path Expansion Algorithm (Modified A*).

use fr_geometry::planar::{Direction, IntPoint};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

/// A node in the A* expansion priority queue.
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

/// Settings and cost factors for the maze search algorithm.
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
            layer_change_cost: 500.0,
            max_expansion_nodes: 50_000,
        }
    }
}

/// Result of a maze search connection attempt.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutePath {
    pub points: Vec<IntPoint>,
    pub layer: i32,
    pub total_cost: f64,
}

/// 45-degree maze search pathfinder.
pub struct MazeSearchAlgo {
    pub settings: MazeSearchSettings,
}

impl MazeSearchAlgo {
    pub fn new(settings: MazeSearchSettings) -> Self {
        MazeSearchAlgo { settings }
    }

    /// Finds a 45-degree obstacle-avoiding path between `start` and `target`.
    pub fn find_path(
        &self,
        start: IntPoint,
        target: IntPoint,
        layer: i32,
        obstacle_boxes: &[fr_geometry::planar::IntBox],
    ) -> Option<RoutePath> {
        let mut open_set = BinaryHeap::new();
        let mut cost_so_far: HashMap<(IntPoint, i32), f64> = HashMap::new();
        let mut came_from: HashMap<(IntPoint, i32), (IntPoint, i32)> = HashMap::new();

        let initial_est = start.distance(&target);
        open_set.push(QueueElement {
            point: start,
            layer,
            direction: Direction::NULL,
            cost_from_start: 0.0,
            estimated_total_cost: initial_est,
        });
        cost_so_far.insert((start, layer), 0.0);

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

            // Target reached (within step size)
            if current.point.distance(&target) <= self.settings.step_size as f64 {
                // Reconstruct path
                let mut path = vec![target, current.point];
                let mut curr_key = (current.point, current.layer);
                while let Some(&prev_key) = came_from.get(&curr_key) {
                    path.push(prev_key.0);
                    curr_key = prev_key;
                }
                path.reverse();

                // Simplify collinear 45° segments
                let simplified = self.simplify_path(path);
                return Some(RoutePath {
                    points: simplified,
                    layer: current.layer,
                    total_cost: current.cost_from_start,
                });
            }

            for dir in &directions {
                let step_vec = dir.get_int_vector();
                let next_pt = IntPoint::new(
                    current.point.x + step_vec.x * self.settings.step_size,
                    current.point.y + step_vec.y * self.settings.step_size,
                );

                // Check collision with obstacle boxes
                let collides = obstacle_boxes.iter().any(|b| next_pt.is_contained_in(b));
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
                    let priority = new_cost + next_pt.distance(&target);
                    open_set.push(QueueElement {
                        point: next_pt,
                        layer: current.layer,
                        direction: *dir,
                        cost_from_start: new_cost,
                        estimated_total_cost: priority,
                    });
                }
            }
        }

        None
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
}
