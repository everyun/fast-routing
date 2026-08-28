//! Connectivity and the ratsnest: what is electrically joined, and how many
//! connections are still missing.
//!
//! Port of `Item.getNormalContacts` / `getConnectedSet` and
//! `drc/NetIncompletes` (`drc-connectivity.md` §4), in the v2 shape the release
//! jar runs.
//!
//! # The contact predicate
//!
//! Exact-point, never overlap (§4.3): a trace connects only at its two end
//! corners, a pin or via only at its centre, a conduction area by containing the
//! other item's point. Integer equality, no tolerance — v2 removed the one
//! tolerant variant it had because it produced false cycles. Two conduction
//! areas are therefore *never* direct contacts of each other, however much
//! copper they share.
//!
//! Two deliberate simplifications relative to Java, both documented as quirks in
//! the spec:
//!
//! * Containment in a conduction area is tested against the **tile shapes** of
//!   the area on both sides of the relation. Upstream tests the true
//!   area-with-holes on the trace side and one convex tree piece on the area
//!   side (§4.3 invariant 3), which is asymmetric and makes `getConnectedSet`
//!   depend on the item it started from. Using the convex decomposition on both
//!   sides makes contact a symmetric relation, so the connected sets are a
//!   partition. The two disagree only for a point exactly on a hole boundary.
//! * The closure is a BFS over a contact adjacency built **once** per pass, not
//!   `getNormalContacts()` re-queried at every node visit (§10.2). Same result,
//!   `O(V + E)` instead of `O(V · log n)` R-tree probes.
//!
//! # The ratsnest node set
//!
//! v2 filters the net's items before triangulating (§4.6 step 1): dangling
//! traces and non-transiting vias are dropped, because a dangling item is a DRC
//! *violation*, not an unrouted *connection*. A trace that survives the filter
//! has both ends contacted, and `Trace.getRatsnestCorners()` returns only
//! *un*contacted ends — so **traces never contribute a ratsnest node in v2**.
//! The node set is exactly
//!
//! ```text
//! { Pins of the net } ∪ { non-tail Vias } ∪ { ConductionAreas }
//! ```
//!
//! and the airline count is (number of connected groups holding at least one
//! node) − 1. Airline *geometry* is not a parity target (§10.1), so this module
//! uses an exact minimum spanning tree over the contracted groups with an
//! integer distance and a total tie-break, rather than reproducing upstream's
//! seed-99 shuffled Delaunay triangulation. The count is identical: a spanning
//! tree of a complete graph merges every group exactly once.

use std::collections::BTreeMap;

use fr_board::item::{Item, ItemId, ItemKind};
use fr_geometry::planar::{IntPoint, Point};

use crate::anchors::AnchorIndex;
use crate::clearance::shares_layer;
use crate::shape::TileShape;
use crate::view::{BoardView, IdIndex};

/// A missing connection: the shortest link the ratsnest draws between two
/// still-separate groups of one net.
#[derive(Debug, Clone, PartialEq)]
pub struct AirLine {
    /// The net this airline belongs to.
    pub net: i32,
    /// Item at one end.
    pub from_item: ItemId,
    /// The ratsnest corner on `from_item`.
    pub from: IntPoint,
    /// Item at the other end.
    pub to_item: ItemId,
    /// The ratsnest corner on `to_item`.
    pub to: IntPoint,
}

/// The ratsnest verdict for one net.
#[derive(Debug, Clone, PartialEq)]
pub struct NetConnectivity {
    /// The net number.
    pub net: i32,
    /// Upstream's `connectedGroupCount`: connected groups among the *filtered*
    /// items of the net. A group of traces with no pin, via or plane in it is
    /// counted here but can never be linked by an airline.
    pub components: usize,
    /// Connected groups holding at least one ratsnest node.
    pub node_components: usize,
    /// `max(0, node_components - 1)` — the net's contribution to the `N` in
    /// `"(N unrouted and M violations)"`.
    pub incompletes: usize,
    /// The MST edges, one per incomplete.
    pub airlines: Vec<AirLine>,
    /// `max(0, #(Pin | ConductionArea) of this net - 1)`, the net's share of
    /// `connections.maximumCount` (§4.7).
    pub max_connections: usize,
}

