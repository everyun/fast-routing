//! Differential test for the search-tree manager: **after any sequence of
//! board mutations, every tree answers exactly what a brute-force scan over the
//! live items answers.**
//!
//! This is the load-bearing invariant of `search-tree-obstacles.md` §5: the
//! trees are a derived index, maintained incrementally on insert, remove,
//! trace split, trace combine, normalize and transaction rollback. A single
//! missed update is invisible until the router silently routes through copper.
//!
//! The oracle is deliberately the dumbest thing that can work: recompute every
//! item's tile shapes from scratch with the same public
//! [`calculate_tree_shapes`] the manager uses, then filter and intersect with
//! no index at all. A disagreement therefore always indicts the *bookkeeping*
//! (which leaves exist, which slot they hang off, whether a mutation forgot to
//! reindex) and never the geometry.
//!
//! Three properties are pinned:
//!
//! 1. **Query equality** — for both the default tree and a compensated tree,
//!    over query boxes, per layer and with net filters.
//! 2. **Internal consistency** — `Board::assert_consistent` after every step:
//!    one leaf per occupied shape index, no leaf under a recycled slot, the
//!    store's own indices intact.
//! 3. **Rollback restores the index** — a transaction full of splits, combines
//!    and removals, rolled back, leaves both the items *and* the trees
//!    byte-identical to a snapshot taken before it opened.

use fr_board::board::{Board, LayerStructure};
use fr_board::clearance::ClearanceTable;
use fr_board::item::{
    AreaShape, FixedState, Item, ItemId, ItemKind, LayerRange, ObstacleArea, ObstacleKind, Pin,
    PlacedArea, Via,
};
use fr_board::search_tree::{calculate_tree_shapes, ItemTileShape, SearchTree, TreeEntry};
use fr_board::shape::{Padstack, TileShape};
use fr_board::NetNos;
use fr_datastructures::arena_shape_tree::QueryStack;
use fr_geometry::planar::{IntBox, IntPoint, Point, Polyline};

/// xorshift64* — deterministic and dependency-free, as in the other fr-board
/// and fr-datastructures differential tests.
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

    /// A coordinate on a coarse grid, so that junctions, overlaps and exact
    /// corner coincidences actually happen instead of being measure-zero.
    fn coord(&mut self) -> i32 {
        (self.below(9) as i32 - 4) * 500
    }
}

const LAYER_COUNT: usize = 3;
const NET_COUNT: u32 = 4;
const CLEARANCE_CLASSES: usize = 4;

fn new_board() -> Board {
    let mut board = Board::new(
        LayerStructure::signal_layers(LAYER_COUNT),
        ClearanceTable::uniform(CLEARANCE_CLASSES, LAYER_COUNT, 40),
    );
    // 0: a square through-hole pad on every layer, drilled.
    let mut through = Padstack::uniform(
        0,
        LAYER_COUNT as i32 - 1,
        TileShape::Box(IntBox::new(-60, -60, 60, 60)),
    );
    through.drill_radius = 30;
    board.padstacks_mut().push(through);
    // 1: an SMD pad on layer 0 only.
    board.padstacks_mut().push(Padstack::uniform(
        0,
        0,
        TileShape::Box(IntBox::new(-80, -40, 80, 40)),
    ));
    // 2: a round via padstack with an unused middle layer, so the synthetic
    //    drill-hole obstacle is exercised.
    let mut sparse = Padstack::round(0, LAYER_COUNT as i32 - 1, 50);
    if sparse.shapes.len() > 1 {
        sparse.shapes[1] = None;
    }
    board.padstacks_mut().push(sparse);
    board
}

// ---------------------------------------------------------------- the oracle

