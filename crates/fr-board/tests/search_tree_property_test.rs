//! Differential test for the search-tree manager: **after any sequence of
//! board mutations, every tree answers exactly what a brute-force scan over the
//! live items answers.**
//!
//! This is the load-bearing invariant of `search-tree-obstacles.md` §5: the
//! trees are a derived index, maintained incrementally on insert, remove,
//! trace split, trace combine, normalize, **in-place polyline change** and
//! transaction rollback. A single missed update is invisible until the router
//! silently routes through copper.
//!
//! The in-place change ([`Board::change_trace_polyline`], §6's `change_entries`)
//! is the one mutation that deliberately does *not* rebuild what it can prove
//! is unchanged: it keeps the leaves and tile shapes of every segment outside
//! the edited run. That makes it the mutation with the most ways to be subtly
//! wrong — a kept leaf under a stale shape, a shape index that shifted without
//! its leaf, a hull cache left behind — so it gets a storm of its own on top of
//! its place in the general one.
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
use fr_geometry::planar::{IntBox, IntPoint, Line, Point, Polyline};

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
                    slot: board.store().slot_of(id).expect("live item has a slot"),
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

/// A new line array for `id`'s polyline, built the way the optimizer builds
/// one: from the trace's own lines, with a short run near the middle edited.
///
/// The five shapes cover every branch of the kept-range arithmetic:
/// an edit that keeps the count (both ends reusable and no index shifts), one
/// that shrinks it and one that grows it (the trailing leaves have to move), a
/// reversal (nothing at either end is reusable), and the identity (the call
/// must report "no change" and touch nothing).
fn changed_polyline(board: &Board, id: ItemId, rng: &mut Rng) -> Option<Polyline> {
    let item = board.store().get(id)?;
    let ItemKind::Trace(trace) = &item.kind else {
        return None;
    };
    let lines: &[Line] = &trace.polyline.arr;
    if lines.len() < 3 {
        return None;
    }
    let mut edited = lines.to_vec();
    // Interior lines only: the two end lines carry the trace's endpoints, and
    // `reposition_line` never moves them either.
    let interior = lines.len() - 2;
    let pick = 1 + rng.below(interior as u32) as usize;
    match rng.below(5) {
        // Slide one line sideways — `reposition_line`'s move, and the only one
        // the segment count survives.
        0 | 1 => {
            let distance = f64::from(100 * (1 + rng.below(4) as i32));
            let sign = if rng.below(2) == 0 { 1.0 } else { -1.0 };
            edited[pick] = edited[pick].translate(sign * distance);
        }
        // Drop a corner — `reduce_corners` / `skip_segments_of_length_0`.
        2 => {
            edited.remove(pick);
        }
        // Add one — `smoothen_corners` cutting a corner in two.
        3 => {
            let extra = edited[pick].translate(f64::from(100 * (1 + rng.below(3) as i32)));
            edited.insert(pick, extra);
        }
        // The whole trace the other way round: no equal prefix, no equal suffix.
        _ => {
            edited.reverse();
            for line in edited.iter_mut() {
                *line = line.opposite();
            }
        }
    }
    Some(Polyline::new_from_lines(&edited))
}

