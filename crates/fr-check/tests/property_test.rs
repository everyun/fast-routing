//! Differential property test: the sweep-based checker against a brute-force
//! `O(n²)` oracle on random small boards.
//!
//! The referee has two halves that can fail independently:
//!
//! * the **verdict** — the obstacle table, the two exemptions, the octagon-norm
//!   enlarge/intersect/dimension chain. That half is pinned by hand-computed
//!   cases in `octagon_norm_test.rs` and `connectivity_test.rs`.
//! * the **candidate generation** — the per-layer sweep line that decides which
//!   pairs are worth testing at all. A bug there silently *drops* violations,
//!   which no self-consistent test can catch.
//!
//! This file pins the second half. The oracle enumerates every ordered pair of
//! tile shapes on every layer and applies the verdict rules re-derived from the
//! spec — it shares no code with `fr_check::clearance`, only the `TileShape`
//! metric, which has its own hand-computed tests. If the sweep ever drops a
//! candidate the multisets diverge.

mod support;

use std::collections::HashMap;

use fr_board::item::{Item, ItemId, ItemKind, LayerRange, ObstacleKind};
use fr_board::NetNos;
use fr_check::view::ItemShape;
use fr_check::{actual_clearance, check_clearance, BoardView, TileShape};
use fr_geometry::planar::{IntBox, IntPoint};
use fr_rules::{ClearanceMatrix, Layer, LayerStructure};
use support::{keepout_item, pin_item, pour_item, trace_item, via_item, Scaffold};

/// xorshift64* — deterministic and dependency-free, as in the fr-board and
/// fr-datastructures differential tests.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: u32) -> u32 {
        (self.next_u64() >> 32) as u32 % bound
    }

    fn coord(&mut self) -> i32 {
        // A coarse grid, so that trace endpoints and pad centres coincide often
        // enough to exercise the contact and tie-pin paths.
        (self.below(9) * 50) as i32
    }
}

const LAYERS: usize = 2;

fn matrix() -> ClearanceMatrix {
    let layers = LayerStructure::new(vec![Layer::new("front", true), Layer::new("back", true)]);
    let mut m = ClearanceMatrix::new(3, layers, &["null", "default", "wide"]);
    m.set_value_symmetric(1, 1, 20);
    m.set_value_symmetric(1, 2, 40);
    m.set_value_symmetric(2, 2, 60);
    // Layer-dependent on the back layer, both directions, so the matrix stays
    // symmetric and `max_value_for_class` bounds every pair it is asked about.
    m.set_value_on_layer(1, 2, 1, 90);
    m.set_value_on_layer(2, 1, 1, 90);
    m
}

fn random_nets(rng: &mut Rng) -> NetNos {
    let a = 1 + rng.below(3) as i32;
    if rng.below(6) == 0 {
        NetNos::from_slice(&[a, 1 + rng.below(3) as i32])
    } else {
        NetNos::of(a)
    }
}

fn random_box(rng: &mut Rng) -> IntBox {
    let x = rng.coord();
    let y = rng.coord();
    let w = 10 + rng.below(60) as i32;
    let h = 10 + rng.below(60) as i32;
    IntBox::new(x, y, x + w, y + h)
}

fn random_shapes(rng: &mut Rng, layers: LayerRange) -> Vec<ItemShape> {
    let count = 1 + rng.below(3) as usize;
    (0..count)
        .map(|_| {
            let layer = if layers.is_single() {
                layers.first
            } else {
                layers.first + rng.below((layers.count()) as u32) as i32
            };
            let b = random_box(rng);
            let shape = if rng.below(4) == 0 {
                TileShape::Octagon(b.to_int_octagon())
            } else {
                TileShape::Box(b)
            };
            ItemShape { shape, layer }
        })
        .collect()
}