/// Every (item, shape index) whose tile shape really overlaps `query`,
/// recomputed from the items with no index involved.
///
/// Mirrors `SearchTree::overlapping_tree_entries` filter for filter: layer,
/// then the ignore-net rule (skip an item that is an obstacle to none of the
/// listed nets), then the exact intersection.
fn brute_force_entries(
    board: &Board,
    tree_class: i32,
    query: &TileShape,
    layer: Option<i32>,
    ignore_nets: &[i32],
) -> Vec<TreeEntry> {
    let ctx = board.shape_ctx();
    let mut shapes: Vec<ItemTileShape> = Vec::new();
    let mut out = Vec::new();
    for (id, item) in board.store().iter() {
        if !ignore_nets.is_empty() && !ignore_nets.iter().any(|net| !item.contains_net(*net)) {
            continue;
        }
        calculate_tree_shapes(item, tree_class, ctx, &mut shapes);
        for (index, tile) in shapes.iter().enumerate() {
            if let Some(l) = layer {
                if tile.layer != l {
                    continue;
                }
            }
            let Some(shape) = tile.shape.as_ref() else {
                continue;
            };
            if shape.intersects(query) {
                out.push(TreeEntry {
                    item: id,
                    shape_index: index as u16,
                });
            }
        }
    }
    sort_entries(&mut out);
    out
}

fn sort_entries(entries: &mut [TreeEntry]) {
    entries.sort_by(|a, b| {
        b.item
            .cmp(&a.item)
            .then_with(|| a.shape_index.cmp(&b.shape_index))
    });
}

fn tree_entries(
    board: &Board,
    tree: &SearchTree,
    query: &TileShape,
    layer: Option<i32>,
    ignore_nets: &[i32],
) -> Vec<TreeEntry> {
    let mut stack = QueryStack::new();
    let mut out = Vec::new();
    tree.overlapping_tree_entries(
        board.store(),
        query,
        layer,
        ignore_nets,
        &mut stack,
        &mut out,
    );
    out
}

/// The full set of query probes the invariant is checked with.
fn probes() -> Vec<(TileShape, Option<i32>, Vec<i32>)> {
    let mut probes = Vec::new();
    // The whole board, per layer and across layers, unfiltered.
    let everything = TileShape::Box(IntBox::new(-10_000, -10_000, 10_000, 10_000));
    probes.push((everything.clone(), None, Vec::new()));
    for layer in 0..LAYER_COUNT as i32 {
        probes.push((everything.clone(), Some(layer), Vec::new()));
    }
    // Net filters, which is where the "obstacle to none of the listed nets"
    // rule is easy to get backwards.
    probes.push((everything.clone(), Some(0), vec![1]));
    probes.push((everything, Some(0), vec![1, 2]));
    // Small windows, so the tree's bounds pruning is actually exercised rather
    // than every leaf being visited.
    for (x, y) in [(0, 0), (500, -1000), (-1500, 1500), (2000, 2000)] {
        probes.push((
            TileShape::Box(IntBox::new(x - 300, y - 300, x + 300, y + 300)),
            Some(0),
            Vec::new(),
        ));
    }
    // A point probe — the shape `pick_items` and the contact predicate use.
    probes.push((
        TileShape::from_point(&Point::Int(IntPoint::new(0, 0))),
        None,
        Vec::new(),
    ));
    probes
}

fn assert_trees_match_brute_force(board: &Board, compensated_class: Option<i32>, context: &str) {
    for (query, layer, ignore_nets) in probes() {
        let expected = brute_force_entries(
            board,
            board.default_tree().compensated_class(),
            &query,
            layer,
            &ignore_nets,
        );
        let actual = tree_entries(board, board.default_tree(), &query, layer, &ignore_nets);
        assert_eq!(
            actual, expected,
            "default tree disagrees with the brute-force scan ({context}), \
             query {query:?} layer {layer:?} ignore {ignore_nets:?}"
        );

        if let Some(class) = compensated_class {
            let tree = board
                .trees()
                .compensated_tree(class)
                .expect("the compensated tree was created");
            let expected = brute_force_entries(board, class, &query, layer, &ignore_nets);
            let actual = tree_entries(board, tree, &query, layer, &ignore_nets);
            assert_eq!(
                actual, expected,
                "compensated tree (class {class}) disagrees with the brute-force scan ({context})"
            );
        }
    }
}

// ------------------------------------------------------------- the mutations