/// The ratsnest verdict for a whole board.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BoardConnectivity {
    /// Per net, ascending by net number.
    pub nets: Vec<NetConnectivity>,
    /// `Σ incompletes` — the `N` in the batch log line.
    pub incomplete_count: usize,
    /// `Σ max_connections` — the denominator of the normalized score.
    pub max_connections: usize,
}

/// The contact graph: who touches whom, plus the two derived predicates the
/// ratsnest filter needs.
///
/// Built once per board state; every net's connectivity is a BFS over it.
#[derive(Debug, Clone)]
pub struct ContactGraph {
    ids: Vec<ItemId>,
    index: IdIndex,
    /// CSR adjacency over dense item indices, each list sorted ascending.
    adj_start: Vec<u32>,
    adj: Vec<u32>,
    /// Per item, the number of contacts at `[first corner, last corner]`.
    /// Meaningful for traces only.
    end_contacts: Vec<[u32; 2]>,
    /// `Trace.isTail()` / `Via.isTail()`, precomputed.
    tail: Vec<bool>,
}

impl ContactGraph {
    /// Builds the contact graph of a whole board.
    pub fn build<V: BoardView + ?Sized>(view: &V) -> ContactGraph {
        ContactGraph::build_with(view, &AnchorIndex::build(view))
    }

    /// [`ContactGraph::build`] reusing an [`AnchorIndex`] the caller already
    /// built — the clearance checker needs one too, for the tie-pin exemption.
    pub fn build_with<V: BoardView + ?Sized>(view: &V, anchors: &AnchorIndex) -> ContactGraph {
        let ids: Vec<ItemId> = view.item_ids().to_vec();
        let n = ids.len();
        let mut edges: Vec<(u32, u32)> = Vec::new();
        let mut end_contacts = vec![[0u32; 2]; n];

        // (1) Coincident anchors: trace end <-> trace end, trace end <-> drill
        // centre, drill centre <-> drill centre. Every anchor pair sitting at
        // exactly the same point contacts iff the items share a net and a layer.
        let all = anchors.all();
        let mut run_start = 0usize;
        while run_start < all.len() {
            let mut run_end = run_start + 1;
            while run_end < all.len() && all[run_end].point.compare_xy(&all[run_start].point) == 0 {
                run_end += 1;
            }
            for i in run_start..run_end {
                for j in (i + 1)..run_end {
                    let (a, b) = (&all[i], &all[j]);
                    if a.item == b.item {
                        continue;
                    }
                    let (ia, ib) = (
                        view.item(ids[a.item as usize]),
                        view.item(ids[b.item as usize]),
                    );
                    if !ia.shares_net(ib) || !shares_layer(ia, ib) {
                        continue;
                    }
                    edges.push((a.item, b.item));
                    edges.push((b.item, a.item));
                    if let Some(e) = a.kind.trace_end() {
                        end_contacts[a.item as usize][e] += 1;
                    }
                    if let Some(e) = b.kind.trace_end() {
                        end_contacts[b.item as usize][e] += 1;
                    }
                }
            }
            run_start = run_end;
        }

        // (2) Conduction areas contain anchors. Prefiltered by the area's tile
        // shape bounding box through the anchor array's x ordering.
        for (dense, id) in ids.iter().enumerate() {
            let area_item = view.item(*id);
            if !matches!(area_item.kind, ItemKind::ConductionArea(_)) {
                continue;
            }
            for piece in view.shapes(*id) {
                let bbox = piece.shape.bounding_box();
                if bbox.is_empty() {
                    continue;
                }
                for anchor in anchors.in_x_range(bbox.ll.x, bbox.ur.x) {
                    if anchor.item as usize == dense {
                        continue;
                    }
                    let other = view.item(ids[anchor.item as usize]);
                    if !area_item.shares_net(other) || !shares_layer(area_item, other) {
                        continue;
                    }
                    if !piece.shape.contains_point(&anchor.point) {
                        continue;
                    }
                    edges.push((dense as u32, anchor.item));
                    edges.push((anchor.item, dense as u32));
                    if let Some(e) = anchor.kind.trace_end() {
                        end_contacts[anchor.item as usize][e] += 1;
                    }
                }
            }
        }

        edges.sort_unstable();
        edges.dedup();
        let mut adj_start = vec![0u32; n + 1];
        for (a, _) in &edges {
            adj_start[*a as usize + 1] += 1;
        }
        for i in 0..n {
            adj_start[i + 1] += adj_start[i];
        }
        let adj: Vec<u32> = edges.iter().map(|(_, b)| *b).collect();

        let index = IdIndex::build(&ids);
        let mut graph = ContactGraph {
            ids,
            index,
            adj_start,
            adj,
            end_contacts,
            tail: vec![false; n],
        };
        for dense in 0..n {
            graph.tail[dense] = graph.compute_tail(view, dense);
        }
        graph
    }