fn random_board(rng: &mut Rng, item_count: usize) -> Scaffold {
    let mut board = Scaffold::new(LAYERS);
    for _ in 0..item_count {
        let nets = random_nets(rng);
        let cc = rng.below(3) as i32;
        let kind = rng.below(10);
        let (mut item, layers) = match kind {
            0..=3 => {
                let layer = rng.below(LAYERS as u32) as i32;
                let mut from = IntPoint::new(rng.coord(), rng.coord());
                let to = IntPoint::new(rng.coord(), rng.coord());
                if from == to {
                    from = IntPoint::new(from.x + 50, from.y);
                }
                (
                    trace_item(&[], layer, &[from, to], 10),
                    LayerRange::single(layer),
                )
            }
            4..=5 => {
                let layers = if rng.below(2) == 0 {
                    LayerRange::single(rng.below(LAYERS as u32) as i32)
                } else {
                    LayerRange::new(0, LAYERS as i32 - 1)
                };
                (
                    pin_item(&[], IntPoint::new(rng.coord(), rng.coord()), layers),
                    layers,
                )
            }
            6 => {
                let layers = LayerRange::new(0, LAYERS as i32 - 1);
                (
                    via_item(&[], IntPoint::new(rng.coord(), rng.coord()), layers),
                    layers,
                )
            }
            7..=8 => {
                let layer = rng.below(LAYERS as u32) as i32;
                let obstacle_kind = match rng.below(3) {
                    0 => ObstacleKind::Keepout,
                    1 => ObstacleKind::ViaKeepout,
                    _ => ObstacleKind::ComponentKeepout,
                };
                (
                    keepout_item(&[], layer, random_box(rng), obstacle_kind),
                    LayerRange::single(layer),
                )
            }
            _ => {
                let layer = rng.below(LAYERS as u32) as i32;
                (
                    pour_item(&[], layer, random_box(rng), rng.below(2) == 0),
                    LayerRange::single(layer),
                )
            }
        };
        item.nets = nets;
        item.clearance_class = cc;
        item.component_no = rng.below(3) as i32;
        let shapes = random_shapes(rng, layers);
        board.insert(item, shapes);
    }
    board
}

// ---------------------------------------------------------------------------
// the oracle
// ---------------------------------------------------------------------------

/// `Item.sharesLayer` — re-derived here so the oracle shares nothing with the
/// implementation but the geometry kernel.
fn shares_layer(a: &Item, b: &Item) -> bool {
    let (ra, rb) = (a.layer_range(), b.layer_range());
    ra.first.max(rb.first) <= ra.last.min(rb.last)
}

fn has_anchor_at(item: &Item, point: &fr_geometry::planar::Point) -> bool {
    match &item.kind {
        ItemKind::Trace(t) => t.first_corner() == *point || t.last_corner() == *point,
        ItemKind::Pin(p) => fr_geometry::planar::Point::Int(p.center) == *point,
        ItemKind::Via(v) => fr_geometry::planar::Point::Int(v.center) == *point,
        _ => false,
    }
}

/// `drc-connectivity.md` §2.4, written the dumbest possible way: scan every item
/// on the board for one sitting at the endpoint.
fn tie_pin_exempt(items: &[&Item], this: &Item, other: &Item) -> bool {
    let ItemKind::Trace(t) = &this.kind else {
        return false;
    };
    if !matches!(other.kind, ItemKind::Trace(_)) {
        return false;
    }
    for point in [t.first_corner(), t.last_corner()] {
        let contacts: Vec<&&Item> = items
            .iter()
            .filter(|c| c.id != this.id && shares_layer(c, this) && has_anchor_at(c, &point))
            .collect();
        if !contacts.iter().any(|c| c.id == other.id) {
            continue;
        }
        return contacts.iter().any(|c| {
            matches!(c.kind, ItemKind::Pin(_)) && c.shares_net(this) && c.shares_net(other)
        });
    }
    false
}

fn escape_via_exempt(this: &Item, other: &Item, layer: i32) -> bool {
    match &this.kind {
        ItemKind::Via(v) => {
            v.is_escape_via
                && v.escape_via_smd_layer >= 0
                && v.escape_via_smd_layer == layer
                && other.shares_net(this)
        }
        _ => false,
    }
}

/// One violation as the oracle reports it: `(a, b, layer, expected)`.
type Record = (ItemId, ItemId, i32, i32);

