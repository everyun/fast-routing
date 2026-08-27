//! Independent adversarial probes for `ArenaShapeTree`, written during review.
//!
//! These attack the parts of the contract the implementing agent asserted but
//! did not stress: slab reuse across `clear()`, handle hygiene after repeated
//! remove/insert churn, the structural invariants under a long mixed workload,
//! and the query result set for bounds the fixtures never generate
//! (empty-parameter, fully nested, identical, and axis-degenerate octagons).

use fr_datastructures::arena_shape_tree::{ArenaLeafId, ArenaShapeTree, OctBounds, QueryStack};
use std::collections::HashSet;

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
    fn range(&mut self, low: i32, high: i32) -> i32 {
        low + self.below((high - low + 1) as u32) as i32
    }
}

/// Deliberately *unconstrained* bounds: unlike the agent's fixture this can
/// produce octagons whose diagonal parameters are redundant, tight, or
/// inconsistent with the box, plus zero-extent boxes. The broad-phase
/// `intersects` contract has to hold for all of them.
fn wild_bounds(rng: &mut Rng) -> OctBounds {
    match rng.below(5) {
        0 => {
            // degenerate: a single point
            let x = rng.range(-500, 500);
            let y = rng.range(-500, 500);
            OctBounds::from_box(x, y, x, y)
        }
        1 => {
            // axis-degenerate: zero height
            let x = rng.range(-500, 500);
            let y = rng.range(-500, 500);
            OctBounds::from_box(x, y, x + rng.range(0, 300), y)
        }
        2 => {
            // plain box (diagonals tight)
            let lx = rng.range(-500, 500);
            let ly = rng.range(-500, 500);
            OctBounds::from_box(lx, ly, lx + rng.range(0, 400), ly + rng.range(0, 400))
        }
        3 => {
            // redundant diagonals (slack far beyond the box corners)
            let lx = rng.range(-500, 500);
            let ly = rng.range(-500, 500);
            let rx = lx + rng.range(0, 400);
            let uy = ly + rng.range(0, 400);
            OctBounds::new(
                lx,
                ly,
                rx,
                uy,
                lx - uy - 999,
                rx - ly + 999,
                lx + ly - 999,
                rx + uy + 999,
            )
        }
        _ => {
            // a real clipped octagon
            let lx = rng.range(-500, 500);
            let ly = rng.range(-500, 500);
            let rx = lx + rng.range(0, 400);
            let uy = ly + rng.range(0, 400);
            let cut = ((rx - lx).min(uy - ly) / 2).max(0);
            let c = |rng: &mut Rng| if cut == 0 { 0 } else { rng.range(0, cut) };
            let (a, b, d, e) = (c(rng), c(rng), c(rng), c(rng));
            OctBounds::new(
                lx,
                ly,
                rx,
                uy,
                (lx - uy) + a,
                (rx - ly) - b,
                (lx + ly) + d,
                (rx + uy) - e,
            )
        }
    }
}

fn brute(entries: &[(OctBounds, u64)], q: &OctBounds) -> Vec<u64> {
    let mut hits: Vec<u64> = entries
        .iter()
        .filter(|(b, _)| b.intersects(q))
        .map(|&(_, p)| p)
        .collect();
    hits.sort_unstable();
    hits
}

