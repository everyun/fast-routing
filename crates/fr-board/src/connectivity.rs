//! Incremental per-net connectivity: which items of a net are electrically
//! joined, and how many separate pieces the net is in.
//!
//! The predicate is upstream's `getNormalContacts`
//! (`drc-connectivity.md` §4.3), and it is **exact-point based, not overlap
//! based**:
//!
//! * a trace connects only at its **two end corners** — a trace crossing
//!   another mid-span is *not* connected, even on the same net and layer;
//! * a drill item (pin or via) connects at its **centre**;
//! * a conduction area connects to anything whose connection point it
//!   **contains**;
//! * both items must share a layer (`max(firstLayer) <= min(lastLayer)`);
//! * point equality is exact integer equality. §4.3 invariant 1 is explicit
//!   that there is *no* tolerance, and that v2 deliberately removed the
//!   tolerant variant because it caused false cycle detection during trace
//!   normalization. `f64` never reaches this decision.
//!
//! # Why a lazy per-net rebuild rather than an incremental union-find
//!
//! Union-find supports `union` in near-constant time but has no `split`, and
//! the router's dominant mutation is *removal* (ripup, trace split, shove).
//! Upstream sidesteps this by recomputing `getConnectedSet` by DFS at every
//! question. Here each net keeps its components cached and a **dirty flag**;
//! any mutation touching the net sets it, and the next query rebuilds that
//! one net with a union-find over its own items. Rebuilding is O(k log k) in
//! the net's item count — a sort of the connection points, not a quadratic
//! pairwise scan — so a dirty net costs about what one `getConnectedSet` walk
//! costs upstream, and every clean net costs nothing.
//!
//! # What is simplified
//!
//! §4.3 invariant 3 records an upstream *asymmetry*: the trace side of a
//! trace↔conduction-area contact tests the true area (`getArea().contains`)
//! while the area side tests one convex piece of the *tree* decomposition, so
//! the two can disagree at a hole boundary and `getConnectedSet` can depend on
//! the start item. This module tests the convex decomposition from both sides,
//! which makes the relation symmetric — a deliberate divergence, noted here
//! because a symmetric relation is what a union-find can represent at all.

use std::collections::BTreeMap;

use fr_geometry::planar::Point;

use crate::item::{Item, ItemId, ItemKind};
use crate::search_tree::placed_area_pieces;
use crate::store::ItemStore;

/// The connection points of one item, without allocating.
///
/// [`Point`] is not `Copy` (its rational variant owns bignums), so this is an
/// enum rather than an `ArrayVec`.
#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionPoints {
    /// Areas and non-connectable items have no connection *point*; a
    /// conduction area connects by containment instead.
    None,
    /// A drill item's centre.
    One(Point),
    /// A trace's two end corners.
    Two(Point, Point),
}

impl ConnectionPoints {
    /// Iterates the points, 0 to 2 of them.
    pub fn iter(&self) -> impl Iterator<Item = &Point> {
        let (first, second) = match self {
            ConnectionPoints::None => (None, None),
            ConnectionPoints::One(p) => (Some(p), None),
            ConnectionPoints::Two(a, b) => (Some(a), Some(b)),
        };
        first.into_iter().chain(second)
    }
}

/// The connection points of `item` (`drc-connectivity.md` §4.3).
pub fn connection_points(item: &Item) -> ConnectionPoints {
    match &item.kind {
        ItemKind::Trace(trace) => {
            ConnectionPoints::Two(trace.first_corner(), trace.last_corner())
        }
        ItemKind::Pin(pin) => ConnectionPoints::One(Point::Int(pin.center)),
        ItemKind::Via(via) => ConnectionPoints::One(Point::Int(via.center)),
        _ => ConnectionPoints::None,
    }
}

