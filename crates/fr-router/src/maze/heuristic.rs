//! `DestinationDistance` — the admissible A\* lower bound.
//!
//! A verbatim transcription of `autoroute/DestinationDistance.java`, which the
//! conformance appendix records as *"byte-for-byte equivalent to v19 modulo
//! formatting"*. Three aggregate destination boxes (component side, solder
//! side, everything between) and a minimum over route sketches with 0..3 vias.
//!
//! Two deliberate departures, both noted by `maze-engine.md` §13:
//!
//! * `calculate_cheap_distance` mutates `min_normal_via_cost` in place and
//!   restores it (`DestinationDistance.java:346-353`). Here the via cost is a
//!   parameter of [`DestinationDistance::calculate_with_via_cost`] instead, so
//!   the type stays `&self` and is trivially shareable.
//! * The result when nothing was joined is `i32::MAX as f64`, exactly
//!   upstream's `Integer.MAX_VALUE`, so the value is comparable across the two
//!   implementations in the settle trace.

use fr_geometry::planar::{FloatPoint, IntBox};

use crate::ctrl::ExpansionCostFactor;

/// The A\* heuristic for one connection.
#[derive(Debug, Clone)]
pub struct DestinationDistance {
    trace_costs: Vec<ExpansionCostFactor>,
    layer_count: usize,
    active_layer_count: usize,
    min_normal_via_cost: f64,
    min_cheap_via_cost: f64,

    min_component_side_trace_cost: f64,
    min_solder_side_trace_cost: f64,
    min_component_inner_trace_cost: f64,
    min_solder_inner_trace_cost: f64,
    min_component_solder_inner_trace_cost: f64,

    component_side_box: IntBox,
    solder_side_box: IntBox,
    inner_side_box: IntBox,
    box_is_empty: bool,
    component_side_box_is_empty: bool,
    solder_side_box_is_empty: bool,
    inner_side_box_is_empty: bool,
}

/// Upstream's `Integer.MAX_VALUE` sentinel for "no destination joined".
pub const NO_DESTINATION: f64 = i32::MAX as f64;

impl DestinationDistance {
    /// `DestinationDistance(trace_costs, layer_active, min_normal, min_cheap)`
    /// (`:43-92`).
    pub fn new(
        trace_costs: &[ExpansionCostFactor],
        layer_active: &[bool],
        min_normal_via_cost: f64,
        min_cheap_via_cost: f64,
    ) -> DestinationDistance {
        let layer_count = layer_active.len();
        let active_layer_count = layer_active.iter().filter(|active| **active).count();

        let mut min_component_side_trace_cost = 0.0;
        let mut max_component_side_trace_cost = 0.0;
        if layer_count > 0 && layer_active[0] {
            let cost = trace_costs[0];
            if cost.horizontal < cost.vertical {
                min_component_side_trace_cost = cost.horizontal;
                max_component_side_trace_cost = cost.vertical;
            } else {
                min_component_side_trace_cost = cost.vertical;
                max_component_side_trace_cost = cost.horizontal;
            }
        }

        let mut min_solder_side_trace_cost = 0.0;
        let mut max_solder_side_trace_cost = 0.0;
        if layer_count > 0 && layer_active[layer_count - 1] {
            let cost = trace_costs[layer_count - 1];
            if cost.horizontal < cost.vertical {
                min_solder_side_trace_cost = cost.horizontal;
                max_solder_side_trace_cost = cost.vertical;
            } else {
                min_solder_side_trace_cost = cost.vertical;
                max_solder_side_trace_cost = cost.horizontal;
            }
        }

        // "for inner layers we assume, that cost in preferred direction is 1"
        // (`:79-88`).
        let mut max_inner_side_trace_cost =
            max_component_side_trace_cost.min(max_solder_side_trace_cost);
        for layer in 1..layer_count.saturating_sub(1) {
            if !layer_active[layer] {
                continue;
            }
            let cost = trace_costs[layer];
            max_inner_side_trace_cost =
                max_inner_side_trace_cost.min(cost.horizontal.max(cost.vertical));
        }
        let min_component_inner_trace_cost =
            min_component_side_trace_cost.min(max_inner_side_trace_cost);
        let min_solder_inner_trace_cost = min_solder_side_trace_cost.min(max_inner_side_trace_cost);
        let min_component_solder_inner_trace_cost =
            min_component_inner_trace_cost.min(min_solder_inner_trace_cost);

        DestinationDistance {
            trace_costs: trace_costs.to_vec(),
            layer_count,
            active_layer_count,
            min_normal_via_cost,
            min_cheap_via_cost,
            min_component_side_trace_cost,
            min_solder_side_trace_cost,
            min_component_inner_trace_cost,
            min_solder_inner_trace_cost,
            min_component_solder_inner_trace_cost,
            component_side_box: IntBox::EMPTY,
            solder_side_box: IntBox::EMPTY,
            inner_side_box: IntBox::EMPTY,
            box_is_empty: true,
            component_side_box_is_empty: true,
            solder_side_box_is_empty: true,
            inner_side_box_is_empty: true,
        }
    }

