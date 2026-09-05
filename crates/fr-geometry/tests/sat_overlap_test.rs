//! Property test: `Simplex::has_2dim_overlap` must answer **exactly**
//! `intersection().dimension() == 2` on every pair of tiles — that predicate
//! gates the carve loop's `restrain_shape` (`rooms/complete.rs`), so a single
//! divergence is a room that overlaps copper or a carve that never happens.
//!
//! Pairs are drawn from the three tile flavors the tree actually stores —
//! boxes, octagons, free simplices — with the deck stacked toward the cases
//! the gate exists for: touching edges, shared edges, containment, degenerate
//! (empty / line / point) shapes, and near-misses at 1-unit distances.

use fr_geometry::planar::{IntBox, IntOctagon, IntPoint, Line, Simplex};

/// SplitMix64 — the same pin-down-the-stream trick the router's portfolio
/// shuffle uses; no dependencies, reproducible across machines.
fn splitmix64(seed: u64) -> u64 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = splitmix64(self.0);
        self.0
    }

    /// Uniform in `[-limit, limit]`.
    fn coord(&mut self, limit: i32) -> i32 {
        (self.next() % (2 * limit as u64 + 1)) as i32 - limit
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// A random box, biased toward degenerate and near-tangent forms: coordinates
/// are drawn from a small pool so edges coincide often.
fn random_box(rng: &mut Rng, pool: i32) -> Simplex {
    let mut xs = [rng.coord(pool), rng.coord(pool)];
    xs.sort();
    let mut ys = [rng.coord(pool), rng.coord(pool)];
    ys.sort();
    // One case in eight: collapse a dimension (zero-width or zero-height box,
    // which `to_simplex` turns into a 1-dim or empty simplex).
    if rng.below(8) == 0 {
        xs[1] = xs[0];
    }
    if rng.below(8) == 0 {
        ys[1] = ys[0];
    }
    IntBox::new(xs[0], ys[0], xs[1], ys[1]).to_simplex()
}

/// A random 45° octagon: a random box with a random chamfer, occasionally
/// degenerate (zero-width box, chamfer past the corner).
fn random_octagon(rng: &mut Rng, pool: i32) -> Simplex {
    let mut xs = [rng.coord(pool), rng.coord(pool)];
    xs.sort();
    let mut ys = [rng.coord(pool), rng.coord(pool)];
    ys.sort();
    if rng.below(10) == 0 {
        xs[1] = xs[0];
    }
    if rng.below(10) == 0 {
        ys[1] = ys[0];
    }
    let (lx, rx) = (xs[0], xs[1]);
    let (ly, uy) = (ys[0], ys[1]);
    // Chamfer as drawn can exceed the box side — `to_simplex`'s
    // remove_redundant_lines absorbs the over-cut.
    let c = rng.coord(pool).unsigned_abs() as i32 / 4;
    IntOctagon::new(
        lx,
        ly,
        rx,
        uy,
        lx - uy + c,
        rx - ly - c,
        lx + ly + c,
        rx + uy - c,
    )
    .to_simplex()
}

/// A random free simplex: 3..=6 half-planes with arbitrary (not just 45°)
/// directions, biased to small integer slopes so coincidences happen. The
/// region can be empty, degenerate or unbounded — those are the cases the
/// fallback exists for. Because the *reference* is itself asymmetric on
/// inconsistent regions (its empty-region detection depends on the sort order
/// of parallel lines), pairs involving such a simplex are only checked
/// directionally: `a.has_2dim_overlap(b)` against `a.intersection(b)` in the
/// same order.
fn random_simplex(rng: &mut Rng, pool: i32) -> Simplex {
    let count = 3 + rng.below(4);
    let mut lines = Vec::with_capacity(count);
    for _ in 0..count {
        // A line through two random lattice points; with probability 1/4
        // snapped to horizontal/vertical/diagonal.
        let a = IntPoint::new(rng.coord(pool), rng.coord(pool));
        let mut b = IntPoint::new(rng.coord(pool), rng.coord(pool));
        match rng.below(4) {
            0 => b.y = a.y,
            1 => b.x = a.x,
            2 => b = IntPoint::new(a.x + 1 + rng.below(8) as i32, a.y + 1 + rng.below(8) as i32),
            _ => {}
        }
        if a == b {
            b.x += 1;
        }
        lines.push(Line::new(a, b));
    }
    Simplex::get_instance(&lines)
}

fn random_tile(rng: &mut Rng, pool: i32) -> Simplex {
    match rng.below(3) {
        0 => random_box(rng, pool),
        1 => random_octagon(rng, pool),
        _ => random_simplex(rng, pool),
    }
}

/// One exactly-known pair shape: containment, shared edge, corner touch,
/// 1-unit gap — the cases a float or tolerance-based predicate gets wrong.
fn scripted_pairs() -> Vec<(Simplex, Simplex, &'static str)> {
    let mut out: Vec<(Simplex, Simplex, &'static str)> = Vec::new();
    let base = IntBox::new(0, 0, 100, 100).to_simplex();
    out.push((
        base.clone(),
        IntBox::new(50, 50, 150, 150).to_simplex(),
        "overlap",
    ));
    out.push((
        base.clone(),
        IntBox::new(100, 0, 200, 100).to_simplex(),
        "shared edge",
    ));
    out.push((
        base.clone(),
        IntBox::new(101, 0, 200, 100).to_simplex(),
        "1-gap",
    ));
    out.push((
        base.clone(),
        IntBox::new(100, 100, 200, 200).to_simplex(),
        "corner touch",
    ));
    out.push((
        base.clone(),
        IntBox::new(10, 10, 90, 90).to_simplex(),
        "contained",
    ));
    out.push((base.clone(), base.clone(), "identical"));
    out.push((
        base.clone(),
        IntBox::new(200, 200, 300, 300).to_simplex(),
        "disjoint",
    ));
    // A 45° chamfer exactly resting on the box's corner: the diagonal through
    // (50, 0)-(0, 50) style cuts; the octagon occupies [0,100]^2 with its
    // corners cut at chamfer 50.
    let chamfer = IntOctagon::new(0, 0, 100, 100, -50, 50, 50, 150).to_simplex();
    out.push((
        chamfer.clone(),
        IntBox::new(100, 50, 200, 150).to_simplex(),
        "oct/box edge",
    ));
    out.push((
        chamfer.clone(),
        IntBox::new(99, 99, 200, 200).to_simplex(),
        "oct/box 1-inside",
    ));
    out.push((
        chamfer,
        IntBox::new(100, 100, 200, 200).to_simplex(),
        "oct/box corner touch",
    ));
    // Degenerate pairs: an empty box and a zero-height box against a real one.
    out.push((base.clone(), IntBox::EMPTY.to_simplex(), "empty"));
    out.push((
        base.clone(),
        IntBox::new(50, 50, 60, 50).to_simplex(),
        "flat box inside",
    ));
    out
}

fn check(a: &Simplex, b: &Simplex, label: &str) {
    // Directional equivalence is unconditional: this is the carve loop's
    // question, `candidate.intersection(&obstacle).dimension() == 2`, in this
    // order.
    let expected = a.intersection(b).dimension() == 2;
    let got = a.has_2dim_overlap(b);
    assert_eq!(
        got, expected,
        "{label}: has_2dim_overlap disagrees with intersection().dimension()\n  a: {:?}\n  b: {:?}",
        a.arr, b.arr
    );
    // The reference itself is order-sensitive once an operand's region is
    // unbounded or contradictory: `remove_redundant_lines`' empty-region
    // detection reads the *sorted* line order, and an unbounded region's
    // "bounding box" is not a hull. The fast path never fires for those
    // (its gates reject them, so the answer IS the fallback's), which is why
    // the directional check above still holds. Symmetric reasoning is only
    // claimed for two genuine bounded polygons — the carve loop's case.
    let genuine = |s: &Simplex| s.is_bounded() && s.dimension() == 2;
    if genuine(a) && genuine(b) {
        let got_swapped = b.has_2dim_overlap(a);
        assert_eq!(
            got_swapped, got,
            "{label}: not symmetric on bounded 2-dim polygons\n  a: {:?}\n  b: {:?}",
            a.arr, b.arr
        );
    }
}

#[test]
fn scripted_degenerate_and_tangent_pairs_match() {
    for (a, b, label) in scripted_pairs() {
        check(&a, &b, label);
    }
}

#[test]
fn random_tile_pairs_match_the_exact_construction() {
    let mut rng = Rng(0x5A7);
    let mut overlaps = 0usize;
    let mut small_pool = 0usize;
    let pairs = 120_000;
    for i in 0..pairs {
        // Alternate coordinate pools: the small pool forces constant edge
        // contact; the large pool is the generic case.
        let pool = if i % 3 == 0 { 12 } else { 1 << 16 };
        if pool == 12 {
            small_pool += 1;
        }
        let a = random_tile(&mut rng, pool);
        let b = random_tile(&mut rng, pool);
        if a.intersection(&b).dimension() == 2 {
            overlaps += 1;
        }
        check(&a, &b, "random");
    }
    // Guard against a deck so stacked the property is vacuous: both answers
    // must show up in bulk.
    assert!(
        overlaps > pairs / 20,
        "too few positives: {overlaps}/{pairs}"
    );
    assert!(
        overlaps < pairs * 19 / 20,
        "too few negatives: {overlaps}/{pairs}"
    );
    assert!(small_pool > pairs / 4);
}