/// Whether `candidate` has a connection at exactly `point` — the per-candidate
/// half of `getNormalContacts`.
///
/// Deliberately does **not** check nets or layers: the caller has already
/// filtered on those, and keeping them out makes this function exactly the
/// geometric predicate.
pub fn touches_at(candidate: &Item, point: &Point) -> bool {
    match &candidate.kind {
        ItemKind::Trace(trace) => {
            trace.first_corner() == *point || trace.last_corner() == *point
        }
        ItemKind::Pin(pin) => Point::Int(pin.center) == *point,
        ItemKind::Via(via) => Point::Int(via.center) == *point,
        ItemKind::ConductionArea(area) => placed_area_pieces(&area.area)
            .iter()
            .any(|piece| piece.contains(point)),
        _ => false,
    }
}

/// Upstream `Item.sharesLayer` (`Item.java:314-318`).
pub fn shares_layer(a: &Item, b: &Item) -> bool {
    let (a, b) = (a.layer_range(), b.layer_range());
    a.first.max(b.first) <= a.last.min(b.last)
}

/// Components of one net, as of the last rebuild.
#[derive(Debug, Clone, Default)]
struct NetComponents {
    /// Set when a mutation touched the net; the next query rebuilds.
    dirty: bool,
    /// The net's connectable items, ascending by id.
    members: Vec<ItemId>,
    /// Component label per member, parallel to `members`. Labels are assigned
    /// in ascending-member order, so they are deterministic.
    labels: Vec<u32>,
    /// Number of distinct labels.
    count: usize,
}

/// Per-net connected components over the whole board.
///
/// Every net starts dirty; queries take `&mut self` because they rebuild on
/// demand.
#[derive(Debug, Clone, Default)]
pub struct Connectivity {
    nets: BTreeMap<i32, NetComponents>,
    /// Reused across rebuilds so a refresh allocates nothing after warm-up.
    scratch: Scratch,
}

#[derive(Debug, Clone, Default)]
struct Scratch {
    points: Vec<(Point, u32)>,
    parent: Vec<u32>,
    size: Vec<u32>,
    label_of: Vec<u32>,
}

impl Connectivity {
    /// An empty index; every net is considered dirty until first queried.
    pub fn new() -> Self {
        Connectivity::default()
    }

    /// Marks one net for rebuild.
    pub fn mark_dirty(&mut self, net_no: i32) {
        if net_no <= 0 {
            return;
        }
        self.nets.entry(net_no).or_default().dirty = true;
    }

    /// Marks every net of `item` for rebuild — the hook every board mutation
    /// calls.
    pub fn mark_item_dirty(&mut self, item: &Item) {
        for net in item.nets.iter().copied() {
            self.mark_dirty(net);
        }
    }

    /// Marks every known net for rebuild.
    pub fn mark_all_dirty(&mut self) {
        for components in self.nets.values_mut() {
            components.dirty = true;
        }
    }

    /// True when `net_no` needs a rebuild before its components can be read.
    ///
    /// A net that has never been queried reports `true`.
    // `Option::is_none_or` would read better but needs Rust 1.82; the
    // workspace MSRV is 1.75.
    #[allow(clippy::unnecessary_map_or)]
    pub fn is_dirty(&self, net_no: i32) -> bool {
        self.nets.get(&net_no).map_or(true, |c| c.dirty)
    }