    /// `join(IntBox, layer)` (`:94-106`): folds one destination tile shape's
    /// bounding box into the aggregate box of its layer class.
    pub fn join(&mut self, boundary: &IntBox, layer: i32) {
        if layer == 0 {
            self.component_side_box = self.component_side_box.union(boundary);
            self.component_side_box_is_empty = false;
        } else if layer as usize == self.layer_count.saturating_sub(1) {
            self.solder_side_box = self.solder_side_box.union(boundary);
            self.solder_side_box_is_empty = false;
        } else {
            self.inner_side_box = self.inner_side_box.union(boundary);
            self.inner_side_box_is_empty = false;
        }
        self.box_is_empty = false;
    }

    /// True once at least one destination has been joined; `init` fails the
    /// whole search otherwise (`MazeSearchAlgo.java:1240-1244`).
    #[inline]
    pub fn has_destination(&self) -> bool {
        !self.box_is_empty
    }

    /// `calculate(FloatPoint, layer)` (`:108-110`).
    #[inline]
    pub fn calculate(&self, point: &FloatPoint, layer: i32) -> f64 {
        self.calculate_box(&point.bounding_box(), layer)
    }

    /// `calculate(IntBox, layer)` (`:112-344`).
    pub fn calculate_box(&self, query: &IntBox, layer: i32) -> f64 {
        self.calculate_inner(query, layer, self.min_normal_via_cost)
    }

    /// `calculate_cheap_distance` (`:346-353`) without the field swap: the
    /// via cost is the parameter, and the batch ordering passes
    /// [`min_cheap_via_cost`](Self::min_cheap_via_cost).
    #[inline]
    pub fn calculate_with_via_cost(&self, query: &IntBox, layer: i32, via_cost: f64) -> f64 {
        self.calculate_inner(query, layer, via_cost)
    }

    /// `0.8 × min_normal_via_cost`, as the control computed it.
    #[inline]
    pub fn min_cheap_via_cost(&self) -> f64 {
        self.min_cheap_via_cost
    }