    /// The items in contact with `id`, in the view's own item order — which for
    /// [`StoreView`](crate::StoreView) is descending by id, the order upstream's
    /// `TreeSet<Item>` contact sets iterate in. Deterministic, and no result in
    /// this crate depends on it.
    pub fn contacts(&self, id: ItemId) -> impl Iterator<Item = ItemId> + '_ {
        let range = match self.index.dense(id) {
            Some(d) => self.adj_start[d] as usize..self.adj_start[d + 1] as usize,
            None => 0..0,
        };
        self.adj[range].iter().map(|d| self.ids[*d as usize])
    }

    /// Upstream's `isTail()`.
    pub fn is_tail(&self, id: ItemId) -> bool {
        self.index.dense(id).map(|d| self.tail[d]).unwrap_or(false)
    }

    fn neighbours(&self, dense: usize) -> &[u32] {
        &self.adj[self.adj_start[dense] as usize..self.adj_start[dense + 1] as usize]
    }

    /// `Trace.isTail()` = either end uncontacted (Trace.java:212-219).
    /// `Via.isTail()` = at most one contact, or every contact spans the identical
    /// layer pair, i.e. the via changes no layer (Via.java:170-187).
    /// Everything else is never a tail.
    fn compute_tail<V: BoardView + ?Sized>(&self, view: &V, dense: usize) -> bool {
        let item = view.item(self.ids[dense]);
        match &item.kind {
            ItemKind::Trace(_) => {
                let ends = self.end_contacts[dense];
                ends[0] == 0 || ends[1] == 0
            }
            ItemKind::Via(_) => {
                let n = self.neighbours(dense);
                if n.len() <= 1 {
                    return true;
                }
                let first = view.item(self.ids[n[0] as usize]).layer_range();
                n.iter().all(|d| {
                    let r = view.item(self.ids[*d as usize]).layer_range();
                    r.first == first.first && r.last == first.last
                })
            }
            _ => false,
        }
    }
}

/// The ratsnest for one net.
///
/// Builds a [`ContactGraph`] for the whole board; use
/// [`board_connectivity`] when more than one net is wanted.
pub fn connectivity<V: BoardView + ?Sized>(view: &V, net: i32) -> NetConnectivity {
    let graph = ContactGraph::build(view);
    net_connectivity(view, &graph, net)
}

/// Every net's ratsnest, plus the two board-level totals the score needs.
///
/// The per-net membership is bucketed in **one** pass over the item list rather
/// than rescanning the board once per net, and the union-find array is reused
/// across nets (only the members a net touched are reset). Upstream rebuilds
/// both per net and runs the whole thing six times per routing pass
/// (`drc-connectivity.md` §3.2, §6); this is the cheap shape of the same result.
pub fn board_connectivity<V: BoardView + ?Sized>(view: &V) -> BoardConnectivity {
    let graph = ContactGraph::build(view);
    // BTreeMap and not HashMap: this is iterated, and the net order decides the
    // order of `BoardConnectivity::nets`.
    let mut by_net: BTreeMap<i32, Vec<usize>> = BTreeMap::new();
    for (dense, id) in view.item_ids().iter().enumerate() {
        let item = view.item(*id);
        if !item.is_connectable() {
            continue;
        }
        for n in item.nets.iter() {
            if *n > 0 {
                let members = by_net.entry(*n).or_default();
                // An item whose net array lists the same net twice must still
                // be one member. Upstream's `calculateAllIncompletes` pushes
                // once per array slot and so double-counts it in
                // `maxConnections`; the array is ascending in `dense`, so the
                // duplicate is always the previous entry.
                if members.last() != Some(&dense) {
                    members.push(dense);
                }
            }
        }
    }

    let mut parent: Vec<usize> = (0..graph.ids.len()).collect();
    let mut result = BoardConnectivity::default();
    for (net, members) in &by_net {
        let nc = net_connectivity_of(view, &graph, *net, members, &mut parent);
        result.incomplete_count += nc.incompletes;
        result.max_connections += nc.max_connections;
        result.nets.push(nc);
    }
    result
}