fn random_trace(board: &mut Board, rng: &mut Rng) -> Option<ItemId> {
    let corner_count = 2 + rng.below(3) as usize;
    let mut corners = Vec::with_capacity(corner_count);
    for _ in 0..corner_count {
        corners.push(IntPoint::new(rng.coord(), rng.coord()));
    }
    let polyline = Polyline::from_int_points(&corners);
    if polyline.corner_count() < 2 {
        return None;
    }
    board.insert_trace_without_cleaning(
        polyline,
        rng.below(LAYER_COUNT as u32) as i32,
        20 + 20 * rng.below(3) as i32,
        NetNos::of(1 + rng.below(NET_COUNT) as i32),
        1 + rng.below(CLEARANCE_CLASSES as u32 - 1) as i32,
        FixedState::Unfixed,
    )
}

fn random_via(board: &mut Board, rng: &mut Rng) -> ItemId {
    let via = Via {
        center: IntPoint::new(rng.coord(), rng.coord()),
        padstack_no: if rng.below(2) == 0 { 0 } else { 2 },
        layers: LayerRange::new(0, LAYER_COUNT as i32 - 1),
        attach_allowed: true,
        is_escape_via: false,
        escape_via_smd_layer: -1,
    };
    let nets = NetNos::of(1 + rng.below(NET_COUNT) as i32);
    let clearance = 1 + rng.below(CLEARANCE_CLASSES as u32 - 1) as i32;
    if rng.below(2) == 0 {
        board.insert_via(via, nets, clearance, FixedState::Unfixed)
    } else {
        board.insert_escape_via(via, nets, clearance, FixedState::Unfixed)
    }
}

fn random_pin(board: &mut Board, rng: &mut Rng) -> ItemId {
    board.insert_pin(
        Pin {
            center: IntPoint::new(rng.coord(), rng.coord()),
            padstack_no: if rng.below(2) == 0 { 0 } else { 1 },
            pin_no: 0,
            layers: if rng.below(2) == 0 {
                LayerRange::new(0, LAYER_COUNT as i32 - 1)
            } else {
                LayerRange::single(0)
            },
        },
        NetNos::of(1 + rng.below(NET_COUNT) as i32),
        1 + rng.below(CLEARANCE_CLASSES as u32 - 1) as i32,
    )
}

fn random_keepout(board: &mut Board, rng: &mut Rng) -> ItemId {
    let x = rng.coord();
    let y = rng.coord();
    board.insert_item(Item::new(
        NetNos::EMPTY,
        1 + rng.below(CLEARANCE_CLASSES as u32 - 1) as i32,
        FixedState::Unfixed,
        ItemKind::ObstacleArea(ObstacleArea {
            area: PlacedArea::new(AreaShape::Box(IntBox::new(x, y, x + 400, y + 400))),
            layer: rng.below(LAYER_COUNT as u32) as i32,
            kind: ObstacleKind::Keepout,
        }),
    ))
}

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

/// One random board mutation. Returns a short label for failure messages.
fn mutate(board: &mut Board, rng: &mut Rng) -> &'static str {
    match rng.below(10) {
        0..=2 => {
            random_trace(board, rng);
            "insert trace"
        }
        3 => {
            random_via(board, rng);
            "insert via"
        }
        4 => {
            random_pin(board, rng);
            "insert pin"
        }
        5 => {
            random_keepout(board, rng);
            "insert keepout"
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
        8 => {
            let traces = live_traces(board);
            if !traces.is_empty() {
                board.combine_trace(traces[rng.below(traces.len() as u32) as usize]);
            }
            "combine trace"
        }
        _ => {
            board.normalize_traces(1 + rng.below(NET_COUNT) as i32);
            "normalize net"
        }
    }
}

fn snapshot(board: &Board) -> Vec<(ItemId, Item)> {
    board
        .store()
        .iter()
        .map(|(id, item)| (id, item.clone()))
        .collect()
}

/// A canonical description of what the trees hold, independent of insertion
/// order: every leaf as (item, shape index), sorted.
fn tree_fingerprint(board: &Board) -> Vec<TreeEntry> {
    let everything = TileShape::Box(IntBox::new(-100_000, -100_000, 100_000, 100_000));
    let mut entries = tree_entries(board, board.default_tree(), &everything, None, &[]);
    sort_entries(&mut entries);
    entries
}