    fn calculate_inner(&self, query: &IntBox, layer: i32, min_normal_via_cost: f64) -> f64 {
        if self.box_is_empty {
            return NO_DESTINATION;
        }

        let (component_dx, component_dy) = axis_deltas(query, &self.component_side_box);
        let (solder_dx, solder_dy) = axis_deltas(query, &self.solder_side_box);
        let (inner_dx, inner_dy) = axis_deltas(query, &self.inner_side_box);

        let (component_max, component_min) = max_min(component_dx, component_dy);
        let (solder_max, solder_min) = max_min(solder_dx, solder_dy);
        let (inner_max, inner_min) = max_min(inner_dx, inner_dy);

        let mut result = NO_DESTINATION;

        if layer == 0 {
            if !self.component_side_box_is_empty {
                result = weighted_box_distance(
                    query,
                    &self.component_side_box,
                    self.trace_costs[0].horizontal,
                    self.trace_costs[0].vertical,
                );
            }
            if self.active_layer_count <= 1 {
                return result;
            }
            let tmp = if self.min_solder_side_trace_cost < self.min_component_side_trace_cost {
                self.min_solder_side_trace_cost * solder_max
                    + self.min_component_side_trace_cost * solder_min
                    + min_normal_via_cost
            } else {
                self.min_component_side_trace_cost * solder_max
                    + self.min_solder_side_trace_cost * solder_min
                    + min_normal_via_cost
            };
            result = result.min(tmp);

            let tmp = component_max
                + component_min * self.min_component_inner_trace_cost
                + 2.0 * min_normal_via_cost;
            result = result.min(tmp);

            if self.active_layer_count == 2 {
                return result;
            }

            let tmp =
                inner_max + inner_min * self.min_component_inner_trace_cost + min_normal_via_cost;
            result = result.min(tmp);

            let tmp = solder_max
                + self.min_component_solder_inner_trace_cost * solder_min
                + 2.0 * min_normal_via_cost;
            result = result.min(tmp);

            let tmp = component_max + component_min + 2.0 * min_normal_via_cost;
            result = result.min(tmp);

            if self.active_layer_count == 3 {
                return result;
            }

            let tmp = inner_max + inner_min + 2.0 * min_normal_via_cost;
            result = result.min(tmp);

            let tmp = solder_max + solder_min + 3.0 * min_normal_via_cost;
            return result.min(tmp);
        }

        if self.layer_count > 0 && layer as usize == self.layer_count - 1 {
            if !self.solder_side_box_is_empty {
                result = weighted_box_distance(
                    query,
                    &self.solder_side_box,
                    self.trace_costs[layer as usize].horizontal,
                    self.trace_costs[layer as usize].vertical,
                );
            }
            let tmp = if self.min_component_side_trace_cost < self.min_solder_side_trace_cost {
                self.min_component_side_trace_cost * component_max
                    + self.min_solder_side_trace_cost * component_min
                    + min_normal_via_cost
            } else {
                self.min_solder_side_trace_cost * component_max
                    + self.min_component_side_trace_cost * component_min
                    + min_normal_via_cost
            };
            result = result.min(tmp);

            let tmp = solder_max
                + solder_min * self.min_solder_inner_trace_cost
                + 2.0 * min_normal_via_cost;
            result = result.min(tmp);

            if self.active_layer_count <= 2 {
                return result;
            }

            let tmp =
                inner_min * self.min_solder_inner_trace_cost + inner_max + min_normal_via_cost;
            result = result.min(tmp);

            let tmp = component_max
                + self.min_component_solder_inner_trace_cost * component_min
                + 2.0 * min_normal_via_cost;
            result = result.min(tmp);

            let tmp = solder_max + solder_min + 2.0 * min_normal_via_cost;
            result = result.min(tmp);

            if self.active_layer_count == 3 {
                return result;
            }

            let tmp = inner_max + inner_min + 2.0 * min_normal_via_cost;
            result = result.min(tmp);

            let tmp = component_max + component_min + 3.0 * min_normal_via_cost;
            return result.min(tmp);
        }

        // Inner layer.
        if !self.inner_side_box_is_empty {
            let cost = self.trace_costs[layer as usize];
            result =
                weighted_box_distance(query, &self.inner_side_box, cost.horizontal, cost.vertical);
        }
        let tmp = inner_max + inner_min + min_normal_via_cost;
        result = result.min(tmp);
        let tmp = component_max
            + component_min * self.min_component_inner_trace_cost
            + min_normal_via_cost;
        result = result.min(tmp);
        let tmp = solder_max + solder_min * self.min_solder_inner_trace_cost + min_normal_via_cost;
        result = result.min(tmp);
        let tmp = component_max + component_min + 2.0 * min_normal_via_cost;
        result = result.min(tmp);
        let tmp = solder_max + solder_min + 2.0 * min_normal_via_cost;
        result.min(tmp)
    }
}

