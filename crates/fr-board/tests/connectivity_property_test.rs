//! Differential test for the incremental connectivity index: **its components
//! always equal a brute-force flood fill over the same contact predicate**.
//!
//! The oracle is an O(n²) adjacency matrix plus a breadth-first flood fill, with
//! the contact rules of `drc-connectivity.md` §4.3 re-derived independently of
//! the implementation — exact corner/centre equality, layer sharing, and
//! point-in-box containment for pours (the fixture uses box pours so the oracle
//! never has to borrow the crate's area decomposition).
//!
//! Three properties are pinned:
//!
//! 1. **Partition equality** — same component count *and* same grouping, after
//!    random inserts, removals, splits, combines and via drops.
//! 2. **Dirty-flag soundness** — a stale cache is the failure mode that matters,
//!    so every check runs against the *incremental* index without ever forcing
//!    a full rebuild.
//! 3. **Rollback restores connectivity** — a transaction rolled back leaves the
//!    partition equal to a brute-force fill of the restored board.

use std::collections::VecDeque;

use fr_board::board::{Board, LayerStructure};
use fr_board::clearance::ClearanceTable;
use fr_board::item::{
    AreaShape, ConductionArea, FixedState, Item, ItemId, ItemKind, LayerRange, PlacedArea, Pin, Via,
};
use fr_board::shape::{Padstack, TileShape};
use fr_board::store::ItemStore;
use fr_board::NetNos;
use fr_geometry::planar::{IntBox, IntPoint, Point, Polyline};

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

    /// A coarse grid, so that endpoints coincide exactly often enough for the
    /// contact predicate to fire.
    fn coord(&mut self) -> i32 {
        (self.below(7) as i32 - 3) * 500
    }
}

const LAYER_COUNT: usize = 3;
const NET_COUNT: u32 = 3;

fn new_board() -> Board {
    let mut board = Board::new(
        LayerStructure::signal_layers(LAYER_COUNT),
        ClearanceTable::uniform(3, LAYER_COUNT, 40),
    );
    board.padstacks_mut().push(Padstack::uniform(
        0,
        LAYER_COUNT as i32 - 1,
        TileShape::Box(IntBox::new(-60, -60, 60, 60)),
    ));
    board
}

// ---------------------------------------------------------------- the oracle

/// The connection points of an item, re-derived from §4.3 without touching the
/// crate's own helper.
fn oracle_points(item: &Item) -> Vec<IntPoint> {
    match &item.kind {
        ItemKind::Trace(trace) => [trace.first_corner(), trace.last_corner()]
            .iter()
            .filter_map(|corner| match corner {
                Point::Int(p) => Some(*p),
                Point::Rational(_) => None,
            })
            .collect(),
        ItemKind::Pin(pin) => vec![pin.center],
        ItemKind::Via(via) => vec![via.center],
        _ => Vec::new(),
    }
}

/// The pour's box, for the containment half of the predicate. The fixture only
/// ever builds box pours, so this is exact.
fn oracle_pour_box(item: &Item) -> Option<IntBox> {
    match &item.kind {
        ItemKind::ConductionArea(area) => match &area.area.relative_area {
            AreaShape::Box(b) => Some(*b),
            _ => None,
        },
        _ => None,
    }
}

fn oracle_shares_layer(a: &Item, b: &Item) -> bool {
    let (a, b) = (a.layer_range(), b.layer_range());
    a.first.max(b.first) <= a.last.min(b.last)
}

fn oracle_connected(a: &Item, b: &Item) -> bool {
    if !oracle_shares_layer(a, b) {
        return false;
    }
    let (points_a, points_b) = (oracle_points(a), oracle_points(b));
    if points_a.iter().any(|pa| points_b.contains(pa)) {
        return true;
    }
    if let Some(pour) = oracle_pour_box(a) {
        if points_b.iter().any(|p| p.is_contained_in(&pour)) {
            return true;
        }
    }
    if let Some(pour) = oracle_pour_box(b) {
        if points_a.iter().any(|p| p.is_contained_in(&pour)) {
            return true;
        }
    }
    false
}