// ------------------------------------------------------------------ the tests

#[test]
fn tree_queries_equal_a_brute_force_scan_after_a_mutation_storm() {
    for seed in 1..=12u64 {
        let mut rng = Rng::new(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut board = new_board();
        for step in 0..60 {
            let label = mutate(&mut board, &mut rng);
            board.assert_consistent();
            assert_trees_match_brute_force(
                &board,
                None,
                &format!("seed {seed}, step {step}, after {label}"),
            );
        }
    }
}

#[test]
fn a_compensated_tree_stays_in_step_with_the_default_one() {
    // §5.2: once an autoroute tree exists, every later mutation fans out to it
    // as well — the easiest place for an incremental update to be forgotten.
    for seed in 1..=6u64 {
        let mut rng = Rng::new(seed.wrapping_mul(0xD1B5_4A32_D192_ED03));
        let mut board = new_board();
        for _ in 0..15 {
            mutate(&mut board, &mut rng);
        }
        board.ensure_compensated_tree(2);
        assert_eq!(board.trees().tree_count(), 2);

        for step in 0..40 {
            let label = mutate(&mut board, &mut rng);
            board.assert_consistent();
            assert_trees_match_brute_force(
                &board,
                Some(2),
                &format!("seed {seed}, step {step}, after {label}"),
            );
        }
    }
}

#[test]
fn rollback_restores_the_items_and_the_trees_exactly() {
    for seed in 1..=10u64 {
        let mut rng = Rng::new(seed.wrapping_mul(0xA24B_AED4_963E_E407));
        let mut board = new_board();
        for _ in 0..25 {
            mutate(&mut board, &mut rng);
        }

        let items_before = snapshot(&board);
        let trees_before = tree_fingerprint(&board);

        let mark = board.begin();
        for _ in 0..20 {
            mutate(&mut board, &mut rng);
        }
        board.rollback_to(mark);

        assert_eq!(board.transaction_depth(), 0);
        board.assert_consistent();
        assert_eq!(
            snapshot(&board),
            items_before,
            "seed {seed}: the item store differs from the deep snapshot"
        );
        assert_eq!(
            tree_fingerprint(&board),
            trees_before,
            "seed {seed}: the search tree differs from the deep snapshot"
        );
        assert_trees_match_brute_force(&board, None, &format!("seed {seed}, after rollback"));
    }
}

#[test]
fn nested_rollback_restores_only_the_inner_level() {
    let mut rng = Rng::new(0xC0FF_EE12_3456_789A);
    let mut board = new_board();
    for _ in 0..20 {
        mutate(&mut board, &mut rng);
    }

    let outer = board.begin();
    for _ in 0..8 {
        mutate(&mut board, &mut rng);
    }
    let after_outer_work = snapshot(&board);
    let trees_after_outer_work = tree_fingerprint(&board);

    let inner = board.begin();
    for _ in 0..8 {
        mutate(&mut board, &mut rng);
    }
    board.rollback_to(inner);

    assert_eq!(board.transaction_depth(), 1, "the outer level is still open");
    assert_eq!(snapshot(&board), after_outer_work);
    assert_eq!(tree_fingerprint(&board), trees_after_outer_work);
    board.assert_consistent();

    board.commit(outer);
    assert_eq!(board.transaction_depth(), 0);
    assert_eq!(snapshot(&board), after_outer_work);
    board.assert_consistent();
}

#[test]
fn a_committed_inner_level_is_still_undone_by_the_outer_rollback() {
    let mut rng = Rng::new(0x0BAD_F00D_1234_5678);
    let mut board = new_board();
    for _ in 0..15 {
        mutate(&mut board, &mut rng);
    }
    let before = snapshot(&board);
    let trees_before = tree_fingerprint(&board);

    let outer = board.begin();
    let inner = board.begin();
    for _ in 0..10 {
        mutate(&mut board, &mut rng);
    }
    board.commit(inner);
    for _ in 0..5 {
        mutate(&mut board, &mut rng);
    }
    board.rollback_to(outer);

    assert_eq!(snapshot(&board), before);
    assert_eq!(tree_fingerprint(&board), trees_before);
    board.assert_consistent();
}