/// The ratsnest for one net, over a prebuilt contact graph.
pub fn net_connectivity<V: BoardView + ?Sized>(
    view: &V,
    graph: &ContactGraph,
    net: i32,
) -> NetConnectivity {
    // The net's connectable items, in the view's order.
    let members: Vec<usize> = (0..graph.ids.len())
        .filter(|d| {
            let item = view.item(graph.ids[*d]);
            item.is_connectable() && item.contains_net(net)
        })
        .collect();
    let mut parent: Vec<usize> = (0..graph.ids.len()).collect();
    net_connectivity_of(view, graph, net, &members, &mut parent)
}

/// The body of [`net_connectivity`], with the membership already resolved and
/// the union-find array supplied by the caller.
///
/// `parent` must be the identity on entry; it is restored to the identity on
/// every index this call touched before returning, so one array serves the whole
/// board.
fn net_connectivity_of<V: BoardView + ?Sized>(
    view: &V,
    graph: &ContactGraph,
    net: i32,
    members: &[usize],
    parent: &mut [usize],
) -> NetConnectivity {
    debug_assert!(members.iter().all(|d| parent[*d] == *d));

    let max_connections = members
        .iter()
        .filter(|d| {
            matches!(
                view.item(graph.ids[**d]).kind,
                ItemKind::Pin(_) | ItemKind::ConductionArea(_)
            )
        })
        .count()
        .saturating_sub(1);

    // Union-find over the net's items. `getConnectedSet(netNumber)` walks
    // contacts that contain the net, so items of other nets are not bridges —
    // but items the ratsnest *filter* later drops still are, which is why the
    // closure runs over `members` and not over the filtered set.
    for &d in members {
        for &nb in graph.neighbours(d) {
            let nb = nb as usize;
            let other = view.item(graph.ids[nb]);
            if other.is_connectable() && other.contains_net(net) {
                union(parent, d, nb);
            }
        }
    }

    // Step 1 of NetIncompletes: drop tails and isolated non-drill items.
    let filtered: Vec<usize> = members
        .iter()
        .copied()
        .filter(|d| {
            if graph.tail[*d] {
                return false;
            }
            let item = view.item(graph.ids[*d]);
            match &item.kind {
                ItemKind::ConductionArea(_) | ItemKind::Pin(_) | ItemKind::Via(_) => true,
                _ => !graph.neighbours(*d).is_empty(),
            }
        })
        .collect();

    let groups: Vec<usize> = filtered.iter().map(|d| find(parent, *d)).collect();
    let mut distinct = groups.clone();
    distinct.sort_unstable();
    distinct.dedup();
    let components = distinct.len();

    // Step 3/4 replacement: the node set, grouped, then an exact MST over the
    // contracted groups. Only Pins, non-tail Vias and ConductionAreas are nodes.
    let mut node_groups: Vec<Vec<(ItemId, IntPoint)>> = Vec::new();
    let mut group_slot: Vec<(usize, usize)> = Vec::new(); // (root, index into node_groups)
    for (pos, &d) in filtered.iter().enumerate() {
        let id = graph.ids[d];
        let mut corners: Vec<IntPoint> = Vec::new();
        ratsnest_corners(view, id, &mut corners);
        if corners.is_empty() {
            continue;
        }
        let root = groups[pos];
        let slot = match group_slot.iter().find(|(r, _)| *r == root) {
            Some((_, s)) => *s,
            None => {
                node_groups.push(Vec::new());
                group_slot.push((root, node_groups.len() - 1));
                node_groups.len() - 1
            }
        };
        node_groups[slot].extend(corners.into_iter().map(|c| (id, c)));
    }

    // Hand the shared union-find array back as the identity.
    for &d in members {
        parent[d] = d;
    }

    let node_components = node_groups.len();
    let airlines = minimum_spanning_airlines(net, &node_groups);
    NetConnectivity {
        net,
        components,
        node_components,
        incompletes: node_components.saturating_sub(1),
        airlines,
        max_connections,
    }
}