/// A long mixed workload including `clear()`, degenerate bounds and heavy slot
/// reuse; the structural invariants are re-checked continuously, not only at
/// the end.
#[test]
fn churn_with_clear_keeps_invariants_and_query_results() {
    let mut rng = Rng::new(0xC0FF_EE00_1234_5678);
    let mut tree: ArenaShapeTree<u64> = ArenaShapeTree::new();
    let mut live: Vec<(ArenaLeafId, OctBounds, u64)> = Vec::new();
    let mut stack = QueryStack::new();
    let mut out = Vec::new();
    let mut next = 0u64;
    let mut clears = 0u32;
    let mut queries = 0u32;

    for op in 0..8_000u32 {
        match rng.below(100) {
            0..=49 => {
                let b = wild_bounds(&mut rng);
                let id = tree.insert(b, next);
                live.push((id, b, next));
                next += 1;
            }
            50..=74 => {
                if live.is_empty() {
                    continue;
                }
                let victim = rng.below(live.len() as u32) as usize;
                let (id, _, payload) = live.swap_remove(victim);
                assert_eq!(tree.remove(id), Some(payload), "op {op}");
                // Removing the same handle twice must be a no-op, not a
                // double decrement of the leaf count.
                let before = tree.len();
                assert_eq!(tree.remove(id), None, "op {op}: stale removal succeeded");
                assert_eq!(tree.len(), before, "op {op}: stale removal changed len");
            }
            75..=97 => {
                let q = wild_bounds(&mut rng);
                tree.overlaps_with_stack(&q, &mut stack, &mut out);
                let entries: Vec<(OctBounds, u64)> = live.iter().map(|&(_, b, p)| (b, p)).collect();
                let mut actual: Vec<u64> = out.iter().map(|&(_, p)| p).collect();
                actual.sort_unstable();
                assert_eq!(actual, brute(&entries, &q), "op {op}: query mismatch");
                queries += 1;
            }
            _ => {
                let stale: Vec<ArenaLeafId> = live.iter().map(|&(id, _, _)| id).collect();
                tree.clear();
                clears += 1;
                for id in stale {
                    assert!(!tree.contains_leaf(id), "op {op}: handle survived clear");
                    assert_eq!(tree.remove(id), None, "op {op}: stale remove after clear");
                }
                live.clear();
                assert_eq!(tree.len(), 0);
            }
        }
        assert_eq!(tree.len(), live.len(), "op {op}: leaf count drifted");
        if op % 250 == 0 {
            tree.check_invariants()
                .unwrap_or_else(|e| panic!("op {op}: {e}"));
        }
    }
    tree.check_invariants().unwrap();
    assert!(clears > 10 && queries > 500);
}

/// Handles must be unique among *live* leaves at all times. If a generation
/// were ever not bumped on free, two live leaves could share a handle and the
/// board layer would silently alias two items.
#[test]
fn live_handles_are_never_duplicated_under_heavy_slot_reuse() {
    let mut rng = Rng::new(0x1234_5678_9ABC_DEF0);
    let mut tree: ArenaShapeTree<u64> = ArenaShapeTree::new();
    let mut live: Vec<(ArenaLeafId, u64)> = Vec::new();
    let mut retired: Vec<ArenaLeafId> = Vec::new();
    let mut next = 0u64;

    for _ in 0..3_000 {
        if live.len() < 40 || rng.below(2) == 0 {
            let id = tree.insert(wild_bounds(&mut rng), next);
            live.push((id, next));
            next += 1;
        } else {
            let victim = rng.below(live.len() as u32) as usize;
            let (id, payload) = live.swap_remove(victim);
            assert_eq!(tree.remove(id), Some(payload));
            retired.push(id);
        }

        let unique: HashSet<ArenaLeafId> = live.iter().map(|&(id, _)| id).collect();
        assert_eq!(unique.len(), live.len(), "two live leaves share a handle");
        for &(id, payload) in &live {
            assert_eq!(tree.get(id), Some(payload));
        }
    }
    // Every retired handle stays dead forever, even though its slot has almost
    // certainly been reused many times over.
    for id in retired {
        assert!(!tree.contains_leaf(id));
        assert_eq!(tree.get(id), None);
    }
    assert!(
        tree.slab_len() < 4 * (2 * live.len() + 1),
        "slab did not reuse slots"
    );
}

/// The STR bulk loader and greedy insertion must index the same set for the
/// pathological inputs the differential fixture never draws: all-identical
/// bounds, fully nested bounds, and a single degenerate point repeated.
#[test]
fn bulk_load_matches_greedy_on_pathological_inputs() {
    let cases: Vec<Vec<OctBounds>> = vec![
        // 200 identical octagons
        (0..200)
            .map(|_| OctBounds::from_box(-10, -10, 10, 10))
            .collect(),
        // fully nested
        (0..200)
            .map(|i| OctBounds::from_box(-(200 - i), -(200 - i), 200 - i, 200 - i))
            .collect(),
        // a repeated single point
        (0..64).map(|_| OctBounds::from_box(7, 7, 7, 7)).collect(),
        // a long collinear chain (worst case for a median split)
        (0..256)
            .map(|i| OctBounds::from_box(i, 0, i + 1, 1))
            .collect(),
    ];

    for (case_no, bounds) in cases.iter().enumerate() {
        let entries: Vec<(OctBounds, u64)> = bounds
            .iter()
            .enumerate()
            .map(|(i, &b)| (b, i as u64))
            .collect();

        let mut greedy: ArenaShapeTree<u64> = ArenaShapeTree::new();
        for &(b, p) in &entries {
            greedy.insert(b, p);
        }
        let mut ids = Vec::new();
        let packed = ArenaShapeTree::bulk_load_with_ids(&entries, &mut ids);

        greedy.check_invariants().unwrap();
        packed.check_invariants().unwrap();
        assert_eq!(packed.len(), entries.len());
        assert_eq!(ids.len(), entries.len());
        for (i, &id) in ids.iter().enumerate() {
            assert_eq!(packed.get(id), Some(i as u64), "case {case_no}");
        }

        let queries = [
            OctBounds::from_box(-1000, -1000, 1000, 1000),
            OctBounds::from_box(0, 0, 0, 0),
            OctBounds::from_box(7, 7, 7, 7),
            OctBounds::from_box(100, 0, 120, 1),
            OctBounds::from_box(-5, -5, 5, 5),
        ];
        for q in &queries {
            let mut a = Vec::new();
            let mut b = Vec::new();
            greedy.overlaps(q, &mut a);
            packed.overlaps(q, &mut b);
            let mut a: Vec<u64> = a.into_iter().map(|(_, p)| p).collect();
            let mut b: Vec<u64> = b.into_iter().map(|(_, p)| p).collect();
            a.sort_unstable();
            b.sort_unstable();
            assert_eq!(a, brute(&entries, q), "case {case_no}: greedy != brute");
            assert_eq!(b, a, "case {case_no}: packed != greedy");
        }
    }
}