    /// The nets currently marked dirty, ascending.
    pub fn dirty_nets(&self) -> impl Iterator<Item = i32> + '_ {
        self.nets
            .iter()
            .filter(|(_, c)| c.dirty)
            .map(|(net, _)| *net)
    }

    /// Drops every cached component, keeping the allocations.
    pub fn clear(&mut self) {
        self.nets.clear();
    }

    /// Number of connected components of `net_no`; 0 for a net with no
    /// connectable items.
    ///
    /// This is the ratsnest's denominator: a fully routed net has exactly one.
    pub fn component_count(&mut self, net_no: i32, store: &ItemStore) -> usize {
        self.refresh(net_no, store);
        self.nets.get(&net_no).map_or(0, |c| c.count)
    }

    /// Component label of one item within its net, or `None` when the item is
    /// not a connectable member of that net.
    ///
    /// Labels are assigned in ascending member-id order, so they are stable
    /// across runs and comparable between two rebuilds of an unchanged net.
    pub fn component_of(&mut self, net_no: i32, store: &ItemStore, id: ItemId) -> Option<u32> {
        self.refresh(net_no, store);
        let components = self.nets.get(&net_no)?;
        let position = components.members.binary_search(&id).ok()?;
        Some(components.labels[position])
    }

    /// The members of `net_no`, grouped by component.
    ///
    /// Groups come out in ascending label order and each group in ascending id
    /// order, so the result is a canonical form two boards can be compared by.
    pub fn components(&mut self, net_no: i32, store: &ItemStore) -> Vec<Vec<ItemId>> {
        self.refresh(net_no, store);
        let Some(components) = self.nets.get(&net_no) else {
            return Vec::new();
        };
        let mut groups = vec![Vec::new(); components.count];
        for (position, id) in components.members.iter().enumerate() {
            groups[components.labels[position] as usize].push(*id);
        }
        groups
    }

    /// The connectable members of `net_no`, ascending.
    pub fn members(&mut self, net_no: i32, store: &ItemStore) -> &[ItemId] {
        self.refresh(net_no, store);
        self.nets
            .get(&net_no)
            .map_or(&[][..], |c| c.members.as_slice())
    }

    /// True when two items of the same net are in the same component.
    pub fn are_connected(&mut self, net_no: i32, store: &ItemStore, a: ItemId, b: ItemId) -> bool {
        match (
            self.component_of(net_no, store, a),
            self.component_of(net_no, store, b),
        ) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        }
    }

    /// Rebuilds `net_no` if it is dirty.
    pub fn refresh(&mut self, net_no: i32, store: &ItemStore) {
        if net_no <= 0 {
            return;
        }
        let entry = self.nets.entry(net_no).or_insert_with(|| NetComponents {
            dirty: true,
            ..NetComponents::default()
        });
        if !entry.dirty {
            return;
        }
        let mut components = std::mem::take(entry);
        let mut scratch = std::mem::take(&mut self.scratch);
        rebuild(net_no, store, &mut components, &mut scratch);
        self.scratch = scratch;
        components.dirty = false;
        self.nets.insert(net_no, components);
    }

    /// Rebuilds every dirty net that the store knows about.
    pub fn refresh_all(&mut self, store: &ItemStore) {
        let nets: Vec<i32> = store.nets().map(|(net, _)| net).collect();
        for net in nets {
            self.refresh(net, store);
        }
    }
}

// `std::iter::repeat_n` would read better than `repeat().take()` but needs
// Rust 1.82; the workspace MSRV is 1.75.
#[allow(clippy::manual_repeat_n)]
fn rebuild(
    net_no: i32,
    store: &ItemStore,
    components: &mut NetComponents,
    scratch: &mut Scratch,
) {
    components.members.clear();
    components.labels.clear();
    components.count = 0;
    components.members.extend(
        store
            .net_items(net_no)
            .iter()
            .copied()
            .filter(|id| store.get(*id).is_some_and(|item| item.is_connectable())),
    );
    let member_count = components.members.len();
    if member_count == 0 {
        return;
    }

    scratch.parent.clear();
    scratch.size.clear();
    scratch.parent.extend(0..member_count as u32);
    scratch.size.extend(std::iter::repeat(1).take(member_count));

    // Pass 1: point-anchored contacts. Sorting the (point, member) pairs turns
    // the pairwise scan into a group walk; groups are tiny (a fork of three or
    // four items at one corner), so the O(k^2) inside a group is free while the
    // layer test it performs is exactly upstream's.
    scratch.points.clear();
    for (position, id) in components.members.iter().enumerate() {
        let Some(item) = store.get(*id) else { continue };
        let points = connection_points(item);
        for point in points.iter() {
            scratch.points.push((point.clone(), position as u32));
        }
    }
    scratch
        .points
        .sort_by(|a, b| a.0.compare_xy(&b.0).cmp(&0).then(a.1.cmp(&b.1)));

    let mut start = 0usize;
    while start < scratch.points.len() {
        let mut end = start + 1;
        while end < scratch.points.len() && scratch.points[end].0 == scratch.points[start].0 {
            end += 1;
        }
        for i in start..end {
            for j in (i + 1)..end {
                let (a, b) = (scratch.points[i].1, scratch.points[j].1);
                if a == b {
                    continue;
                }
                let (Some(item_a), Some(item_b)) = (
                    store.get(components.members[a as usize]),
                    store.get(components.members[b as usize]),
                ) else {
                    continue;
                };
                if shares_layer(item_a, item_b) {
                    union(&mut scratch.parent, &mut scratch.size, a, b);
                }
            }
        }
        start = end;
    }

    // Pass 2: conduction areas swallow every same-net connection point they
    // contain. Pours are few, so the per-area scan over the net's points is
    // cheaper than indexing them.
    for (position, id) in components.members.iter().enumerate() {
        let Some(item) = store.get(*id) else { continue };
        if !matches!(item.kind, ItemKind::ConductionArea(_)) {
            continue;
        }
        for (point, other) in &scratch.points {
            if *other == position as u32 {
                continue;
            }
            let Some(other_item) = store.get(components.members[*other as usize]) else {
                continue;
            };
            if shares_layer(item, other_item) && touches_at(item, point) {
                union(&mut scratch.parent, &mut scratch.size, position as u32, *other);
            }
        }
    }

    // Label in ascending member order so the numbering is deterministic.
    scratch.label_of.clear();
    scratch
        .label_of
        .extend(std::iter::repeat(u32::MAX).take(member_count));
    let mut next_label = 0u32;
    for position in 0..member_count {
        let root = find(&mut scratch.parent, position as u32) as usize;
        if scratch.label_of[root] == u32::MAX {
            scratch.label_of[root] = next_label;
            next_label += 1;
        }
        components.labels.push(scratch.label_of[root]);
    }
    components.count = next_label as usize;
}