/// Every violating [`Record`], found without any spatial index.
fn brute_force<V: BoardView + ?Sized>(view: &V) -> Vec<Record> {
    let ids = view.item_ids();
    let items: Vec<&Item> = ids.iter().map(|id| view.item(*id)).collect();
    let mut found: HashMap<(u32, u32, i32), Record> = HashMap::new();

    let mut ordered: Vec<ItemId> = ids.to_vec();
    ordered.sort_unstable();

    for (ai, &id_a) in ordered.iter().enumerate() {
        for &id_b in &ordered[ai + 1..] {
            let a = view.item(id_a);
            let b = view.item(id_b);
            // Ascending item order, exactly as upstream's scan: the lower id is
            // `self` first, so it is the one recorded when both directions fire.
            // Each direction is explored over *every* shape pair before the
            // other one starts, because the priority is per (pair, layer) and a
            // pair can violate on several layers.
            for (this, other) in [(a, b), (b, a)] {
                if !other.is_obstacle(this) {
                    continue;
                }
                if tie_pin_exempt(&items, this, other) {
                    continue;
                }
                for sa in view.shapes(this.id) {
                    for sb in view.shapes(other.id) {
                        if sa.layer != sb.layer {
                            continue;
                        }
                        if escape_via_exempt(this, other, sa.layer) {
                            continue;
                        }
                        let key = (id_a.raw(), id_b.raw(), sa.layer);
                        if found.contains_key(&key) {
                            continue;
                        }
                        let cl =
                            view.clearance(other.clearance_class, this.clearance_class, sa.layer);
                        let c1 = (0.5 * cl as f64).round() as i32;
                        let c2 = cl - c1;
                        let e1 = if c1 > 0 {
                            sa.shape.enlarge(c1 as f64)
                        } else {
                            sa.shape.clone()
                        };
                        let e2 = if c2 > 0 {
                            sb.shape.enlarge(c2 as f64)
                        } else {
                            sb.shape.clone()
                        };
                        if e1.intersection(&e2).dimension() == 2 {
                            found.insert(key, (this.id, other.id, sa.layer, cl));
                        }
                    }
                }
            }
        }
    }

    let mut out: Vec<_> = found.into_values().collect();
    out.sort_unstable_by_key(|(a, b, layer, _)| {
        (a.raw().min(b.raw()), a.raw().max(b.raw()), *layer)
    });
    out
}

// ---------------------------------------------------------------------------
// the properties
// ---------------------------------------------------------------------------

