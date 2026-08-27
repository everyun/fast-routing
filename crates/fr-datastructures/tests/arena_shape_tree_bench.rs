//! Timing comparison of [`ArenaShapeTree`] against the [`MinAreaTree`] it is
//! meant to replace.
//!
//! Criterion is deliberately not a workspace dependency, so this is a plain
//! `#[ignore]`d test that prints a table. Run it in release mode:
//!
//! ```text
//! cargo test -p fr-datastructures --release --test arena_shape_tree_bench \
//!     -- --ignored --nocapture
//! ```
//!
//! The workload is the one from the spec: 10 000 random octagons indexed, then
//! 10 000 random octagon queries. The test also asserts that all three trees
//! return the same number of hits, so it doubles as a differential check at a
//! scale the unit tests do not reach.

use std::hint::black_box;
use std::time::{Duration, Instant};

use fr_datastructures::arena_shape_tree::{ArenaShapeTree, OctBounds, QueryStack};
use fr_datastructures::min_area_tree::MinAreaTree;
use fr_datastructures::shape_tree::BoundingShape;

const ITEMS: usize = 10_000;
const QUERIES: usize = 10_000;
/// Half-extent of the coordinate window; sized so a query hits ~15 leaves,
/// the "units to tens" result size the router actually sees (spec §4.2).
const WORLD: i32 = 25_000;
const ITEM_SIZE: i32 = 2_000;
const QUERY_SIZE: i32 = 2_000;

/// xorshift64* — deterministic and dependency-free.
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

    fn range(&mut self, low: i32, high: i32) -> i32 {
        low + ((self.next_u64() >> 32) as u32 % (high - low + 1) as u32) as i32
    }
}

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
    OctBounds::new(
        lx,
        ly,
        rx,
        uy,
        (lx - uy) + cut(rng),
        (rx - ly) - cut(rng),
        (lx + ly) + cut(rng),
        (rx + uy) - cut(rng),
    )
}

/// [`OctBounds`] behind the `MinAreaTree` trait, so both trees index exactly
/// the same geometry with exactly the same predicates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OctShape(OctBounds);

impl BoundingShape for OctShape {
    fn intersects(&self, other: &Self) -> bool {
        self.0.intersects(&other.0)
    }

    fn union(&self, other: &Self) -> Self {
        OctShape(self.0.union(&other.0))
    }

    fn area(&self) -> f64 {
        self.0.area2() as f64 * 0.5
    }

    fn contains(&self, other: &Self) -> bool {
        self.0.contains(&other.0)
    }
}

fn report(label: &str, elapsed: Duration, count: usize) {
    let per_op = elapsed.as_secs_f64() * 1e9 / count as f64;
    println!(
        "  {label:<34} {:>9.2} ms   {per_op:>8.0} ns/op",
        elapsed.as_secs_f64() * 1e3
    );
}