/// A tree mutated *after* a bulk load must stay consistent — the packed tree
/// is built through a private path that does not go through `insert`, so its
/// parent links and free list have to be usable by the incremental operations.
#[test]
fn bulk_loaded_tree_stays_mutable() {
    let mut rng = Rng::new(0xDEAD_0001_BEEF_0002);
    let entries: Vec<(OctBounds, u64)> = (0..500)
        .map(|i| (wild_bounds(&mut rng), i as u64))
        .collect();
    let mut ids = Vec::new();
    let mut tree = ArenaShapeTree::bulk_load_with_ids(&entries, &mut ids);
    let epoch_after_load = tree.epoch();

    let mut live: Vec<(ArenaLeafId, OctBounds, u64)> = entries
        .iter()
        .enumerate()
        .map(|(i, &(b, p))| (ids[i], b, p))
        .collect();

    for step in 0..1_000u32 {
        if step % 3 == 0 && !live.is_empty() {
            let victim = rng.below(live.len() as u32) as usize;
            let (id, _, p) = live.swap_remove(victim);
            assert_eq!(tree.remove(id), Some(p), "step {step}");
        } else {
            let b = wild_bounds(&mut rng);
            let p = 10_000 + step as u64;
            live.push((tree.insert(b, p), b, p));
        }
        if step % 100 == 0 {
            tree.check_invariants()
                .unwrap_or_else(|e| panic!("step {step}: {e}"));
        }
    }
    tree.check_invariants().unwrap();
    assert!(tree.epoch() > epoch_after_load);

    let mut out = Vec::new();
    let q = OctBounds::from_box(-200, -200, 200, 200);
    tree.overlaps(&q, &mut out);
    let mut actual: Vec<u64> = out.into_iter().map(|(_, p)| p).collect();
    actual.sort_unstable();
    let mirror: Vec<(OctBounds, u64)> = live.iter().map(|&(_, b, p)| (b, p)).collect();
    assert_eq!(actual, brute(&mirror, &q));
}

/// Queries take `&self` and must be usable concurrently from many threads with
/// only a shared reference — the contract the epoch scheme will rest on.
#[test]
fn concurrent_queries_share_one_immutable_tree() {
    let mut rng = Rng::new(0x9999_1111_2222_3333);
    let entries: Vec<(OctBounds, u64)> = (0..4_000)
        .map(|i| (wild_bounds(&mut rng), i as u64))
        .collect();
    let tree = ArenaShapeTree::bulk_load(&entries);
    let epoch = tree.epoch();

    let queries: Vec<OctBounds> = {
        let mut r = Rng::new(0x4444_5555_6666_7777);
        (0..2_000).map(|_| wild_bounds(&mut r)).collect()
    };

    let expected: Vec<Vec<u64>> = queries.iter().map(|q| brute(&entries, q)).collect();

    std::thread::scope(|scope| {
        for _ in 0..4 {
            let tree = &tree;
            let queries = &queries;
            let expected = &expected;
            scope.spawn(move || {
                let mut stack = QueryStack::new();
                let mut out = Vec::new();
                for (i, q) in queries.iter().enumerate() {
                    tree.overlaps_with_stack(q, &mut stack, &mut out);
                    let mut got: Vec<u64> = out.iter().map(|&(_, p)| p).collect();
                    got.sort_unstable();
                    assert_eq!(got, expected[i]);
                }
            });
        }
    });

    assert_eq!(tree.epoch(), epoch, "reads must not bump the epoch");
}