#[test]
fn the_sweep_finds_exactly_what_brute_force_finds() {
    let rules = matrix();
    let mut checked_boards = 0usize;
    let mut checked_violations = 0usize;
    for seed in 1..=400u64 {
        let mut rng = Rng::new(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let n = 4 + rng.below(18) as usize;
        let board = random_board(&mut rng, n);
        let view = board.view(&rules);

        let expected = brute_force(&view);
        let actual: Vec<_> = check_clearance(&view)
            .into_iter()
            .map(|v| (v.a, v.b, v.layer, v.expected))
            .collect();
        assert_eq!(
            actual, expected,
            "seed {seed}: sweep and brute force disagree on a {n}-item board"
        );
        checked_boards += 1;
        checked_violations += actual.len();
    }
    assert_eq!(checked_boards, 400);
    // Guard against the test silently degenerating into "both find nothing".
    assert!(
        checked_violations > 400,
        "the generator should produce plenty of violations, got {checked_violations}"
    );
}

/// The same, on boards big enough that the sweep's active-list management and
/// its layer partitioning actually matter.
#[test]
fn the_sweep_agrees_with_brute_force_on_denser_boards() {
    let rules = matrix();
    for seed in 1..=25u64 {
        let mut rng = Rng::new(0xDEAD_0000 ^ seed);
        let board = random_board(&mut rng, 120);
        let view = board.view(&rules);
        let expected = brute_force(&view);
        let actual: Vec<_> = check_clearance(&view)
            .into_iter()
            .map(|v| (v.a, v.b, v.layer, v.expected))
            .collect();
        assert_eq!(actual, expected, "seed {seed}");
    }
}

/// **T2.** The parallel violation scan is not "equivalent" to the sequential
/// one, it is *equal* to it — same records, same order, same `expected`/`actual`
/// on every deduplicated pair.
///
/// The chunk floor is forced down to 8 entries so that most chunks start in the
/// middle of a layer run and have to rebuild the sweep's active set from a
/// prefix scan (`seed_active`). At the shipped floor these boards would be one
/// chunk per layer and the interesting path would never run.
#[test]
fn the_parallel_scan_returns_exactly_the_sequential_result() {
    let rules = matrix();
    let mut chunked_boards = 0;
    for seed in 1..=25u64 {
        let mut rng = Rng::new(0x5EED_0000 ^ seed);
        let board = random_board(&mut rng, 120);
        let view = board.view(&rules);
        let anchors = fr_check::AnchorIndex::build(&view);
        let expected = fr_check::check_clearance_with(&view, &anchors);
        for threads in [2usize, 3, 8] {
            let actual =
                fr_check::clearance::check_clearance_parallel_chunked(&view, &anchors, threads, 8);
            assert_eq!(actual, expected, "seed {seed}, {threads} threads");
        }
        if !expected.is_empty() {
            chunked_boards += 1;
        }
    }
    assert!(
        chunked_boards >= 10,
        "the corpus has to actually produce violations, or this proves nothing"
    );
}

/// ... and at the shipped chunk floor, which is the configuration the product
/// runs.
#[test]
fn the_shipped_parallel_scan_agrees_too() {
    let rules = matrix();
    for seed in 1..=25u64 {
        let mut rng = Rng::new(0xB0A7_0000 ^ seed);
        let board = random_board(&mut rng, 120);
        let view = board.view(&rules);
        let anchors = fr_check::AnchorIndex::build(&view);
        assert_eq!(
            fr_check::check_clearance_parallel(&view, &anchors, 4),
            fr_check::check_clearance_with(&view, &anchors),
            "seed {seed}"
        );
        // One worker is the sequential path by construction.
        assert_eq!(
            fr_check::check_clearance_parallel(&view, &anchors, 1),
            fr_check::check_clearance_with(&view, &anchors),
            "seed {seed}, one thread"
        );
    }
}

/// The referee is a function of the board and of nothing else: no hasher order,
/// no allocation address, no iteration accident.
#[test]
fn the_result_is_bit_identical_across_runs() {
    let rules = matrix();
    for seed in 1..=60u64 {
        let mut rng = Rng::new(0xC0FF_EE00 ^ seed);
        let board = random_board(&mut rng, 40);
        let view = board.view(&rules);
        let first = check_clearance(&view);
        let second = check_clearance(&view);
        assert_eq!(first, second, "seed {seed}");
        let conn_a = fr_check::board_connectivity(&view);
        let conn_b = fr_check::board_connectivity(&view);
        assert_eq!(conn_a, conn_b, "seed {seed}");
    }
}

/// `actual` is a lower bound on the gap that is capped by the requirement, and
/// `actual_clearance` — which minimises over *every* shape pair rather than
/// reporting an arbitrary one — never exceeds the per-violation number.
#[test]
fn the_reported_gap_is_a_bounded_lower_bound() {
    let rules = matrix();
    for seed in 1..=120u64 {
        let mut rng = Rng::new(0x5EED_0000 ^ seed);
        let board = random_board(&mut rng, 24);
        let view = board.view(&rules);
        for v in check_clearance(&view) {
            assert!(
                v.actual >= 0.0 && v.actual <= v.expected as f64,
                "seed {seed}: {v:?}"
            );
            assert_eq!(v.actual_dim2, v.actual == 0.0 && v.actual_dim2);
            let tightest = actual_clearance(&view, v.a, v.b).expect("the pair shares a layer");
            assert!(
                tightest <= v.actual + 1e-9,
                "seed {seed}: the minimum over all shape pairs ({tightest}) must not exceed the reported {} for {v:?}",
                v.actual
            );
        }
    }
}

/// Every contact the indexed graph reports, and no others, found by comparing
/// every pair of items directly.
///
/// The [`ContactGraph`](fr_check::ContactGraph) sorts anchors and binary-searches
/// runs of coincident points; this oracle just asks, for all `n²` pairs, "do
/// these two share a net, a layer, and a point". It also re-derives the exact
/// point-equality rule from the spec rather than reusing the anchor index.
#[test]
fn the_contact_graph_matches_a_naive_all_pairs_scan() {
    let rules = matrix();
    let mut total_contacts = 0usize;
    for seed in 1..=200u64 {
        let mut rng = Rng::new(0xC0_1AC7 ^ seed);
        let board = random_board(&mut rng, 22);
        let view = board.view(&rules);
        let graph = fr_check::ContactGraph::build(&view);

        for &id_a in view.item_ids() {
            let a = view.item(id_a);
            let mut expected: Vec<ItemId> = Vec::new();
            for &id_b in view.item_ids() {
                if id_a == id_b {
                    continue;
                }
                let b = view.item(id_b);
                if !a.is_connectable() || !b.is_connectable() {
                    continue;
                }
                if !a.shares_net(b) || !shares_layer(a, b) {
                    continue;
                }
                if naive_contact(&view, a, b) {
                    expected.push(id_b);
                }
            }
            expected.sort_unstable();
            // `contacts` iterates in the view's item order, not by id, so the
            // comparison is on the set.
            let mut got: Vec<ItemId> = graph.contacts(id_a).collect();
            got.sort_unstable();
            assert_eq!(got, expected, "seed {seed}, item {id_a}");
            total_contacts += got.len();
        }
    }
    assert!(
        total_contacts > 200,
        "the generator should produce contacts, got {total_contacts}"
    );
}

/// Exact-point contact, straight out of `drc-connectivity.md` §4.3.
fn naive_contact<V: BoardView + ?Sized>(view: &V, a: &Item, b: &Item) -> bool {
    let anchors = |item: &Item| -> Vec<fr_geometry::planar::Point> {
        match &item.kind {
            ItemKind::Trace(t) => vec![t.first_corner(), t.last_corner()],
            ItemKind::Pin(p) => vec![fr_geometry::planar::Point::Int(p.center)],
            ItemKind::Via(v) => vec![fr_geometry::planar::Point::Int(v.center)],
            _ => Vec::new(),
        }
    };
    let (pa, pb) = (anchors(a), anchors(b));
    if pa.iter().any(|x| pb.iter().any(|y| x == y)) {
        return true;
    }
    let contains = |area: &Item, points: &[fr_geometry::planar::Point]| {
        matches!(area.kind, ItemKind::ConductionArea(_))
            && view
                .shapes(area.id)
                .iter()
                .any(|s| points.iter().any(|p| s.shape.contains_point(p)))
    };
    contains(a, &pb) || contains(b, &pa)
}

/// Every net's incompletes are `node_components - 1`, never negative, and the
/// airline list is exactly that long. This is the G1.2b internal-consistency
/// property; there is no Java oracle for `N`, so it is checked against its own
/// definition on random boards.
#[test]
fn incompletes_always_equal_the_node_component_count_minus_one() {
    let rules = matrix();
    for seed in 1..=200u64 {
        let mut rng = Rng::new(0xA11E_0000 ^ seed);
        let board = random_board(&mut rng, 20);
        let view = board.view(&rules);
        let all = fr_check::board_connectivity(&view);
        let mut total = 0;
        for net in &all.nets {
            assert_eq!(net.incompletes, net.node_components.saturating_sub(1));
            assert_eq!(net.airlines.len(), net.incompletes);
            assert!(net.node_components <= net.components);
            assert!(net.airlines.iter().all(|a| a.net == net.net));
            total += net.incompletes;
        }
        assert_eq!(all.incomplete_count, total);

        // The whole-board path buckets nets in one pass and reuses one
        // union-find array across them; the single-net path rescans and
        // allocates its own. They must not be able to drift apart.
        for net in &all.nets {
            assert_eq!(*net, fr_check::connectivity(&view, net.net), "seed {seed}");
        }
    }
}