/// Brute-force flood fill: the connectable items of `net_no`, grouped, with
/// groups ordered by their smallest member and each group ascending — the same
/// canonical form `Connectivity::components` produces.
fn oracle_components(store: &ItemStore, net_no: i32) -> Vec<Vec<ItemId>> {
    let members: Vec<ItemId> = store
        .net_items(net_no)
        .iter()
        .copied()
        .filter(|id| store.get(*id).is_some_and(|item| item.is_connectable()))
        .collect();
    let count = members.len();

    let mut adjacency = vec![Vec::new(); count];
    for i in 0..count {
        for j in (i + 1)..count {
            let (Some(a), Some(b)) = (store.get(members[i]), store.get(members[j])) else {
                continue;
            };
            if oracle_connected(a, b) {
                adjacency[i].push(j);
                adjacency[j].push(i);
            }
        }
    }

    let mut label = vec![usize::MAX; count];
    let mut groups: Vec<Vec<ItemId>> = Vec::new();
    for start in 0..count {
        if label[start] != usize::MAX {
            continue;
        }
        let group_index = groups.len();
        let mut group = Vec::new();
        let mut queue = VecDeque::new();
        label[start] = group_index;
        queue.push_back(start);
        while let Some(node) = queue.pop_front() {
            group.push(members[node]);
            for next in &adjacency[node] {
                if label[*next] == usize::MAX {
                    label[*next] = group_index;
                    queue.push_back(*next);
                }
            }
        }
        group.sort_unstable();
        groups.push(group);
    }
    groups
}

fn assert_partitions_match(board: &mut Board, context: &str) {
    for net in 1..=NET_COUNT as i32 {
        let expected = oracle_components(board.store(), net);
        let actual = {
            let mut connectivity = std::mem::take(board.connectivity_mut());
            let actual = connectivity.components(net, board.store());
            *board.connectivity_mut() = connectivity;
            actual
        };
        assert_eq!(
            actual, expected,
            "net {net} partition disagrees with the flood fill ({context})"
        );
        assert_eq!(
            board.net_component_count(net),
            expected.len(),
            "net {net} component count disagrees ({context})"
        );
    }
}

// ------------------------------------------------------------- the mutations

fn live_ids(board: &Board) -> Vec<ItemId> {
    board.store().ids().collect()
}

fn live_traces(board: &Board) -> Vec<ItemId> {
    board
        .store()
        .iter()
        .filter(|(_, item)| matches!(item.kind, ItemKind::Trace(_)))
        .map(|(id, _)| id)
        .collect()
}

fn mutate(board: &mut Board, rng: &mut Rng) -> &'static str {
    let net = 1 + rng.below(NET_COUNT) as i32;
    match rng.below(9) {
        0..=2 => {
            let corners = [
                IntPoint::new(rng.coord(), rng.coord()),
                IntPoint::new(rng.coord(), rng.coord()),
            ];
            let polyline = Polyline::from_int_points(&corners);
            if polyline.corner_count() >= 2 {
                board.insert_trace_without_cleaning(
                    polyline,
                    rng.below(LAYER_COUNT as u32) as i32,
                    50,
                    NetNos::of(net),
                    1,
                    FixedState::Unfixed,
                );
            }
            "insert trace"
        }
        3 => {
            board.insert_via(
                Via {
                    center: IntPoint::new(rng.coord(), rng.coord()),
                    padstack_no: 0,
                    layers: LayerRange::new(0, LAYER_COUNT as i32 - 1),
                    attach_allowed: true,
                    is_escape_via: false,
                    escape_via_smd_layer: -1,
                },
                NetNos::of(net),
                1,
                FixedState::Unfixed,
            );
            "insert via"
        }
        4 => {
            board.insert_pin(
                Pin {
                    center: IntPoint::new(rng.coord(), rng.coord()),
                    padstack_no: 0,
                    pin_no: 0,
                    layers: LayerRange::new(0, LAYER_COUNT as i32 - 1),
                },
                NetNos::of(net),
                1,
            );
            "insert pin"
        }
        5 => {
            let x = rng.coord();
            let y = rng.coord();
            board.insert_item(Item::new(
                NetNos::of(net),
                1,
                FixedState::Unfixed,
                ItemKind::ConductionArea(ConductionArea {
                    area: PlacedArea::new(AreaShape::Box(IntBox::new(x, y, x + 1000, y + 1000))),
                    layer: rng.below(LAYER_COUNT as u32) as i32,
                    is_obstacle: true,
                    is_filled: true,
                }),
            ));
            "insert pour"
        }
        6 => {
            let ids = live_ids(board);
            if !ids.is_empty() {
                board.remove_item(ids[rng.below(ids.len() as u32) as usize]);
            }
            "remove item"
        }
        7 => {
            let traces = live_traces(board);
            if !traces.is_empty() {
                let id = traces[rng.below(traces.len() as u32) as usize];
                let point = Point::Int(IntPoint::new(rng.coord(), rng.coord()));
                board.split_trace_at_point(id, &point);
            }
            "split trace"
        }
        _ => {
            let traces = live_traces(board);
            if !traces.is_empty() {
                board.combine_trace(traces[rng.below(traces.len() as u32) as usize]);
            }
            "combine trace"
        }
    }
}

