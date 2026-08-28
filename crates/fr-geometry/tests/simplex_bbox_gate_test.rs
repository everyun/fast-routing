//! The bounding-box gates in front of [`Simplex::intersects_box`] and
//! [`Simplex::intersects_octagon`] answer exactly what the exact test answers.
//!
//! Those gates are a pure optimisation: they short-circuit
//! `Simplex::intersection`, which is three heap allocations and an `i128`
//! `side_of_intersection` per line. An optimisation is only allowed here if it
//! is *exact*, because the answer decides whether a shape is an obstacle —
//! `f64` must never decide topology. The gate's soundness argument leans on two
//! things that this file checks by construction rather than by inspection:
//!
//! 1. the corner approximations behind [`Simplex::bounding_box`] round outward,
//!    so the box is a true hull and neither gate can shrink the shape;
//! 2. `Simplex::is_empty()` is `arr.is_empty()`, so the containment arm may
//!    answer "yes" from the line count alone.
//!
//! (2) holds for **normalized** simplices — anything out of
//! [`Simplex::get_instance`], [`Simplex::from_convex_polygon`],
//! [`Simplex::intersection`] or a rigid motion of one, which is everything the
//! board and the router store. It does *not* hold for a raw
//! [`Simplex::new`], which keeps whatever lines it is handed: three half-planes
//! can all turn left and still have an empty intersection, and there the gate
//! and the exact test disagree by construction. That is the gate's
//! precondition, and `left_turning_half_planes_are_normalized_away` pins the
//! normalization that discharges it.

use fr_geometry::planar::{IntBox, IntOctagon, IntPoint, Line, Simplex};

/// xorshift64*, so the storm is reproducible and needs no dev-dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn in_range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + (self.next() % ((hi - lo + 1) as u64)) as i32
    }
}

fn random_line(rng: &mut Rng, span: i32) -> Line {
    loop {
        let a = IntPoint::new(rng.in_range(-span, span), rng.in_range(-span, span));
        let b = IntPoint::new(rng.in_range(-span, span), rng.in_range(-span, span));
        if a != b {
            return Line::new(a, b);
        }
    }
}

/// The pre-optimisation path: build the intersection and ask if it is empty.
fn exact_box(shape: &Simplex, box_: &IntBox) -> bool {
    shape.intersects(&box_.to_simplex())
}

fn exact_octagon(shape: &Simplex, oct: &IntOctagon) -> bool {
    shape.intersects(&oct.to_simplex())
}

/// 40k random normalized simplices against random boxes, over four coordinate
/// spans so the `f64` corner approximation is exercised where it is exact
/// (small boards) and where products leave the 53-bit mantissa (5e6, the scale
/// a real `.dsn` reaches after the importer's unit conversion).
#[test]
fn the_box_gate_agrees_with_the_exact_test_on_a_storm() {
    let mut rng = Rng(0x1234_5678_9ABC_DEF0);
    for _ in 0..40_000 {
        let span = [50, 500, 50_000, 5_000_000][(rng.next() % 4) as usize];
        let count = 3 + (rng.next() % 6) as usize;
        let lines: Vec<Line> = (0..count).map(|_| random_line(&mut rng, span)).collect();
        let shape = Simplex::get_instance(&lines);
        let (x0, x1) = (rng.in_range(-span, span), rng.in_range(-span, span));
        let (y0, y1) = (rng.in_range(-span, span), rng.in_range(-span, span));
        let box_ = IntBox::new(x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1));
        assert_eq!(
            shape.intersects_box(&box_),
            exact_box(&shape, &box_),
            "the gate diverged from the exact test: shape={shape:?} box={box_:?}"
        );
    }
}

/// The same storm against the octagon overload, whose containment arm is
/// `octagon_contains_box` rather than `IntBox::contains_box`.
#[test]
fn the_octagon_gate_agrees_with_the_exact_test_on_a_storm() {
    let mut rng = Rng(0x0BAD_C0DE_DEAD_BEEF);
    for _ in 0..30_000 {
        let span = [50, 500, 50_000, 1_000_000][(rng.next() % 4) as usize];
        let count = 3 + (rng.next() % 6) as usize;
        let lines: Vec<Line> = (0..count).map(|_| random_line(&mut rng, span)).collect();
        let shape = Simplex::get_instance(&lines);
        let (x0, x1) = (rng.in_range(-span, span), rng.in_range(-span, span));
        let (y0, y1) = (rng.in_range(-span, span), rng.in_range(-span, span));
        let (lx, rx) = (x0.min(x1), x0.max(x1));
        let (by, ty) = (y0.min(y1), y0.max(y1));
        // A slack of 0 makes the octagon a diamond-clipped box; a large slack
        // makes it the box itself, which is the shape the tree stores most.
        let slack = rng.in_range(0, span);
        let oct = IntOctagon::new(
            lx,
            by,
            rx,
            ty,
            lx - ty - slack,
            rx - by + slack,
            lx + by - slack,
            rx + ty + slack,
        )
        .normalize();
        assert_eq!(
            shape.intersects_octagon(&oct),
            exact_octagon(&shape, &oct),
            "the gate diverged from the exact test: shape={shape:?} oct={oct:?}"
        );
    }
}

/// The gate's precondition, made concrete.
///
/// Three half-planes tangent to a circle with their *outsides* kept turn left
/// at every step, so the boundedness test in `bounded_bounding_box` passes and
/// a bounding box is produced — but their intersection is empty. Held as a raw
/// [`Simplex::new`] the containment arm would answer "intersects" for a shape
/// that is not there. [`Simplex::get_instance`] is what stands between the
/// router and that case: it must reduce this to the empty simplex, and then
/// the `arr.is_empty()` guard at the top of both gates fires first.
#[test]
fn left_turning_half_planes_are_normalized_away() {
    for d in [1i32, 5, 100, 10_000] {
        let reach = 1_000_000i32;
        // `Line(a, b)` keeps the points on its left. Put `a` on the circle of
        // radius `d` and aim `b` along the clockwise tangent, so "left" is the
        // half-plane away from the origin.
        let tangent = |theta: f64| -> Line {
            let (sin, cos) = theta.sin_cos();
            let px = (cos * d as f64).round() as i32;
            let py = (sin * d as f64).round() as i32;
            let dx = (sin * reach as f64).round() as i32;
            let dy = (-cos * reach as f64).round() as i32;
            Line::new(IntPoint::new(px, py), IntPoint::new(px + dx, py + dy))
        };
        let tau = std::f64::consts::TAU;
        let raw = [tangent(0.0), tangent(tau / 3.0), tangent(2.0 * tau / 3.0)];

        let shape = Simplex::get_instance(&raw);
        for size in [0, 1, d, 4 * d, 1000 * d.max(1)] {
            let box_ = IntBox::new(-size, -size, size, size);
            assert_eq!(
                shape.intersects_box(&box_),
                exact_box(&shape, &box_),
                "d={d} size={size}: the normalized shape must agree with the exact test"
            );
        }
    }
}