/// The per-axis gap between two boxes, `0` when they overlap on that axis
/// (`DestinationDistance.java:117-172`, repeated three times there).
fn axis_deltas(query: &IntBox, target: &IntBox) -> (f64, f64) {
    let dx = if query.left_x() > target.right_x() {
        (query.left_x() - target.right_x()) as f64
    } else if query.right_x() < target.left_x() {
        (target.left_x() - query.right_x()) as f64
    } else {
        0.0
    };
    let dy = if query.bottom_y() > target.top_y() {
        (query.bottom_y() - target.top_y()) as f64
    } else if query.top_y() < target.bottom_y() {
        (target.bottom_y() - query.top_y()) as f64
    } else {
        0.0
    };
    (dx, dy)
}

#[inline]
fn max_min(dx: f64, dy: f64) -> (f64, f64) {
    if dx > dy {
        (dx, dy)
    } else {
        (dy, dx)
    }
}

/// `IntBox.weighted_distance(IntBox, h, v)`: the axis gaps priced with the
/// per-layer cost factors.
fn weighted_box_distance(query: &IntBox, target: &IntBox, horizontal: f64, vertical: f64) -> f64 {
    let (dx, dy) = axis_deltas(query, target);
    horizontal * dx + vertical * dy
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_costs(layer_count: usize) -> Vec<ExpansionCostFactor> {
        vec![
            ExpansionCostFactor {
                horizontal: 1.0,
                vertical: 1.0
            };
            layer_count
        ]
    }

    #[test]
    fn no_destination_returns_the_int_max_sentinel() {
        let distance = DestinationDistance::new(&unit_costs(2), &[true, true], 50.0, 40.0);
        assert!(!distance.has_destination());
        assert_eq!(
            distance.calculate(&FloatPoint::new(0.0, 0.0), 0),
            NO_DESTINATION
        );
    }

    #[test]
    fn same_layer_distance_is_the_weighted_box_gap() {
        let mut distance = DestinationDistance::new(&unit_costs(2), &[true, false], 50.0, 40.0);
        distance.join(&IntBox::new(1000, 0, 1100, 100), 0);
        // Only layer 0 is active, so the `active_layer_count <= 1` early-out
        // returns the pure one-layer term (`:218-220`).
        let value = distance.calculate(&FloatPoint::new(0.0, 50.0), 0);
        assert!((value - 1000.0).abs() < 1e-6, "got {value}");
    }

    #[test]
    fn a_destination_on_the_other_side_costs_at_least_one_via() {
        let mut distance = DestinationDistance::new(&unit_costs(2), &[true, true], 50.0, 40.0);
        distance.join(&IntBox::new(0, 0, 100, 100), 1);
        // Query sits inside the destination box in x/y but on layer 0: the
        // cheapest sketch is "one via, zero travel".
        let value = distance.calculate(&FloatPoint::new(50.0, 50.0), 0);
        assert!((value - 50.0).abs() < 1e-6, "got {value}");
    }

    #[test]
    fn the_heuristic_never_exceeds_the_true_single_layer_cost() {
        // Admissibility spot check: with a 2.0 vertical cost the heuristic must
        // stay at or below the exact weighted distance.
        let costs = vec![
            ExpansionCostFactor {
                horizontal: 1.0,
                vertical: 2.0,
            },
            ExpansionCostFactor {
                horizontal: 2.0,
                vertical: 1.0,
            },
        ];
        let mut distance = DestinationDistance::new(&costs, &[true, true], 50.0, 40.0);
        distance.join(&IntBox::new(500, 700, 500, 700), 0);
        let exact = 1.0 * 500.0 + 2.0 * 700.0;
        let value = distance.calculate(&FloatPoint::new(0.0, 0.0), 0);
        assert!(value <= exact + 1e-6, "{value} must not exceed {exact}");
    }

    #[test]
    fn cheap_distance_only_swaps_the_via_cost() {
        let mut distance = DestinationDistance::new(&unit_costs(2), &[true, true], 50.0, 40.0);
        distance.join(&IntBox::new(0, 0, 100, 100), 1);
        let query = IntBox::new(50, 50, 50, 50);
        let normal = distance.calculate_box(&query, 0);
        let cheap = distance.calculate_with_via_cost(&query, 0, distance.min_cheap_via_cost());
        assert!((normal - 50.0).abs() < 1e-6);
        assert!((cheap - 40.0).abs() < 1e-6);
    }
}
