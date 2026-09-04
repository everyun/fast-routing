//! `tree_nodes_visited` is charged, and charged once per query.
//!
//! The counter is process-wide and summed over every thread
//! (`fr_metrics::counters::snapshot`), so this file holds **exactly one test**:
//! `cargo test` runs integration test files in separate binaries but the tests
//! inside one binary in parallel, and a second test doing a tree query here
//! would make both readings non-deterministic.
//!
//! The counter used to be permanently zero — nothing bumped it, because the
//! only honest place to count from is inside the tree walk, and the walk lives
//! in `fr-datastructures`, which may not depend on `fr-metrics`. The fix is that
//! `ArenaShapeTree::visit_overlaps` *returns* its node-test count and
//! `SearchTree::visit` charges it once, so the hot loop keeps its accumulator in
//! a register.

use fr_board::clearance::ClearanceTable;
use fr_board::item::{FixedState, Item, ItemKind, PolylineTrace};
use fr_board::net_nos::NetNos;
use fr_board::search_tree::{SearchTree, ShapeContext, MAX_TREE_SHAPE_WIDTH};
use fr_board::shape::{AngleRestriction, PadstackTable, TileShape};
use fr_board::store::ItemStore;
use fr_datastructures::arena_shape_tree::QueryStack;
use fr_geometry::planar::{IntBox, IntPoint, Polyline};
use fr_metrics::counters::{value, Counter};

fn trace(x: i32) -> Item {
    Item::new(
        NetNos::from_slice(&[1]),
        1,
        FixedState::Unfixed,
        ItemKind::Trace(PolylineTrace {
            polyline: Polyline::from_int_points(&[
                IntPoint::new(x, 0),
                IntPoint::new(x + 1000, 0),
                IntPoint::new(x + 1000, 1000),
            ]),
            layer: 0,
            half_width: 50,
        }),
    )
}

#[test]
fn a_query_charges_the_node_counter_once_with_the_walks_own_total() {
    let clearances = ClearanceTable::uniform(3, 2, 20);
    let padstacks = PadstackTable::new();
    let ctx = ShapeContext {
        clearances: &clearances,
        padstacks: &padstacks,
        layer_count: 2,
        angle_restriction: AngleRestriction::FortyFive,
        max_tree_shape_width: MAX_TREE_SHAPE_WIDTH,
    };

    let mut store = ItemStore::new();
    let mut tree = SearchTree::new(0);
    let everything = TileShape::Box(IntBox::new(-100_000, -100_000, 100_000, 100_000));
    let mut stack = QueryStack::new();
    let mut out = Vec::new();

    // An empty tree has no node to test, so nothing is charged — the counter is
    // the walk's work, not the query count.
    let start = value(Counter::TreeNodesVisited);
    tree.overlapping_tree_entries(&store, &everything, None, &[], &mut stack, &mut out);
    assert_eq!(
        value(Counter::TreeNodesVisited),
        start,
        "an empty tree tests no nodes"
    );

    const TRACES: usize = 8;
    for i in 0..TRACES {
        let id = store.insert(trace(i as i32 * 4_000));
        tree.insert(store.slot_of(id).unwrap(), store.get(id).unwrap(), ctx);
    }
    let leaves = tree.leaf_count();
    assert_eq!(leaves, 2 * TRACES, "two segments per trace");

    let before = value(Counter::TreeNodesVisited);
    tree.overlapping_tree_entries(&store, &everything, None, &[], &mut stack, &mut out);
    let charged = value(Counter::TreeNodesVisited) - before;
    assert_eq!(out.len(), leaves, "the sweep reaches every leaf");
    assert!(charged > 0, "the counter is no longer permanently zero");
    // The exact number is the insertion history's, so it is bracketed rather
    // than pinned: a sweep that reports every leaf has to have tested every
    // leaf, and a binary tree of `n` leaves has `2n - 1` nodes, each tested at
    // most once as a child plus the root.
    let nodes = (2 * leaves - 1) as u64;
    assert!(
        (leaves as u64..=nodes + 1).contains(&charged),
        "charged {charged} for a {leaves}-leaf ({nodes}-node) tree"
    );

    // A query that the root rejects outright tests exactly the root, which is
    // what makes this a *pruning* proxy rather than a query counter.
    let far = TileShape::Box(IntBox::new(1_000_000, 1_000_000, 1_000_001, 1_000_001));
    let before = value(Counter::TreeNodesVisited);
    tree.overlapping_tree_entries(&store, &far, None, &[], &mut stack, &mut out);
    assert!(out.is_empty());
    assert_eq!(value(Counter::TreeNodesVisited) - before, 1);

    // The layer fast reject happens after the walk, so it changes what is
    // *reported*, never what is charged.
    let before = value(Counter::TreeNodesVisited);
    tree.overlapping_tree_entries(&store, &everything, Some(1), &[], &mut stack, &mut out);
    assert!(out.is_empty(), "every shape is on layer 0");
    assert_eq!(
        value(Counter::TreeNodesVisited) - before,
        charged,
        "the layer filter is not a pruning of the walk"
    );
}
