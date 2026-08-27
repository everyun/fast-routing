//! Differential and regression tests for [`ArenaShapeTree`].
//!
//! Three properties are pinned here, matching the acceptance criteria of the
//! arena shape tree (`docs/upstream-study/search-tree-obstacles.md` §10):
//!
//! 1. a long random insert/remove/query workload agrees with a linear scan;
//! 2. the STR bulk load indexes exactly the same set as greedy insertion;
//! 3. a stale [`ArenaLeafId`] never resolves after its slot is reused (the ABA
//!    hazard of `MinAreaTree`'s bare `LeafId(usize)`).

use fr_datastructures::arena_shape_tree::{ArenaLeafId, ArenaShapeTree, OctBounds, QueryStack};

/// xorshift64* — a deterministic, dependency-free PRNG for the fixtures.
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

    /// Uniform value in `0..bound` (bound > 0).
    fn below(&mut self, bound: u32) -> u32 {
        (self.next_u64() >> 32) as u32 % bound
    }

    fn range(&mut self, low: i32, high: i32) -> i32 {
        low + self.below((high - low + 1) as u32) as i32
    }
}

/// Half-extent of the coordinate window the fixtures draw from. Sized so that
/// a query hits a handful of leaves — a sparser world would make the
/// differential comparison vacuous.
const WORLD: i32 = 15_000;

/// A random, normalized octagon: an axis-parallel box with four corner cuts
/// small enough that they cannot overlap.
fn random_octagon(rng: &mut Rng, max_size: i32) -> OctBounds {
    let lx = rng.range(-WORLD, WORLD);
    let ly = rng.range(-WORLD, WORLD);
    let rx = lx + rng.range(0, max_size);
    let uy = ly + rng.range(0, max_size);
    let max_cut = (rx - lx).min(uy - ly) / 2;
    let cut = |rng: &mut Rng| {
        if max_cut <= 0 {
            0
        } else {
            rng.range(0, max_cut)
        }
    };
    let ll = cut(rng);
    let lr = cut(rng);
    let ur = cut(rng);
    let ul = cut(rng);
    OctBounds::new(
        lx,
        ly,
        rx,
        uy,
        (lx - uy) + ul,
        (rx - ly) - lr,
        (lx + ly) + ll,
        (rx + uy) - ur,
    )
}

fn brute_force(entries: &[(OctBounds, u64)], query: &OctBounds) -> Vec<u64> {
    let mut hits: Vec<u64> = entries
        .iter()
        .filter(|(bounds, _)| bounds.intersects(query))
        .map(|&(_, payload)| payload)
        .collect();
    hits.sort_unstable();
    hits
}

fn sorted_payloads(out: &[(ArenaLeafId, u64)]) -> Vec<u64> {
    let mut hits: Vec<u64> = out.iter().map(|&(_, payload)| payload).collect();
    hits.sort_unstable();
    hits
}

#[test]
fn random_workload_matches_a_linear_scan() {
    let mut rng = Rng::new(0x5EED_1234_ABCD_0001);
    let mut tree: ArenaShapeTree<u64> = ArenaShapeTree::new();
    // Mirror of the live leaves, in insertion order.
    let mut live: Vec<(ArenaLeafId, OctBounds, u64)> = Vec::new();
    let mut stack = QueryStack::new();
    let mut out = Vec::new();
    let mut next_payload = 0u64;

    let mut queries = 0usize;
    let mut total_hits = 0usize;
    let mut removals = 0usize;

    for op in 0..10_000 {
        match rng.below(100) {
            // 55 % insert
            0..=54 => {
                let bounds = random_octagon(&mut rng, 2_000);
                let payload = next_payload;
                next_payload += 1;
                let id = tree.insert(bounds, payload);
                live.push((id, bounds, payload));
            }
            // 20 % remove a random live leaf
            55..=74 => {
                if live.is_empty() {
                    continue;
                }
                let victim = rng.below(live.len() as u32) as usize;
                let (id, _, payload) = live.swap_remove(victim);
                assert_eq!(tree.remove(id), Some(payload), "op {op}: removal failed");
                assert!(!tree.contains_leaf(id), "op {op}: handle survived removal");
                removals += 1;
            }
            // 25 % query
            _ => {
                let query = random_octagon(&mut rng, 4_000);
                tree.overlaps_with_stack(&query, &mut stack, &mut out);

                let entries: Vec<(OctBounds, u64)> = live.iter().map(|&(_, b, p)| (b, p)).collect();
                let expected = brute_force(&entries, &query);
                let actual = sorted_payloads(&out);
                assert_eq!(actual, expected, "op {op}: query result mismatch");

                // Every reported handle must still resolve to its payload.
                for &(id, payload) in out.iter() {
                    assert_eq!(tree.get(id), Some(payload));
                }
                queries += 1;
                total_hits += actual.len();
            }
        }

        assert_eq!(tree.len(), live.len(), "op {op}: leaf count drifted");
        if op % 250 == 0 {
            tree.check_invariants()
                .unwrap_or_else(|e| panic!("op {op}: {e}"));
        }
    }

    tree.check_invariants().unwrap();
    assert!(removals > 500, "workload exercised too few removals");
    assert!(queries > 1_000, "workload exercised too few queries");
    assert!(
        total_hits > 5 * queries,
        "workload was too sparse to be meaningful: {total_hits} hits over {queries} queries"
    );
}