#[test]
#[ignore = "timing benchmark; run with --release -- --ignored --nocapture"]
fn bench_arena_shape_tree_vs_min_area_tree() {
    let mut rng = Rng::new(0xB0A7_1234_5678_9ABD);
    let items: Vec<(OctBounds, u32)> = (0..ITEMS as u32)
        .map(|i| (random_octagon(&mut rng, ITEM_SIZE), i))
        .collect();
    let queries: Vec<OctBounds> = (0..QUERIES)
        .map(|_| random_octagon(&mut rng, QUERY_SIZE))
        .collect();

    println!("\n{ITEMS} octagons, {QUERIES} queries, world +/-{WORLD}\n");
    println!("build:");

    // ---- build -----------------------------------------------------------
    let start = Instant::now();
    let mut min_area: MinAreaTree<OctShape, u32> = MinAreaTree::new();
    for &(bounds, payload) in &items {
        min_area.insert(OctShape(bounds), payload, 0);
    }
    let min_area_build = start.elapsed();
    report("MinAreaTree insert", min_area_build, ITEMS);

    let start = Instant::now();
    let mut greedy: ArenaShapeTree<u32> = ArenaShapeTree::with_leaf_capacity(ITEMS);
    for &(bounds, payload) in &items {
        greedy.insert(bounds, payload);
    }
    let greedy_build = start.elapsed();
    report("ArenaShapeTree insert", greedy_build, ITEMS);

    let start = Instant::now();
    let packed = ArenaShapeTree::bulk_load(&items);
    let packed_build = start.elapsed();
    report("ArenaShapeTree bulk_load (STR)", packed_build, ITEMS);

    // ---- query -----------------------------------------------------------
    println!("\nquery:");

    let start = Instant::now();
    let mut min_area_hits = 0usize;
    for query in &queries {
        let hits = min_area.overlaps(&OctShape(*query));
        min_area_hits += black_box(hits.len());
    }
    let min_area_query = start.elapsed();
    report("MinAreaTree overlaps", min_area_query, QUERIES);

    let mut stack = QueryStack::new();
    let mut out = Vec::new();

    let start = Instant::now();
    let mut greedy_hits = 0usize;
    for query in &queries {
        greedy.overlaps_with_stack(query, &mut stack, &mut out);
        greedy_hits += black_box(out.len());
    }
    let greedy_query = start.elapsed();
    report("ArenaShapeTree overlaps", greedy_query, QUERIES);

    let start = Instant::now();
    let mut packed_hits = 0usize;
    for query in &queries {
        packed.overlaps_with_stack(query, &mut stack, &mut out);
        packed_hits += black_box(out.len());
    }
    let packed_query = start.elapsed();
    report("ArenaShapeTree overlaps (STR)", packed_query, QUERIES);

    // The visitor form: no result vector at all.
    let start = Instant::now();
    let mut visited = 0usize;
    for query in &queries {
        packed.visit_overlaps(query, &mut stack, |_, _| visited += 1);
    }
    let visit_query = start.elapsed();
    report("ArenaShapeTree visit (STR)", visit_query, QUERIES);

    // ---- correctness at scale -------------------------------------------
    assert_eq!(min_area_hits, greedy_hits, "greedy arena tree lost hits");
    assert_eq!(min_area_hits, packed_hits, "packed arena tree lost hits");
    assert_eq!(min_area_hits, visited, "visitor lost hits");

    let greedy_depth = greedy.depth_stats();
    let packed_depth = packed.depth_stats();
    println!(
        "\ntree depth: greedy max {} avg {:.1} | packed max {} avg {:.1}",
        greedy_depth.max_depth,
        greedy_depth.sum_depth as f64 / greedy_depth.leaves as f64,
        packed_depth.max_depth,
        packed_depth.sum_depth as f64 / packed_depth.leaves as f64,
    );
    println!(
        "hits: {min_area_hits} total, {:.1} per query\n",
        min_area_hits as f64 / QUERIES as f64
    );
    println!(
        "speedup vs MinAreaTree: build {:.2}x (greedy) {:.2}x (STR), \
         query {:.2}x (greedy) {:.2}x (STR)\n",
        min_area_build.as_secs_f64() / greedy_build.as_secs_f64(),
        min_area_build.as_secs_f64() / packed_build.as_secs_f64(),
        min_area_query.as_secs_f64() / greedy_query.as_secs_f64(),
        min_area_query.as_secs_f64() / packed_query.as_secs_f64(),
    );
}

/// Removal throughput, the other half of the router's tree traffic (every
/// routed connection rips and reinserts traces).
#[test]
#[ignore = "timing benchmark; run with --release -- --ignored --nocapture"]
fn bench_arena_shape_tree_churn() {
    let mut rng = Rng::new(0xB0A7_1234_5678_9ABF);
    let items: Vec<(OctBounds, u32)> = (0..ITEMS as u32)
        .map(|i| (random_octagon(&mut rng, ITEM_SIZE), i))
        .collect();

    let mut min_area: MinAreaTree<OctShape, u32> = MinAreaTree::new();
    let min_area_ids: Vec<_> = items
        .iter()
        .map(|&(bounds, payload)| min_area.insert(OctShape(bounds), payload, 0))
        .collect();
    let start = Instant::now();
    for id in &min_area_ids {
        black_box(min_area.remove(*id));
    }
    let min_area_remove = start.elapsed();

    let mut ids = Vec::new();
    let mut arena = ArenaShapeTree::bulk_load_with_ids(&items, &mut ids);
    let start = Instant::now();
    for id in &ids {
        black_box(arena.remove(*id));
    }
    let arena_remove = start.elapsed();

    println!("\nremove {ITEMS} leaves:");
    report("MinAreaTree remove", min_area_remove, ITEMS);
    report("ArenaShapeTree remove (STR)", arena_remove, ITEMS);
    println!(
        "speedup vs MinAreaTree: {:.2}x\n",
        min_area_remove.as_secs_f64() / arena_remove.as_secs_f64()
    );

    assert!(min_area.is_empty());
    assert!(arena.is_empty());
    arena.check_invariants().unwrap();
}
