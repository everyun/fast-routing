//! Net connectivity analysis and Disjoint-Set Connected Components for multi-pin nets.

use fr_board::BasicBoard;
use fr_geometry::planar::IntPoint;

/// Disjoint-Set Union (DSU) data structure for tracking electrical connected components.
pub struct DisjointSet {
    parent: Vec<usize>,
    rank: Vec<usize>,
}

impl DisjointSet {
    pub fn new(size: usize) -> Self {
        DisjointSet {
            parent: (0..size).collect(),
            rank: vec![0; size],
        }
    }

    pub fn find(&mut self, i: usize) -> usize {
        let mut root = i;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        let mut curr = i;
        while curr != root {
            let next = self.parent[curr];
            self.parent[curr] = root;
            curr = next;
        }
        root
    }

    pub fn union(&mut self, i: usize, j: usize) -> bool {
        let root_i = self.find(i);
        let root_j = self.find(j);
        if root_i != root_j {
            if self.rank[root_i] < self.rank[root_j] {
                self.parent[root_i] = root_j;
            } else if self.rank[root_i] > self.rank[root_j] {
                self.parent[root_j] = root_i;
            } else {
                self.parent[root_j] = root_i;
                self.rank[root_i] += 1;
            }
            true
        } else {
            false
        }
    }
}

#[inline(always)]
fn euclidean_dist(p1: &IntPoint, p2: &IntPoint) -> f64 {
    let dx = (p1.x - p2.x) as f64;
    let dy = (p1.y - p2.y) as f64;
    (dx * dx + dy * dy).sqrt()
}

/// Electrical connectivity status of a net.
#[derive(Debug, Clone)]
pub struct NetConnectivityStatus {
    pub net_id: i32,
    pub total_pins: usize,
    pub num_components: usize,
    pub is_fully_connected: bool,
    /// Representatives (anchor point, layer) for each disjoint component
    pub component_anchors: Vec<(IntPoint, i32)>,
}

/// Analyzes electrical connectivity of a single net on the board.
pub fn analyze_net_connectivity(board: &BasicBoard, net_id: i32) -> NetConnectivityStatus {
    let pins = board.get_pins_for_net(net_id);
    let traces = board.get_traces_for_net(net_id);
    let vias = board.get_vias_for_net(net_id);

    let total_pins = pins.len();
    if total_pins == 0 {
        return NetConnectivityStatus {
            net_id,
            total_pins: 0,
            num_components: 0,
            is_fully_connected: true,
            component_anchors: Vec::new(),
        };
    }
    if total_pins == 1 {
        let anchor = (pins[0].center, pins[0].first_layer);
        return NetConnectivityStatus {
            net_id,
            total_pins: 1,
            num_components: 1,
            is_fully_connected: true,
            component_anchors: vec![anchor],
        };
    }

    // Build list of all conductive elements in this net
    let num_pins = pins.len();
    let num_traces = traces.len();
    let num_vias = vias.len();
    let total_elements = num_pins + num_traces + num_vias;

    let mut dsu = DisjointSet::new(total_elements);

    // 1. Check contacts between Pins and Traces
    for (p_idx, pin) in pins.iter().enumerate() {
        for (t_idx, trace) in traces.iter().enumerate() {
            let trace_elem_idx = num_pins + t_idx;
            if trace.layer >= pin.first_layer && trace.layer <= pin.last_layer {
                if let (Some(&first_pt), Some(&last_pt)) = (trace.corner_points.first(), trace.corner_points.last()) {
                    let d1 = (first_pt.x - pin.center.x).abs().max((first_pt.y - pin.center.y).abs());
                    let d2 = (last_pt.x - pin.center.x).abs().max((last_pt.y - pin.center.y).abs());
                    let threshold = pin.pad_bounding_box.width().max(pin.pad_bounding_box.height()) / 2 + trace.half_width + 50;
                    if d1 <= threshold || d2 <= threshold || first_pt.is_contained_in(&pin.pad_bounding_box) || last_pt.is_contained_in(&pin.pad_bounding_box) {
                        dsu.union(p_idx, trace_elem_idx);
                    }
                }
            }
        }
    }

    // 2. Check contacts between Pins and Vias
    for (p_idx, pin) in pins.iter().enumerate() {
        for (v_idx, via) in vias.iter().enumerate() {
            let via_elem_idx = num_pins + num_traces + v_idx;
            if via.first_layer.max(pin.first_layer) <= via.last_layer.min(pin.last_layer) {
                let dist = (via.center.x - pin.center.x).abs().max((via.center.y - pin.center.y).abs());
                if dist <= via.pad_radius + 50 || via.center.is_contained_in(&pin.pad_bounding_box) {
                    dsu.union(p_idx, via_elem_idx);
                }
            }
        }
    }

    // 3. Check contacts between Traces and Traces
    for (t1_idx, t1) in traces.iter().enumerate() {
        for (t2_idx, t2) in traces.iter().enumerate().skip(t1_idx + 1) {
            if t1.layer == t2.layer {
                let e1 = num_pins + t1_idx;
                let e2 = num_pins + t2_idx;
                if let (Some(&s1), Some(&e1_pt), Some(&s2), Some(&e2_pt)) =
                    (t1.corner_points.first(), t1.corner_points.last(), t2.corner_points.first(), t2.corner_points.last())
                {
                    let max_dist = (t1.half_width + t2.half_width + 20) as f64;
                    if euclidean_dist(&s1, &s2) <= max_dist
                        || euclidean_dist(&s1, &e2_pt) <= max_dist
                        || euclidean_dist(&e1_pt, &s2) <= max_dist
                        || euclidean_dist(&e1_pt, &e2_pt) <= max_dist
                    {
                        dsu.union(e1, e2);
                    }
                }
            }
        }
    }

    // 4. Check contacts between Traces and Vias
    for (t_idx, trace) in traces.iter().enumerate() {
        for (v_idx, via) in vias.iter().enumerate() {
            if trace.layer >= via.first_layer && trace.layer <= via.last_layer {
                let e_t = num_pins + t_idx;
                let e_v = num_pins + num_traces + v_idx;
                if let (Some(&s), Some(&e)) = (trace.corner_points.first(), trace.corner_points.last()) {
                    let max_dist = (via.pad_radius + trace.half_width + 20) as f64;
                    if euclidean_dist(&s, &via.center) <= max_dist || euclidean_dist(&e, &via.center) <= max_dist {
                        dsu.union(e_t, e_v);
                    }
                }
            }
        }
    }

    // Find distinct component roots among pins
    let mut root_to_anchor = std::collections::HashMap::new();
    for (p_idx, pin) in pins.iter().enumerate() {
        let root = dsu.find(p_idx);
        root_to_anchor.entry(root).or_insert((pin.center, pin.first_layer));
    }

    let num_components = root_to_anchor.len();
    let is_fully_connected = num_components <= 1;
    let component_anchors: Vec<(IntPoint, i32)> = root_to_anchor.into_values().collect();

    NetConnectivityStatus {
        net_id,
        total_pins,
        num_components,
        is_fully_connected,
        component_anchors,
    }
}