fn find(parent: &mut [u32], mut node: u32) -> u32 {
    while parent[node as usize] != node {
        // Path halving: one pass, no recursion, no scratch stack.
        let grand_parent = parent[parent[node as usize] as usize];
        parent[node as usize] = grand_parent;
        node = grand_parent;
    }
    node
}

fn union(parent: &mut [u32], size: &mut [u32], a: u32, b: u32) {
    let (mut ra, mut rb) = (find(parent, a), find(parent, b));
    if ra == rb {
        return;
    }
    if size[ra as usize] < size[rb as usize] {
        std::mem::swap(&mut ra, &mut rb);
    }
    parent[rb as usize] = ra;
    size[ra as usize] += size[rb as usize];
}

#[cfg(test)]
mod tests {
    use fr_geometry::planar::{IntBox, IntPoint, Polyline};

    use super::*;
    use crate::item::{
        AreaShape, ConductionArea, FixedState, LayerRange, PlacedArea, PolylineTrace, Via,
    };
    use crate::net_nos::NetNos;

    fn trace(nets: &[i32], layer: i32, points: &[IntPoint]) -> Item {
        Item::new(
            NetNos::from_slice(nets),
            1,
            FixedState::Unfixed,
            ItemKind::Trace(PolylineTrace {
                polyline: Polyline::from_int_points(points),
                layer,
                half_width: 50,
            }),
        )
    }

    fn via(nets: &[i32], center: IntPoint, layers: LayerRange) -> Item {
        Item::new(
            NetNos::from_slice(nets),
            1,
            FixedState::Unfixed,
            ItemKind::Via(Via {
                center,
                padstack_no: 0,
                layers,
                attach_allowed: true,
                is_escape_via: false,
                escape_via_smd_layer: -1,
            }),
        )
    }

    fn pour(nets: &[i32], layer: i32, area: IntBox) -> Item {
        Item::new(
            NetNos::from_slice(nets),
            1,
            FixedState::SystemFixed,
            ItemKind::ConductionArea(ConductionArea {
                area: PlacedArea::new(AreaShape::Box(area)),
                layer,
                is_obstacle: true,
                is_filled: true,
            }),
        )
    }

    #[test]
    fn traces_meeting_end_to_end_are_one_component() {
        let mut store = ItemStore::new();
        store.insert(trace(&[1], 0, &[IntPoint::new(0, 0), IntPoint::new(100, 0)]));
        store.insert(trace(
            &[1],
            0,
            &[IntPoint::new(100, 0), IntPoint::new(100, 100)],
        ));
        let mut connectivity = Connectivity::new();
        assert_eq!(connectivity.component_count(1, &store), 1);
    }