fn random_polyline_change(board: &mut Board, rng: &mut Rng) {
    let traces = live_traces(board);
    if traces.is_empty() {
        return;
    }
    let id = traces[rng.below(traces.len() as u32) as usize];
    // One in six calls hands back the polyline the trace already has, which is
    // upstream's "both polylines are equal, no change necessary" early-out.
    let polyline = if rng.below(6) == 0 {
        match board.store().get(id).map(|item| &item.kind) {
            Some(ItemKind::Trace(trace)) => trace.polyline.clone(),
            _ => return,
        }
    } else {
        match changed_polyline(board, id, rng) {
            Some(polyline) => polyline,
            None => return,
        }
    };
    board.change_trace_polyline(id, polyline);
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
    match rng.below(12) {
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
        9 => {
            board.normalize_traces(1 + rng.below(NET_COUNT) as i32);
            "normalize net"
        }
        _ => {
            random_polyline_change(board, rng);
            "change trace polyline"
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

// ---------------------------------------- the in-place change (§6, change_entries)

/// A storm made only of in-place polyline changes, on a board dense enough that
/// the kept ranges are long.
///
/// The general storm mixes changes in with everything else, which is the right
/// integration test but a poor stress test for *this* mutation: most of its
/// traces are three corners long and most steps do something else. Here every
/// step edits a polyline, so the reuse arithmetic — kept prefix, kept suffix,
/// shifted trailing indices — is exercised thousands of times against the
/// brute-force oracle.
#[test]
fn tree_queries_survive_a_storm_of_in_place_polyline_changes() {
    for seed in 1..=8u64 {
        let mut rng = Rng::new(seed.wrapping_mul(0x2545_F491_4F6C_DD1D));
        let mut board = new_board();
        for _ in 0..14 {
            random_trace(&mut board, &mut rng);
            random_via(&mut board, &mut rng);
        }
        board.ensure_compensated_tree(2);

        for step in 0..80 {
            random_polyline_change(&mut board, &mut rng);
            board.assert_consistent();
            assert_trees_match_brute_force(
                &board,
                Some(2),
                &format!("seed {seed}, change storm step {step}"),
            );
        }
    }
}

/// The reuse must be invisible: changing a polyline in place has to leave the
/// board in exactly the state a remove + re-insert of the same geometry would.
///
/// The two boards are stepped in lockstep on the same random edits. The
/// in-place one keeps the trace's id; the reference one burns a new id every
/// step, so the comparison is over the *geometry and the index*, keyed by
/// position rather than by id.
#[test]
fn an_in_place_change_indexes_exactly_what_a_reinsert_would() {
    for seed in 1..=8u64 {
        let mut rng = Rng::new(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut board = new_board();
        for _ in 0..10 {
            random_trace(&mut board, &mut rng);
            random_pin(&mut board, &mut rng);
        }

        let mut reference = new_board();
        for (_, item) in board.store().iter_ascending() {
            reference.insert_item(item.clone());
        }

        for step in 0..40 {
            let traces = live_traces(&board);
            if traces.is_empty() {
                break;
            }
            let id = traces[rng.below(traces.len() as u32) as usize];
            let Some(polyline) = changed_polyline(&board, id, &mut rng) else {
                continue;
            };
            let Some(prep) = trace_parts(&board, id) else {
                continue;
            };
            // `change` has no insertion guard — upstream's does not either, it
            // swaps the geometry and lets `normalize` drop what collapsed
            // (`PolylineTrace.java:1004`). `insert_trace_without_cleaning` *does*
            // refuse a degenerate polyline, so on those the two paths are not
            // supposed to agree and the step is skipped rather than papered over.
            if polyline.corner_count() < 2 || polyline.first_corner() == polyline.last_corner() {
                continue;
            }

            board.change_trace_polyline(id, polyline.clone());

            // The reference does it the expensive way, and — like the old
            // pull-tight path — gets a fresh id for its trouble.
            reference.remove_item(reference_id(&reference, &prep));
            reference.insert_trace_without_cleaning(
                polyline,
                prep.layer,
                prep.half_width,
                prep.nets.clone(),
                prep.clearance_class,
                prep.fixed_state,
            );

            board.assert_consistent();
            reference.assert_consistent();
            assert_eq!(
                geometry_fingerprint(&board),
                geometry_fingerprint(&reference),
                "seed {seed} step {step}: in-place change diverged from remove + re-insert"
            );
            assert_eq!(
                shape_fingerprint(&board),
                shape_fingerprint(&reference),
                "seed {seed} step {step}: the indexed shapes diverged from remove + re-insert"
            );
        }
    }
}

/// `combine_trace` goes through the in-place path (upstream's
/// `merge_entries_*`), so the same equivalence has to hold for a join: the
/// index a chain of in-place joins leaves behind must be the index a full
/// remove-and-re-insert of every item produces.
///
/// [`Board::rebuild_trees`] *is* that remove-and-re-insert — it drops every
/// tree and re-indexes the store from scratch — so the assertion is "the
/// incrementally maintained index equals the rebuilt one", taken on the same
/// board so the geometry cannot drift and confuse the comparison. (Two boards
/// combined in different orders reach different, equally valid fixed points;
/// that would test the traversal order, not the index.)
#[test]
fn combining_in_place_indexes_exactly_what_a_reinsert_would() {
    for seed in 1..=10u64 {
        let mut rng = Rng::new(seed.wrapping_mul(0xD1B5_4A32_D192_ED03));
        let mut board = new_board();
        // Chains of traces that share corners, so `combine_trace` actually has
        // work: every piece starts where the previous one ended.
        for _ in 0..6 {
            let mut at = IntPoint::new(rng.coord(), rng.coord());
            let layer = rng.below(LAYER_COUNT as u32) as i32;
            let net = 1 + rng.below(NET_COUNT) as i32;
            for _ in 0..4 {
                let next = IntPoint::new(rng.coord(), rng.coord());
                let polyline = Polyline::from_int_points(&[at, next]);
                if polyline.corner_count() >= 2 {
                    board.insert_trace_without_cleaning(
                        polyline,
                        layer,
                        40,
                        NetNos::of(net),
                        1,
                        FixedState::Unfixed,
                    );
                }
                at = next;
            }
        }

        board.ensure_compensated_tree(2);
        for _ in 0..25 {
            let traces = live_traces(&board);
            if traces.is_empty() {
                break;
            }
            let id = traces[rng.below(traces.len() as u32) as usize];
            if board.combine_trace(id) {
                board.assert_consistent();
                assert_trees_match_brute_force(
                    &board,
                    Some(2),
                    &format!("seed {seed}, mid-combine"),
                );
            }
        }
        for net in 1..=NET_COUNT as i32 {
            board.combine_traces(net);
        }
        board.assert_consistent();

        let geometry_after_incremental = geometry_fingerprint(&board);
        let index_after_incremental = shape_fingerprint(&board);
        let leaves_after_incremental = tree_fingerprint(&board);

        board.rebuild_trees();

        assert_eq!(
            geometry_fingerprint(&board),
            geometry_after_incremental,
            "seed {seed}: rebuilding the index changed the items, which it cannot"
        );
        assert_eq!(
            shape_fingerprint(&board),
            index_after_incremental,
            "seed {seed}: the incrementally merged index differs from a full re-insert"
        );
        assert_eq!(
            tree_fingerprint(&board),
            leaves_after_incremental,
            "seed {seed}: the incrementally merged leaf set differs from a full re-insert"
        );
    }
}

/// Rolling a transaction full of in-place changes back has to restore the
/// board **exactly** — items and index — even though the pre-image only
/// reached the journal on the first change of each trace.
#[test]
fn rollback_across_in_place_changes_restores_the_exact_prior_board() {
    for seed in 1..=10u64 {
        let mut rng = Rng::new(seed.wrapping_mul(0xA24B_AED4_963E_E407));
        let mut board = new_board();
        for _ in 0..12 {
            random_trace(&mut board, &mut rng);
            random_via(&mut board, &mut rng);
        }
        board.ensure_compensated_tree(2);

        let items_before = snapshot(&board);
        let trees_before = tree_fingerprint(&board);
        let shapes_before = shape_fingerprint(&board);

        let mark = board.begin();
        // Many changes to *few* traces, which is the case the once-per-
        // transaction pre-image is for: the second and later edits of a trace
        // log nothing, so a rollback that is only correct because every
        // mutation logged would fail here.
        for _ in 0..60 {
            random_polyline_change(&mut board, &mut rng);
        }
        board.rollback_to(mark);

        assert_eq!(board.transaction_depth(), 0);
        board.assert_consistent();
        assert_eq!(
            snapshot(&board),
            items_before,
            "seed {seed}: the items differ from the deep snapshot"
        );
        assert_eq!(
            tree_fingerprint(&board),
            trees_before,
            "seed {seed}: the tree's leaves differ from the deep snapshot"
        );
        assert_eq!(
            shape_fingerprint(&board),
            shapes_before,
            "seed {seed}: the indexed shapes differ from the deep snapshot"
        );
        assert_trees_match_brute_force(&board, Some(2), &format!("seed {seed}, after rollback"));
    }
}

/// Nested transactions with in-place changes at both levels: the inner rollback
/// must undo only the inner edits, and the outer one everything.
#[test]
fn nested_rollback_across_in_place_changes_undoes_one_level_at_a_time() {
    let mut rng = Rng::new(0x5EED_1234_ABCD_9876);
    let mut board = new_board();
    for _ in 0..12 {
        random_trace(&mut board, &mut rng);
    }
    let before_outer = snapshot(&board);

    let outer = board.begin();
    for _ in 0..20 {
        random_polyline_change(&mut board, &mut rng);
    }
    let after_outer_work = snapshot(&board);
    let trees_after_outer_work = tree_fingerprint(&board);

    let inner = board.begin();
    for _ in 0..20 {
        random_polyline_change(&mut board, &mut rng);
    }
    board.rollback_to(inner);

    assert_eq!(board.transaction_depth(), 1);
    assert_eq!(snapshot(&board), after_outer_work);
    assert_eq!(tree_fingerprint(&board), trees_after_outer_work);
    board.assert_consistent();

    board.rollback_to(outer);
    assert_eq!(board.transaction_depth(), 0);
    assert_eq!(snapshot(&board), before_outer);
    board.assert_consistent();
}

// ------------------------------------------------- helpers for the equivalence tests

/// Everything `insert_trace_without_cleaning` needs to rebuild a trace, plus
/// the polyline that identifies it on the reference board.
struct TraceParts {
    polyline: Polyline,
    layer: i32,
    half_width: i32,
    clearance_class: i32,
    fixed_state: FixedState,
    nets: NetNos,
}

fn trace_parts(board: &Board, id: ItemId) -> Option<TraceParts> {
    let item = board.store().get(id)?;
    let ItemKind::Trace(trace) = &item.kind else {
        return None;
    };
    Some(TraceParts {
        polyline: trace.polyline.clone(),
        layer: trace.layer,
        half_width: trace.half_width,
        clearance_class: item.clearance_class,
        fixed_state: item.fixed_state,
        nets: item.nets.clone(),
    })
}

/// The reference board's id for the trace `parts` describes.
///
/// Ids diverge as soon as the reference re-inserts once, so the two boards are
/// matched on content instead.
fn reference_id(board: &Board, parts: &TraceParts) -> ItemId {
    board
        .store()
        .iter_ascending()
        .find(|(_, item)| match &item.kind {
            ItemKind::Trace(trace) => {
                trace.polyline == parts.polyline
                    && trace.layer == parts.layer
                    && trace.half_width == parts.half_width
                    && item.nets.as_slice() == parts.nets.as_slice()
            }
            _ => false,
        })
        .map(|(id, _)| id)
        .unwrap_or(ItemId::UNASSIGNED)
}

/// Every item's geometry, in a canonical order that does not mention ids.
fn geometry_fingerprint(board: &Board) -> Vec<String> {
    let mut out: Vec<String> = board
        .store()
        .iter()
        .map(|(_, item)| format!("{:?}|{:?}|{}", item.kind, item.nets, item.clearance_class))
        .collect();
    out.sort();
    out
}

/// Every *indexed* tile shape of every item, likewise id-free.
///
/// This is the assertion that a reused leaf really does still stand for the
/// shape the slot says it does: it reads the shapes the tree stores, not the
/// ones recomputed from the items.
fn shape_fingerprint(board: &Board) -> Vec<String> {
    let tree = board.default_tree();
    let mut out = Vec::new();
    for (id, _) in board.store().iter() {
        let Some(slot) = board.store().slot_of(id) else {
            continue;
        };
        for (index, tile) in tree.item_shapes(slot, id).iter().enumerate() {
            out.push(format!("{index}|{}|{:?}", tile.layer, tile.shape));
        }
    }
    out.sort();
    out
}

/// The kept ranges are computed from line equality alone, which is upstream's
/// rule (`index_of_first_different_line - 2`). But
/// [`Polyline::offset_shapes`] is not purely local: after building a segment's
/// four border lines it walks *outwards* along the polyline cutting "dog ears"
/// off the corner, for as long as consecutive corners stay within
/// `sqrt(2) * half_width` of each other (`polyline.rs:318-385`). A segment
/// whose own three lines did not move can therefore still have a different tile
/// shape, if a corner close enough to touch its dog-ear scan did.
///
/// This board is built to hit that: half widths far larger than the corner
/// spacing, so every shape's scan runs several segments in each direction. If
/// the kept-range arithmetic is unsound, the brute-force oracle sees it here.
#[test]
fn dense_corners_under_wide_traces_still_match_the_brute_force_scan() {
    for seed in 1..=12u64 {
        let mut rng = Rng::new(seed.wrapping_mul(0xF1E2_D3C4_B5A6_9788));
        let mut board = Board::new(
            LayerStructure::signal_layers(LAYER_COUNT),
            ClearanceTable::uniform(CLEARANCE_CLASSES, LAYER_COUNT, 40),
        );
        board.padstacks_mut().push(Padstack::round(0, 0, 50));

        // Corners 100 units apart under a 300-unit half width: every corner is
        // well inside `sqrt(2) * 300` of its neighbours, so the dog-ear scans
        // reach far past the three lines the kept-range rule accounts for.
        for _ in 0..8 {
            let mut corners = Vec::new();
            let mut at = IntPoint::new(
                (rng.below(9) as i32 - 4) * 100,
                (rng.below(9) as i32 - 4) * 100,
            );
            for _ in 0..(5 + rng.below(4) as usize) {
                corners.push(at);
                at = IntPoint::new(
                    at.x + (rng.below(3) as i32 - 1) * 100,
                    at.y + (rng.below(3) as i32 - 1) * 100,
                );
            }
            let polyline = Polyline::from_int_points(&corners);
            if polyline.corner_count() < 2 {
                continue;
            }
            board.insert_trace_without_cleaning(
                polyline,
                rng.below(LAYER_COUNT as u32) as i32,
                300,
                NetNos::of(1 + rng.below(NET_COUNT) as i32),
                1,
                FixedState::Unfixed,
            );
        }
        board.ensure_compensated_tree(2);

        for step in 0..40 {
            let traces = live_traces(&board);
            if traces.is_empty() {
                break;
            }
            let id = traces[rng.below(traces.len() as u32) as usize];
            // Slide one interior line by a fraction of the half width, which is
            // exactly what `reposition_line` does and what leaves the longest
            // equal prefix and suffix around a genuinely changed segment.
            let Some(polyline) = changed_polyline(&board, id, &mut rng) else {
                continue;
            };
            board.change_trace_polyline(id, polyline);
            board.assert_consistent();
            assert_trees_match_brute_force(
                &board,
                Some(2),
                &format!("seed {seed}, dense step {step}"),
            );
        }
    }
}

// ------------------------------------- the in-place change, adversarially (M3 wave 2 gate)
//
// The three tests above check the in-place change against a *probe set*: a
// fixed list of query shapes, answered by the tree and by a brute-force scan.
// That is the right oracle for "does the index answer correctly", and it is
// blind to exactly one failure mode — a kept leaf whose stored tile shape is
// stale in a way no probe happens to straddle. The two tests below close that
// gap from the other side: they compare the *stored* shapes, exhaustively, and
// they attack the journal's once-per-transaction pre-image with the transaction
// shapes the optimizer actually produces.

/// Every tile shape and every leaf the trees hold, for **all** trees, keyed by
/// (item, shape index) rather than by leaf identity.
///
/// [`shape_fingerprint`] reads the default tree only. The compensated tree
/// stores the same polylines at a *larger* offset width, which is the width the
/// dog-ear horizon is measured in — so it is the tree where a kept-range rule
/// that is sound at half width can still be unsound, and it needs its own
/// exhaustive comparison.
fn all_tree_shape_fingerprint(board: &Board, compensated_class: i32) -> Vec<String> {
    let mut out = Vec::new();
    for (label, tree) in [
        ("default", Some(board.default_tree())),
        (
            "compensated",
            board.trees().compensated_tree(compensated_class),
        ),
    ] {
        let Some(tree) = tree else {
            continue;
        };
        for (id, _) in board.store().iter() {
            let Some(slot) = board.store().slot_of(id) else {
                continue;
            };
            for (index, tile) in tree.item_shapes(slot, id).iter().enumerate() {
                out.push(format!("{label}|{index}|{}|{:?}", tile.layer, tile.shape));
            }
        }
    }
    out.sort();
    out
}

/// The exhaustive form of the reuse contract: after a storm of in-place
/// changes, the shapes the trees *store* must be the shapes a from-scratch
/// rebuild computes — not merely shapes that answer the probe set the same way.
///
/// [`Board::rebuild_trees`] drops every tree and re-offsets every item, so it is
/// the definition of "what remove + re-insert would have produced", taken on the
/// same board so no geometry can drift between the two sides.
///
/// The board is the dense one: half widths several times the corner spacing, so
/// every segment's dog-ear scan reaches past the three lines the kept-range rule
/// accounts for, and a compensated tree on top so the scan is also run at a
/// width the default tree never sees. This is the test that fails if
/// `dogear_safe_keep_at_start` / `_at_end` are removed — verified by reverting
/// them to the identity, where it reports a stale kept shape within two seeds.
#[test]
fn an_in_place_change_storm_stores_exactly_what_a_rebuild_would() {
    for seed in 1..=12u64 {
        let mut rng = Rng::new(seed.wrapping_mul(0xB7E1_5163_8AED_2A6B));
        let mut board = Board::new(
            LayerStructure::signal_layers(LAYER_COUNT),
            ClearanceTable::uniform(CLEARANCE_CLASSES, LAYER_COUNT, 40),
        );
        board.padstacks_mut().push(Padstack::round(0, 0, 50));

        for _ in 0..10 {
            let mut corners = Vec::new();
            let mut at = IntPoint::new(
                (rng.below(9) as i32 - 4) * 100,
                (rng.below(9) as i32 - 4) * 100,
            );
            for _ in 0..(6 + rng.below(6) as usize) {
                corners.push(at);
                at = IntPoint::new(
                    at.x + (rng.below(3) as i32 - 1) * 100,
                    at.y + (rng.below(3) as i32 - 1) * 100,
                );
            }
            let polyline = Polyline::from_int_points(&corners);
            if polyline.corner_count() < 2 {
                continue;
            }
            board.insert_trace_without_cleaning(
                polyline,
                rng.below(LAYER_COUNT as u32) as i32,
                300,
                NetNos::of(1 + rng.below(NET_COUNT) as i32),
                1,
                FixedState::Unfixed,
            );
        }
        board.ensure_compensated_tree(2);

        for step in 0..60 {
            random_polyline_change(&mut board, &mut rng);
            board.assert_consistent();

            let stored = all_tree_shape_fingerprint(&board, 2);
            let mut rebuilt_board = board.clone();
            rebuilt_board.rebuild_trees();
            rebuilt_board.ensure_compensated_tree(2);
            assert_eq!(
                stored,
                all_tree_shape_fingerprint(&rebuilt_board, 2),
                "seed {seed} step {step}: an in-place change left a stored tile shape \
                 a full rebuild does not agree with"
            );
        }
    }
}

/// The journal's once-per-transaction pre-image, attacked with the three
/// transaction shapes that can retire an entry the stamp still points at.
///
/// `ItemStore::mutate` skips cloning a slot whose `saved_epoch` already equals
/// the store's snapshot epoch. That is only sound because *every* event which
/// can retire a logged entry bumps the epoch, and the failure it guards against
/// is silent: the second change of a trace logs nothing, so if the first
/// change's entry is no longer in the log the rollback restores the wrong
/// polyline and nothing complains.
///
/// Three shapes, in one board so they compose:
///
/// 1. **commit then re-open** — the committed level's entries are gone, and the
///    next transaction must not trust the stamp they left behind. (Without the
///    bump in `begin` this is the case that corrupts.)
/// 2. **slot recycling** — a trace is removed and new ones take its slot inside
///    the same transaction, so a stamp written for the old occupant would
///    suppress the new one's pre-image.
/// 3. **repeat edits** — the same few traces changed over and over, which is
///    what makes the skip fire at all.
#[test]
fn the_once_per_transaction_pre_image_survives_commit_recycling_and_repeats() {
    for seed in 1..=10u64 {
        let mut rng = Rng::new(seed.wrapping_mul(0x2B99_2DDF_A232_49D6));
        let mut board = new_board();
        for _ in 0..10 {
            random_trace(&mut board, &mut rng);
            random_via(&mut board, &mut rng);
        }
        board.ensure_compensated_tree(2);

        // Shape 1: a whole transaction's worth of changes, committed. The
        // entries are retired here; every stamp they left is now a lie.
        let closed = board.begin();
        for _ in 0..25 {
            random_polyline_change(&mut board, &mut rng);
        }
        board.commit(closed);

        let items_before = snapshot(&board);
        let trees_before = tree_fingerprint(&board);
        let shapes_before = all_tree_shape_fingerprint(&board, 2);

        // Shapes 2 and 3, in the transaction that has to roll back exactly.
        let mark = board.begin();
        for round in 0..30 {
            random_polyline_change(&mut board, &mut rng);
            random_polyline_change(&mut board, &mut rng);
            if round % 5 == 4 {
                // Free a slot, then fill it again: the recycled slot's stamp
                // must not survive its previous occupant.
                let traces = live_traces(&board);
                if !traces.is_empty() {
                    board.remove_item(traces[rng.below(traces.len() as u32) as usize]);
                }
                random_trace(&mut board, &mut rng);
                random_polyline_change(&mut board, &mut rng);
            }
            if round % 7 == 6 {
                // A nested level that closes: another epoch boundary in the
                // middle of the run of edits.
                let inner = board.begin();
                random_polyline_change(&mut board, &mut rng);
                board.commit(inner);
            }
        }
        board.rollback_to(mark);

        assert_eq!(board.transaction_depth(), 0);
        board.assert_consistent();
        assert_eq!(
            snapshot(&board),
            items_before,
            "seed {seed}: rollback restored the wrong items — a pre-image was skipped \
             that was not still in the log"
        );
        assert_eq!(
            tree_fingerprint(&board),
            trees_before,
            "seed {seed}: rollback restored the wrong leaves"
        );
        assert_eq!(
            all_tree_shape_fingerprint(&board, 2),
            shapes_before,
            "seed {seed}: rollback restored the wrong stored tile shapes"
        );
        assert_trees_match_brute_force(&board, Some(2), &format!("seed {seed}, after rollback"));
    }
}

/// An inner rollback **drains** the entries it undid out of the journal, so the
/// stamps they left behind have to be retired with them.
///
/// This is the one epoch boundary the other tests walk past. Its failure needs
/// four steps in this order, and nothing shorter:
///
/// 1. a trace is changed for the first time *inside* an inner level, so its
///    only pre-image is the one that level logged;
/// 2. the inner level is rolled back, which removes that entry from the log;
/// 3. the same trace is changed again at the outer level — and if the stamp
///    survived step 2, `mutate` believes a pre-image is already logged and
///    writes none;
/// 4. the outer level is rolled back, and finds nothing to restore the trace
///    from.
///
/// The result is a board that silently keeps a polyline the transaction was
/// supposed to have undone. Verified causal: dropping the `bump_snapshot_epoch`
/// from `ItemStore::rollback_to` fails this test and no other in the suite.
#[test]
fn an_inner_rollback_retires_the_pre_images_it_drained() {
    for seed in 1..=10u64 {
        let mut rng = Rng::new(seed.wrapping_mul(0x9E6C_63D0_676A_9A99));
        let mut board = new_board();
        for _ in 0..8 {
            random_trace(&mut board, &mut rng);
        }
        board.ensure_compensated_tree(2);

        let traces = live_traces(&board);
        if traces.is_empty() {
            continue;
        }
        // The store has never opened a transaction, so nothing is stamped and
        // the inner level below logs this trace's first pre-image.
        let items_before = snapshot(&board);
        let trees_before = tree_fingerprint(&board);

        let outer = board.begin();
        let inner = board.begin();
        let mut changed_any = false;
        for &id in &traces {
            if let Some(polyline) = changed_polyline(&board, id, &mut rng) {
                changed_any |= board.change_trace_polyline(id, polyline);
            }
        }
        if !changed_any {
            continue;
        }
        board.rollback_to(inner);
        assert_eq!(
            snapshot(&board),
            items_before,
            "seed {seed}: the inner rollback itself is wrong"
        );

        // Same traces, at the outer level, after their pre-images were drained.
        for &id in &traces {
            if let Some(polyline) = changed_polyline(&board, id, &mut rng) {
                board.change_trace_polyline(id, polyline);
            }
        }
        board.rollback_to(outer);

        assert_eq!(board.transaction_depth(), 0);
        board.assert_consistent();
        assert_eq!(
            snapshot(&board),
            items_before,
            "seed {seed}: a change made after an inner rollback was not undone — its \
             pre-image was skipped against an entry the inner rollback had drained"
        );
        assert_eq!(
            tree_fingerprint(&board),
            trees_before,
            "seed {seed}: the leaves were not restored"
        );
        assert_trees_match_brute_force(&board, Some(2), &format!("seed {seed}, after rollback"));
    }
}

/// The kept **suffix** of a segment-count-changing move is relabelled where it
/// sits, and the relabel has to land on the right index.
///
/// This is the one path `an_in_place_change_storm_stores_exactly_what_a_rebuild_would`
/// reaches only by luck of the dice, and the one where the shipped code and
/// upstream do the same thing by different means: upstream mutates
/// `leaf.shape_index_in_object` on a heap `Leaf` (`ShapeSearchTree.java:135`),
/// this port calls `ArenaShapeTree::set_payload` on a leaf that never leaves the
/// tree. Both are only correct if the *new* index is right, and an off-by-one
/// there is invisible to a query-shaped oracle: every shape is still indexed at
/// the same place, just filed under a neighbouring index — which is exactly the
/// kind of bookkeeping error a router turns into a shove that reads the wrong
/// tile.
///
/// So the oracle is [`tree_fingerprint`] — every leaf as `(item, shape index)` —
/// against [`Board::rebuild_trees`], which relabels nothing because it starts
/// from nothing.
///
/// The move is "drop the second line", repeatedly: it leaves `keep_at_start` at
/// 0 and `keep_at_end` at everything else, so the whole of the trace's suffix
/// shifts down by one on every step. Twelve steps compound twelve relabels onto
/// leaves that were themselves already relabelled.
#[test]
fn dropping_a_line_relabels_the_kept_suffix_onto_the_right_indices() {
    for seed in 1..=8u64 {
        let mut rng = Rng::new(seed.wrapping_mul(0x2545_F491_4F6C_DD1D));
        let mut board = new_board();

        // Neighbours first, so the arena has structure around the trace under
        // test and a relabelled leaf has somewhere wrong to end up.
        for _ in 0..5 {
            let a = IntPoint::new(rng.coord(), rng.coord());
            let b = IntPoint::new(rng.coord(), rng.coord());
            let polyline = Polyline::from_int_points(&[a, b]);
            if polyline.corner_count() >= 2 {
                board.insert_trace_without_cleaning(
                    polyline,
                    rng.below(LAYER_COUNT as u32) as i32,
                    120,
                    NetNos::of(1 + rng.below(NET_COUNT) as i32),
                    1,
                    FixedState::Unfixed,
                );
            }
        }

        // One long staircase: sixteen corners, so there is a long suffix to
        // shift and the dog-ear horizon does not swallow it.
        let mut corners = Vec::new();
        let mut at = IntPoint::new(-4000, -4000);
        for i in 0..16 {
            corners.push(at);
            at = if i % 2 == 0 {
                IntPoint::new(at.x + 500, at.y)
            } else {
                IntPoint::new(at.x, at.y + 500)
            };
        }
        let long_trace = board
            .insert_trace_without_cleaning(
                Polyline::from_int_points(&corners),
                0,
                120,
                NetNos::of(1),
                1,
                FixedState::Unfixed,
            )
            .expect("the staircase inserts");
        board.ensure_compensated_tree(2);

        let mut shrinks = 0;
        for step in 0..12 {
            let Some(ItemKind::Trace(trace)) = board.store().get(long_trace).map(|i| &i.kind)
            else {
                break;
            };
            let lines = &trace.polyline.arr;
            if lines.len() < 6 {
                break;
            }
            let before = lines.len();
            // Drop the line just after the start line: `new[0] == old[0]` and
            // nothing else lines up at the front, so `keep_at_start` is 0 and
            // every remaining shape is a kept, shifted one.
            let mut edited = lines.to_vec();
            edited.remove(1);
            let polyline = Polyline::new_from_lines(&edited);
            if polyline.corner_count() < 2 || polyline.first_corner() == polyline.last_corner() {
                break;
            }
            if !board.change_trace_polyline(long_trace, polyline) {
                break;
            }
            let Some(ItemKind::Trace(trace)) = board.store().get(long_trace).map(|i| &i.kind)
            else {
                break;
            };
            if trace.polyline.arr.len() < before {
                shrinks += 1;
            }

            board.assert_consistent();
            assert_trees_match_brute_force(
                &board,
                Some(2),
                &format!("seed {seed}, relabel step {step}"),
            );
        }
        assert!(
            shrinks >= 3,
            "seed {seed}: the suffix-shift path never ran, so this test proved nothing"
        );

        let leaves_after_incremental = tree_fingerprint(&board);
        let shapes_after_incremental = all_tree_shape_fingerprint(&board, 2);
        let geometry_after_incremental = geometry_fingerprint(&board);

        board.rebuild_trees();

        assert_eq!(
            geometry_fingerprint(&board),
            geometry_after_incremental,
            "seed {seed}: rebuilding the index changed the items, which it cannot"
        );
        assert_eq!(
            tree_fingerprint(&board),
            leaves_after_incremental,
            "seed {seed}: a relabelled leaf carries an index a rebuild does not agree with"
        );
        assert_eq!(
            all_tree_shape_fingerprint(&board, 2),
            shapes_after_incremental,
            "seed {seed}: a relabelled leaf's stored shape is not the rebuilt one"
        );
    }
}