// ------------------------------------------------------------------ the tests

#[test]
fn components_equal_a_brute_force_flood_fill_after_a_mutation_storm() {
    for seed in 1..=12u64 {
        let mut rng = Rng::new(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut board = new_board();
        for step in 0..50 {
            let label = mutate(&mut board, &mut rng);
            assert_partitions_match(&mut board, &format!("seed {seed}, step {step}, after {label}"));
        }
    }
}

#[test]
fn a_pour_swallows_every_same_net_endpoint_inside_it() {
    // A pour is the one contact rule that is containment rather than equality,
    // and it is the one a union-find can silently get wrong.
    let mut board = new_board();
    board.insert_item(Item::new(
        NetNos::of(1),
        1,
        FixedState::Unfixed,
        ItemKind::ConductionArea(ConductionArea {
            area: PlacedArea::new(AreaShape::Box(IntBox::new(-2000, -2000, 2000, 2000))),
            layer: 0,
            is_obstacle: true,
            is_filled: true,
        }),
    ));
    for i in 0..4i32 {
        board.insert_trace_without_cleaning(
            Polyline::from_int_points(&[
                IntPoint::new(-1000 + i * 200, 0),
                IntPoint::new(-1000 + i * 200, 500),
            ]),
            0,
            50,
            NetNos::of(1),
            1,
            FixedState::Unfixed,
        );
    }
    // One trace far outside the pour, on the same net.
    board.insert_trace_without_cleaning(
        Polyline::from_int_points(&[IntPoint::new(9000, 9000), IntPoint::new(9500, 9000)]),
        0,
        50,
        NetNos::of(1),
        1,
        FixedState::Unfixed,
    );

    assert_eq!(board.net_component_count(1), 2);
    assert_partitions_match(&mut board, "pour fixture");
}

#[test]
fn rollback_restores_the_partition() {
    for seed in 1..=10u64 {
        let mut rng = Rng::new(seed.wrapping_mul(0xA24B_AED4_963E_E407));
        let mut board = new_board();
        for _ in 0..20 {
            mutate(&mut board, &mut rng);
        }
        let before: Vec<Vec<Vec<ItemId>>> = (1..=NET_COUNT as i32)
            .map(|net| oracle_components(board.store(), net))
            .collect();

        let mark = board.begin();
        for _ in 0..15 {
            mutate(&mut board, &mut rng);
        }
        board.rollback_to(mark);

        for (index, net) in (1..=NET_COUNT as i32).enumerate() {
            assert_eq!(
                oracle_components(board.store(), net),
                before[index],
                "seed {seed}: the restored board differs on net {net}"
            );
        }
        assert_partitions_match(&mut board, &format!("seed {seed}, after rollback"));
    }
}

#[test]
fn dirty_flags_are_set_by_every_mutation_that_touches_a_net() {
    let mut rng = Rng::new(0xFEED_FACE_CAFE_B0BA);
    let mut board = new_board();
    for _ in 0..20 {
        mutate(&mut board, &mut rng);
    }
    // Clean every net, then confirm one more mutation dirties exactly the net
    // it touched — the guard against a stale cache being read.
    for net in 1..=NET_COUNT as i32 {
        board.net_component_count(net);
        assert!(!board.connectivity().is_dirty(net));
    }

    board.insert_trace_without_cleaning(
        Polyline::from_int_points(&[IntPoint::new(0, 0), IntPoint::new(500, 0)]),
        0,
        50,
        NetNos::of(2),
        1,
        FixedState::Unfixed,
    );
    assert!(board.connectivity().is_dirty(2));
    assert!(!board.connectivity().is_dirty(1));
    assert!(!board.connectivity().is_dirty(3));
    assert_eq!(board.connectivity().dirty_nets().collect::<Vec<_>>(), vec![2]);
    assert_partitions_match(&mut board, "after a single-net insert");
}