/// `Item.getRatsnestCorners()` for the item classes that have any.
///
/// A drill item contributes its centre; a conduction area contributes the
/// corners of its convex decomposition (upstream uses the polygon's own corners
/// — a different point set, but all of them belong to the same item and
/// therefore the same group, so the airline *count* is unaffected). A trace
/// contributes nothing: after the v2 filter both of its ends are contacted.
fn ratsnest_corners<V: BoardView + ?Sized>(view: &V, id: ItemId, out: &mut Vec<IntPoint>) {
    let item = view.item(id);
    match &item.kind {
        ItemKind::Pin(p) => out.push(p.center),
        ItemKind::Via(v) => out.push(v.center),
        ItemKind::ConductionArea(_) => {
            for piece in view.shapes(id) {
                match &piece.shape {
                    TileShape::Box(b) => {
                        for i in 0..4 {
                            out.push(b.corner(i));
                        }
                    }
                    TileShape::Octagon(o) => {
                        for i in 0..8 {
                            out.push(o.corner(i));
                        }
                    }
                    TileShape::Simplex(s) => {
                        for c in s.corner_approx_arr() {
                            out.push(c.round());
                        }
                    }
                }
            }
            out.sort_unstable_by_key(|p| (p.x, p.y));
            out.dedup();
        }
        _ => {}
    }
}

/// An exact minimum spanning tree over the group-contracted node graph.
///
/// Prim, with the edge weight the **integer** squared Euclidean distance
/// (coordinates are bounded by `2^25`, so `dx² + dy²` fits an `i64` exactly —
/// no `f64` decides which airline is drawn) and a full lexicographic tie-break
/// so the output is a function of the input alone.
///
/// The number of edges returned is always `groups - 1`: a spanning tree of a
/// complete graph. That is the only property the score depends on.
fn minimum_spanning_airlines(net: i32, groups: &[Vec<(ItemId, IntPoint)>]) -> Vec<AirLine> {
    if groups.len() <= 1 {
        return Vec::new();
    }
    #[derive(Clone)]
    struct Best {
        dist2: i64,
        from_item: ItemId,
        from: IntPoint,
        to_item: ItemId,
        to: IntPoint,
    }
    let key = |b: &Best| {
        (
            b.dist2,
            b.from_item.raw(),
            b.to_item.raw(),
            b.from.x,
            b.from.y,
            b.to.x,
            b.to.y,
        )
    };

    let mut visited = vec![false; groups.len()];
    let mut best: Vec<Option<Best>> = vec![None; groups.len()];
    let mut out = Vec::with_capacity(groups.len() - 1);
    visited[0] = true;
    let mut just_added = 0usize;

    for _ in 1..groups.len() {
        for g in 0..groups.len() {
            if visited[g] {
                continue;
            }
            for &(from_item, from) in &groups[just_added] {
                for &(to_item, to) in &groups[g] {
                    let dx = (from.x as i64) - (to.x as i64);
                    let dy = (from.y as i64) - (to.y as i64);
                    let cand = Best {
                        dist2: dx * dx + dy * dy,
                        from_item,
                        from,
                        to_item,
                        to,
                    };
                    match &best[g] {
                        Some(prev) if key(prev) <= key(&cand) => {}
                        _ => best[g] = Some(cand),
                    }
                }
            }
        }
        let next = (0..groups.len())
            .filter(|g| !visited[*g] && best[*g].is_some())
            .min_by_key(|g| {
                let b = best[*g].as_ref().unwrap();
                (key(b), *g)
            });
        let Some(next) = next else { break };
        let b = best[next].take().unwrap();
        out.push(AirLine {
            net,
            from_item: b.from_item,
            from: b.from,
            to_item: b.to_item,
            to: b.to,
        });
        visited[next] = true;
        just_added = next;
    }
    out
}

fn find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

fn union(parent: &mut [usize], a: usize, b: usize) {
    let (ra, rb) = (find(parent, a), find(parent, b));
    if ra != rb {
        // Union by index keeps the representative a deterministic function of
        // the member set, which the group bookkeeping above relies on.
        if ra < rb {
            parent[rb] = ra;
        } else {
            parent[ra] = rb;
        }
    }
}

/// The connection points of `item`, for callers that want the same anchor set
/// the contact graph is built from.
pub fn connection_points(item: &Item) -> Vec<Point> {
    match &item.kind {
        ItemKind::Trace(t) => vec![t.first_corner(), t.last_corner()],
        ItemKind::Pin(p) => vec![Point::Int(p.center)],
        ItemKind::Via(v) => vec![Point::Int(v.center)],
        _ => Vec::new(),
    }
}

/// Re-exported for callers that want to talk about connection points without
/// naming the [`anchors`](crate::anchors) module.
pub use crate::anchors::AnchorKind as ConnectionPointKind;