#[test]
fn epoch_counts_every_mutation_and_no_read() {
    let mut tree: ArenaShapeTree<u64> = ArenaShapeTree::new();
    assert_eq!(tree.epoch(), 0);

    let mut ids = Vec::new();
    for i in 0..16u64 {
        ids.push(tree.insert(OctBounds::from_box(0, 0, i as i32 + 1, 1), i));
        assert_eq!(tree.epoch(), i + 1);
    }

    let before = tree.epoch();
    let mut out = Vec::new();
    tree.overlaps(&OctBounds::from_box(0, 0, 100, 100), &mut out);
    let _ = tree.get(ids[0]);
    let _ = tree.depth_stats();
    let _ = tree.iter_leaves().count();
    assert_eq!(tree.epoch(), before, "reads must not advance the epoch");

    tree.remove(ids[3]).unwrap();
    assert_eq!(tree.epoch(), before + 1);
    // A rejected removal is not a mutation.
    assert_eq!(tree.remove(ids[3]), None);
    assert_eq!(tree.epoch(), before + 1);

    tree.clear();
    assert_eq!(tree.epoch(), before + 2);
}

#[test]
fn str_bulk_load_indexes_the_same_set_as_greedy_insertion() {
    let mut rng = Rng::new(0x5EED_1234_ABCD_0002);
    let entries: Vec<(OctBounds, u64)> = (0..2_000u64)
        .map(|i| (random_octagon(&mut rng, 1_500), i))
        .collect();

    let mut greedy: ArenaShapeTree<u64> = ArenaShapeTree::new();
    for &(bounds, payload) in &entries {
        greedy.insert(bounds, payload);
    }

    let mut ids = Vec::new();
    let packed = ArenaShapeTree::bulk_load_with_ids(&entries, &mut ids);

    packed.check_invariants().unwrap();
    assert_eq!(packed.len(), entries.len());
    assert_eq!(packed.node_count(), 2 * entries.len() - 1);
    assert_eq!(packed.epoch(), 0);
    assert_eq!(ids.len(), entries.len());

    // The reported handles line up with the input order.
    for (i, &(bounds, payload)) in entries.iter().enumerate() {
        assert_eq!(packed.get(ids[i]), Some(payload));
        assert_eq!(packed.leaf_bounds(ids[i]), Some(bounds));
    }

    // The packed tree is balanced; the greedy one is not.
    let packed_depth = packed.depth_stats();
    let greedy_depth = greedy.depth_stats();
    let ceil_log2 = usize::BITS - (entries.len() - 1).leading_zeros();
    assert_eq!(packed_depth.leaves, entries.len());
    assert!(
        packed_depth.max_depth <= ceil_log2 as usize,
        "packed depth {} exceeds ceil(log2(n)) = {ceil_log2}",
        packed_depth.max_depth
    );
    assert!(
        packed_depth.max_depth < greedy_depth.max_depth,
        "packing did not improve the tree: packed {} vs greedy {}",
        packed_depth.max_depth,
        greedy_depth.max_depth
    );

    // Same answers, modulo traversal order.
    let mut stack = QueryStack::new();
    let mut packed_out = Vec::new();
    let mut greedy_out = Vec::new();
    for _ in 0..1_000 {
        let query = random_octagon(&mut rng, 3_000);
        packed.overlaps_with_stack(&query, &mut stack, &mut packed_out);
        greedy.overlaps_with_stack(&query, &mut stack, &mut greedy_out);
        let expected = brute_force(&entries, &query);
        assert_eq!(sorted_payloads(&packed_out), expected);
        assert_eq!(sorted_payloads(&greedy_out), expected);
    }

    // A packed tree stays mutable: its handles remove correctly.
    let mut packed = packed;
    for (i, &(_, payload)) in entries.iter().enumerate().take(500) {
        assert_eq!(packed.remove(ids[i]), Some(payload));
    }
    packed.check_invariants().unwrap();
    assert_eq!(packed.len(), entries.len() - 500);

    let mut rest: Vec<(OctBounds, u64)> = entries[500..].to_vec();
    rest.sort_unstable_by_key(|&(_, payload)| payload);
    for _ in 0..200 {
        let query = random_octagon(&mut rng, 3_000);
        packed.overlaps_with_stack(&query, &mut stack, &mut packed_out);
        assert_eq!(sorted_payloads(&packed_out), brute_force(&rest, &query));
    }
}