    #[test]
    fn a_trace_crossing_another_mid_span_is_not_connected() {
        // §4.3 invariant 2: contacts are end corners only.
        let mut store = ItemStore::new();
        store.insert(trace(
            &[1],
            0,
            &[IntPoint::new(-100, 0), IntPoint::new(100, 0)],
        ));
        store.insert(trace(
            &[1],
            0,
            &[IntPoint::new(0, -100), IntPoint::new(0, 100)],
        ));
        let mut connectivity = Connectivity::new();
        assert_eq!(connectivity.component_count(1, &store), 2);
    }

    #[test]
    fn traces_on_different_layers_do_not_connect_without_a_via() {
        let mut store = ItemStore::new();
        let a = store.insert(trace(&[1], 0, &[IntPoint::new(0, 0), IntPoint::new(100, 0)]));
        let b = store.insert(trace(&[1], 1, &[IntPoint::new(100, 0), IntPoint::new(200, 0)]));
        let mut connectivity = Connectivity::new();
        assert_eq!(connectivity.component_count(1, &store), 2);
        assert!(!connectivity.are_connected(1, &store, a, b));

        let _via = store.insert(via(&[1], IntPoint::new(100, 0), LayerRange::new(0, 1)));
        connectivity.mark_dirty(1);
        assert_eq!(connectivity.component_count(1, &store), 1);
        assert!(connectivity.are_connected(1, &store, a, b));
    }

    #[test]
    fn a_pour_joins_every_item_whose_connection_point_it_contains() {
        let mut store = ItemStore::new();
        store.insert(pour(&[1], 0, IntBox::new(-1000, -1000, 1000, 1000)));
        store.insert(trace(&[1], 0, &[IntPoint::new(0, 0), IntPoint::new(500, 0)]));
        store.insert(trace(
            &[1],
            0,
            &[IntPoint::new(-500, -500), IntPoint::new(-400, -500)],
        ));
        let mut connectivity = Connectivity::new();
        assert_eq!(
            connectivity.component_count(1, &store),
            1,
            "both traces land inside the pour"
        );
    }

    #[test]
    fn a_different_net_is_never_joined() {
        let mut store = ItemStore::new();
        store.insert(trace(&[1], 0, &[IntPoint::new(0, 0), IntPoint::new(100, 0)]));
        store.insert(trace(&[2], 0, &[IntPoint::new(100, 0), IntPoint::new(200, 0)]));
        let mut connectivity = Connectivity::new();
        assert_eq!(connectivity.component_count(1, &store), 1);
        assert_eq!(connectivity.component_count(2, &store), 1);
    }

    #[test]
    fn dirty_flags_are_per_net_and_cleared_by_a_query() {
        let mut store = ItemStore::new();
        let a = store.insert(trace(&[1], 0, &[IntPoint::new(0, 0), IntPoint::new(100, 0)]));
        let mut connectivity = Connectivity::new();
        assert!(connectivity.is_dirty(1), "never queried == dirty");
        connectivity.component_count(1, &store);
        assert!(!connectivity.is_dirty(1));

        store.remove(a);
        connectivity.mark_dirty(1);
        assert!(connectivity.is_dirty(1));
        assert_eq!(connectivity.component_count(1, &store), 0);
        assert!(!connectivity.is_dirty(1));
        assert_eq!(connectivity.dirty_nets().count(), 0);
    }

    #[test]
    fn labels_are_assigned_in_ascending_member_order() {
        let mut store = ItemStore::new();
        let first = store.insert(trace(&[1], 0, &[IntPoint::new(0, 0), IntPoint::new(100, 0)]));
        let second = store.insert(trace(
            &[1],
            0,
            &[IntPoint::new(5000, 0), IntPoint::new(5100, 0)],
        ));
        let mut connectivity = Connectivity::new();
        assert_eq!(connectivity.component_of(1, &store, first), Some(0));
        assert_eq!(connectivity.component_of(1, &store, second), Some(1));
        assert_eq!(
            connectivity.components(1, &store),
            vec![vec![first], vec![second]]
        );
    }
}