#[test]
fn bulk_load_handles_degenerate_inputs() {
    let empty: ArenaShapeTree<u64> = ArenaShapeTree::bulk_load(&[]);
    assert!(empty.is_empty());
    assert_eq!(empty.root_bounds(), None);
    empty.check_invariants().unwrap();

    let single = ArenaShapeTree::bulk_load(&[(OctBounds::from_box(1, 2, 3, 4), 7u64)]);
    assert_eq!(single.len(), 1);
    assert_eq!(single.node_count(), 1);
    assert_eq!(single.root_bounds(), Some(OctBounds::from_box(1, 2, 3, 4)));
    single.check_invariants().unwrap();

    // All entries identical: the median split must still terminate.
    let duplicates: Vec<(OctBounds, u64)> = (0..64u64)
        .map(|i| (OctBounds::from_box(10, 10, 20, 20), i))
        .collect();
    let tree = ArenaShapeTree::bulk_load(&duplicates);
    tree.check_invariants().unwrap();
    assert_eq!(tree.len(), 64);
    assert_eq!(tree.depth_stats().max_depth, 6);

    let mut out = Vec::new();
    tree.overlaps(&OctBounds::from_box(15, 15, 15, 15), &mut out);
    assert_eq!(out.len(), 64);
}

#[test]
fn bulk_load_is_deterministic() {
    let mut rng = Rng::new(0x5EED_1234_ABCD_0003);
    let entries: Vec<(OctBounds, u64)> = (0..500u64)
        .map(|i| (random_octagon(&mut rng, 900), i))
        .collect();

    let mut first_ids = Vec::new();
    let first = ArenaShapeTree::bulk_load_with_ids(&entries, &mut first_ids);
    let mut second_ids = Vec::new();
    let second = ArenaShapeTree::bulk_load_with_ids(&entries, &mut second_ids);

    assert_eq!(first_ids, second_ids);
    let first_leaves: Vec<_> = first.iter_leaves().collect();
    let second_leaves: Vec<_> = second.iter_leaves().collect();
    assert_eq!(first_leaves, second_leaves);

    let mut a = Vec::new();
    let mut b = Vec::new();
    for _ in 0..200 {
        let query = random_octagon(&mut rng, 2_000);
        first.overlaps(&query, &mut a);
        second.overlaps(&query, &mut b);
        assert_eq!(a, b, "identical inputs must give identical traversal order");
    }
}

/// Regression: a handle into a reused slot must not resolve.
///
/// `MinAreaTree`'s `LeafId(usize)` is a bare slot index, so after a removal the
/// slot can be handed to an unrelated leaf and the stale handle silently
/// addresses it (flagged in review). The generation stamp closes that.
#[test]
fn stale_handles_never_resolve_after_slot_reuse() {
    let mut tree: ArenaShapeTree<&'static str> = ArenaShapeTree::new();

    let a = tree.insert(OctBounds::from_box(0, 0, 10, 10), "a");
    assert_eq!(tree.remove(a), Some("a"));
    let b = tree.insert(OctBounds::from_box(100, 100, 110, 110), "b");

    // The slot really is recycled — otherwise the test would prove nothing.
    assert_eq!(a.slot(), b.slot(), "expected the freed slot to be reused");
    assert_ne!(a.generation(), b.generation());

    assert_eq!(tree.get(a), None, "stale handle resolved to the new leaf");
    assert_eq!(tree.leaf_bounds(a), None);
    assert!(!tree.contains_leaf(a));
    assert_eq!(tree.remove(a), None, "stale handle removed the new leaf");

    // "b" is untouched by the rejected operations.
    assert_eq!(tree.len(), 1);
    assert_eq!(tree.get(b), Some("b"));
    tree.check_invariants().unwrap();

    // Same story once inner-node slots are in play, and across many reuses of
    // the same slot.
    let keep = tree.insert(OctBounds::from_box(-5, -5, -1, -1), "keep");
    let mut stale = Vec::new();
    for i in 0..64 {
        let id = tree.insert(OctBounds::from_box(i, i, i + 1, i + 1), "churn");
        stale.push(id);
        assert_eq!(tree.remove(id), Some("churn"));
    }
    for id in &stale {
        assert_eq!(tree.get(*id), None);
        assert_eq!(tree.remove(*id), None);
    }
    assert_eq!(tree.len(), 2);
    assert_eq!(tree.get(b), Some("b"));
    assert_eq!(tree.get(keep), Some("keep"));
    tree.check_invariants().unwrap();

    // An out-of-range / never-issued handle is rejected too.
    assert_eq!(tree.get(ArenaLeafId::INVALID), None);
    assert_eq!(tree.remove(ArenaLeafId::INVALID), None);
}

/// A handle to an inner node's slot must not be mistaken for a leaf either.
#[test]
fn stale_handle_pointing_at_an_inner_node_is_rejected() {
    let mut tree: ArenaShapeTree<u32> = ArenaShapeTree::new();
    let first = tree.insert(OctBounds::from_box(0, 0, 1, 1), 0);
    let second = tree.insert(OctBounds::from_box(2, 2, 3, 3), 1);
    // Removing `second` frees its slot and the inner node's slot; the next
    // insert reuses one of them for an inner node.
    tree.remove(second).unwrap();
    tree.insert(OctBounds::from_box(4, 4, 5, 5), 2);
    tree.insert(OctBounds::from_box(6, 6, 7, 7), 3);

    assert_eq!(tree.get(second), None);
    assert_eq!(tree.remove(second), None);
    assert_eq!(tree.get(first), Some(0));
    assert_eq!(tree.len(), 3);
    tree.check_invariants().unwrap();
}
