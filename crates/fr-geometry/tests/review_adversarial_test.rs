//! Independent adversarial probes written during review of the M1 wave-1
//! geometry work. These are deliberately *not* derived from the implementing
//! agents' test files: each one attacks a claim rather than restating it.

use fr_geometry::planar::area::Area;
use fr_geometry::planar::direction::Direction;
use fr_geometry::planar::float_point::FloatPoint;
use fr_geometry::planar::int_octagon::IntOctagon;
use fr_geometry::planar::int_point::IntPoint;
use fr_geometry::planar::limits::Limits;
use fr_geometry::planar::line::Line;
use fr_geometry::planar::point::Point;
use fr_geometry::planar::polyline::Polyline;
use fr_geometry::planar::rational_point::RationalPoint;
use fr_geometry::planar::shape::Shape;
use fr_geometry::planar::side::Side;
use num_bigint::BigInt;

/// SplitMix64, so the probes are deterministic without a dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed)
    }
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in `[-bound, bound]`.
    fn signed(&mut self, bound: i32) -> i32 {
        let span = 2 * bound as i64 + 1;
        ((self.next_u64() >> 1) as i64 % span - bound as i64) as i32
    }
    fn nonzero(&mut self, bound: i32) -> i32 {
        loop {
            let v = self.signed(bound);
            if v != 0 {
                return v;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// exact-arith: cross-multiplication comparison
// ---------------------------------------------------------------------------

/// Hand-computed mixed comparisons, including negative numerators and the
/// denominators that put the cross product right at the top of the `i128` fast
/// path. `RationalPoint` normalises `z >= 0`, so the "negative denominator"
/// case has to be expressed as a negated numerator — both are checked.
#[test]
fn mixed_comparison_hand_checked_cases() {
    let c = Limits::CRIT_INT; // 2^25

    // -3/2 = -1.5 vs the lattice points around it.
    let p = Point::Rational(RationalPoint::new(
        BigInt::from(-3),
        BigInt::from(-3),
        BigInt::from(2),
    ));
    assert_eq!(p.compare_x(&Point::get_instance(-1, 0)), -1);
    assert_eq!(p.compare_x(&Point::get_instance(-2, 0)), 1);
    assert_eq!(p.compare_y(&Point::get_instance(-2, -1)), -1);
    // and the reverse direction, which routes through the `-b.compare_*_int(a)`
    // arm of `Point::compare_*`.
    assert_eq!(Point::get_instance(-1, 0).compare_x(&p), 1);
    assert_eq!(Point::get_instance(-2, 0).compare_x(&p), -1);

    // The same affine point written with a huge denominator: the comparison
    // must not depend on the representative.
    let scaled = Point::Rational(RationalPoint::new(
        BigInt::from(-3i64) * BigInt::from(1_000_000_007i64),
        BigInt::from(-3i64) * BigInt::from(1_000_000_007i64),
        BigInt::from(2i64) * BigInt::from(1_000_000_007i64),
    ));
    assert_eq!(scaled.compare_x(&Point::get_instance(-1, 0)), -1);
    assert_eq!(scaled.compare_x(&Point::get_instance(-2, 0)), 1);
    assert_eq!(scaled.compare_x(&p), 0);
    assert_eq!(p.compare_x(&scaled), 0);

    // Just below / just above the envelope corner: (c*z - 1)/z < c < (c*z+1)/z.
    let z = BigInt::from(1_000_003i64);
    let just_below = Point::Rational(RationalPoint::new(
        BigInt::from(c) * &z - 1,
        -(BigInt::from(c) * &z) - 1,
        z.clone(),
    ));
    assert_eq!(just_below.compare_x(&Point::get_instance(c, -c)), -1);
    assert_eq!(just_below.compare_y(&Point::get_instance(c, -c)), -1);
    assert_eq!(Point::get_instance(c, -c).compare_x(&just_below), 1);
}

/// Randomised oracle for the mixed and rational comparators, drawing operands
/// straight out of the `i128` fast path *and* out of it, so both tiers are
/// exercised against independent `BigInt` arithmetic.
#[test]
fn comparison_matches_a_bigint_oracle_across_both_tiers() {
    let mut rng = Rng::new(0xA5A5_1234_DEAD_BEEF);
    let c = Limits::CRIT_INT;

    for round in 0..4000u32 {
        // Every fourth round uses operands that overflow the i128 fast path.
        let huge = round % 4 == 3;
        let mk = |rng: &mut Rng| -> (BigInt, BigInt, BigInt) {
            let x = BigInt::from(rng.signed(c));
            let y = BigInt::from(rng.signed(c));
            let z = BigInt::from(rng.nonzero(c).unsigned_abs());
            if huge {
                let blow = BigInt::from(1u8) << 100;
                (x * &blow, y * &blow, z * &blow)
            } else {
                (x, y, z)
            }
        };
        let (ax, ay, az) = mk(&mut rng);
        let (bx, by, bz) = mk(&mut rng);
        let a = RationalPoint::new(ax.clone(), ay.clone(), az.clone());
        let b = RationalPoint::new(bx.clone(), by.clone(), bz.clone());

        // Oracle: cross multiplication with positive denominators.
        let expect_x = (&ax * &bz).cmp(&(&bx * &az)) as i32;
        let expect_y = (&ay * &bz).cmp(&(&by * &az)) as i32;
        assert_eq!(a.compare_x_rational(&b), expect_x, "round {round}");
        assert_eq!(a.compare_y_rational(&b), expect_y, "round {round}");
        assert_eq!(b.compare_x_rational(&a), -expect_x, "round {round}");

        let ip = IntPoint::new(rng.signed(c), rng.signed(c));
        let expect_ix = ax.cmp(&(&az * ip.x)) as i32;
        let expect_iy = ay.cmp(&(&az * ip.y)) as i32;
        assert_eq!(a.compare_x_int(&ip), expect_ix, "round {round}");
        assert_eq!(a.compare_y_int(&ip), expect_iy, "round {round}");

        // Point-level dispatch must agree, in both directions.
        let pa = Point::Rational(a.clone());
        let pi = Point::Int(ip);
        assert_eq!(pa.compare_x(&pi), expect_ix, "round {round}");
        assert_eq!(pi.compare_x(&pa), -expect_ix, "round {round}");
    }
}

// ---------------------------------------------------------------------------
// exact-arith: `Ord for Line` must be a real total order
// ---------------------------------------------------------------------------

/// `cmp_line` backs `Ord for Line`, which the router uses to sort border lines.
/// Antisymmetry alone is not enough — an inconsistent comparator corrupts
/// `sort_unstable` silently. This checks totality *and* transitivity over
/// random directions, including near the envelope where the tie-breaking
/// determinant used to be evaluated in `f64`.
#[test]
fn cmp_line_is_a_total_order() {
    let mut rng = Rng::new(0x0BAD_C0DE_1234_5678);
    let c = Limits::CRIT_INT;
    let mut lines = Vec::new();
    for _ in 0..40 {
        let a = IntPoint::new(rng.signed(c), rng.signed(c));
        let b = IntPoint::new(rng.signed(c), rng.signed(c));
        if a != b {
            lines.push(Line::new(a, b));
        }
    }
    // Plus the axis and diagonal directions, which take the special branches.
    for (dx, dy) in [
        (1, 0),
        (-1, 0),
        (0, 1),
        (0, -1),
        (1, 1),
        (-1, -1),
        (1, -1),
        (-1, 1),
    ] {
        lines.push(Line::new(
            IntPoint::new(0, 0),
            IntPoint::new(dx * c, dy * c),
        ));
    }

    for x in &lines {
        assert_eq!(x.cmp_line(x), 0, "not reflexive: {x:?}");
        for y in &lines {
            assert_eq!(
                x.cmp_line(y).signum(),
                -y.cmp_line(x).signum(),
                "not antisymmetric: {x:?} {y:?}"
            );
            for z in &lines {
                if x.cmp_line(y) <= 0 && y.cmp_line(z) <= 0 {
                    assert!(
                        x.cmp_line(z) <= 0,
                        "not transitive: {x:?} <= {y:?} <= {z:?} but not <= the last"
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// exact-arith: side_of_intersection against an independent BigInt oracle
// ---------------------------------------------------------------------------

/// The exact `i128` path is compared against a wholly independent `BigInt`
/// evaluation of the same predicate, on random envelope-scale input. This is
/// the hot predicate (`simplex.rs` cutting), so a sign slip here is a topology
/// change.
#[test]
fn side_of_intersection_matches_an_independent_bigint_oracle() {
    let mut rng = Rng::new(0xFEED_FACE_0000_0001);
    let c = Limits::CRIT_INT;
    let mut decided = 0u32;

    for round in 0..3000u32 {
        let mk = |rng: &mut Rng| {
            let a = IntPoint::new(rng.signed(c), rng.signed(c));
            let mut b = IntPoint::new(rng.signed(c), rng.signed(c));
            if a == b {
                b = IntPoint::new(a.x.wrapping_add(1), a.y);
            }
            Line::new(a, b)
        };
        let l = mk(&mut rng);
        let p1 = mk(&mut rng);
        let p2 = mk(&mut rng);

        // Oracle: the exact intersection point, then the exact side of it.
        let is = p1.intersection(&p2);
        if is.is_infinite() {
            continue;
        }
        let expect = l.side_of_point(&is);
        assert_eq!(
            l.side_of_intersection(&p1, &p2),
            expect,
            "round {round}: {l:?} {p1:?} {p2:?} -> {is:?}"
        );
        if expect != Side::Collinear {
            decided += 1;
        }
    }
    assert!(decided > 100, "probe never produced a decided side");
}

// ---------------------------------------------------------------------------
// geom-robustness: octagon containment vs. the helpers left box-only
// ---------------------------------------------------------------------------

/// `contains_point` / `contains_float_point` now enforce all 8 half-planes,
/// but `nearest_point_approx`, `corner_approx_arr` and (through the first)
/// `Area::distance` were left as *bounding-box* computations. They therefore
/// hand back points that the very same octagon now reports as outside — an
/// inconsistency introduced by fixing only the containment half.
///
/// This is a documentation test for a known gap, not a passing-is-good test:
/// if it starts failing, the box-only helpers were fixed and it should be
/// deleted.
#[test]
fn octagon_nearest_point_and_corners_ignore_the_diagonal_constraints() {
    // A band clipped hard on both 45 degree diagonals: -10 <= x-y <= 10.
    let oct = IntOctagon::new(0, 0, 100, 100, -10, 10, 0, 200);
    assert!(!oct.is_empty());
    assert_eq!(oct.normalize(), oct, "fixture is already normalized");

    // `nearest_point_approx` clamps to the bounding box, so it lands in a
    // clipped corner that `contains_float_point` rejects.
    let nearest = oct.nearest_point_approx(&FloatPoint::new(200.0, -50.0));
    assert_eq!(nearest, FloatPoint::new(100.0, 0.0));
    assert!(
        !oct.contains_float_point(&nearest),
        "nearest_point_approx now honours the diagonals; delete this probe"
    );
    // ... and `Area::distance`, which is defined as the distance to that
    // point, is therefore an under-estimate: the true nearest point of the
    // octagon to (100, 0) is (100, 90), 90 units away, not 0.
    assert_eq!(oct.distance(&FloatPoint::new(100.0, 0.0)), 0.0);

    // `corner_approx_arr` returns the four *box* corners; two of them are
    // outside the octagon.
    let corners = oct.corner_approx_arr();
    assert_eq!(corners.len(), 4);
    let outside = corners
        .iter()
        .filter(|c| !oct.contains_float_point(c))
        .count();
    assert_eq!(
        outside, 2,
        "corner_approx_arr now returns real octagon corners; delete this probe"
    );
}

// ---------------------------------------------------------------------------
// geom-robustness: the whole Polyline API on a degenerate polyline
// ---------------------------------------------------------------------------

/// Four underflow sites were patched by name. This sweeps the *rest* of the
/// public surface on the same degenerate polyline, so a fifth panic cannot
/// hide behind the four regression tests.
#[test]
fn degenerate_polyline_survives_the_whole_public_api() {
    let p = Point::get_instance(42, -17);
    let degenerate = Polyline::new_from_two_points(&p, &p);
    assert_eq!(degenerate.corner_count(), 0);

    // Read-only accessors.
    assert!(degenerate.is_empty());
    let _ = degenerate.is_point();
    let _ = degenerate.first_corner();
    let _ = degenerate.last_corner();
    let _ = degenerate.corner(0);
    let _ = degenerate.corner(7);
    let _ = degenerate.corner_approx(0);
    assert!(degenerate.corner_arr().is_empty());
    assert!(degenerate.corner_approx_arr().is_empty());
    let _ = degenerate.length_total();
    let _ = degenerate.length(0, 0);

    // Shape derivation.
    assert!(degenerate.offset_shapes(50).is_empty());
    assert!(degenerate.offset_shapes_range(50, 0, 0).is_empty());
    assert!(degenerate.offset_shapes_range(50, 0, 9).is_empty());
    assert!(degenerate.offset_shape(50, 0).is_none());
    let _ = degenerate.bounding_box();
    let _ = degenerate.bounding_octagon();
    let _ = degenerate.bounding_box_range(0, 0);
    let _ = degenerate.bounding_octagon_range(0, 0);

    // Transforms.
    let _ = degenerate.translate_by(&fr_geometry::planar::vector::Vector::get_instance(5, 5));
    let _ = degenerate.turn_90_degree(1, &IntPoint::new(0, 0));
    let _ = degenerate.mirror_vertical(&IntPoint::new(0, 0));
    let _ = degenerate.mirror_horizontal(&IntPoint::new(0, 0));
    let _ = degenerate.rotate_approx(0.5, &FloatPoint::new(0.0, 0.0));

    // Queries.
    assert!(!degenerate.contains(&p));
    assert!(degenerate.projection_line(&p).is_none());
    assert!(degenerate
        .nearest_point_approx(&FloatPoint::new(0.0, 0.0))
        .is_none());
    let _ = degenerate.distance(&FloatPoint::new(0.0, 0.0));

    // Combination with a real polyline, in both directions.
    let real =
        Polyline::new_from_two_points(&Point::get_instance(0, 0), &Point::get_instance(500, 0));
    let _ = degenerate.combine(&real);
    let _ = real.combine(&degenerate);
    let _ = degenerate.skip_lines(0, 0);
    let _ = degenerate.split(0, &Line::new(IntPoint::new(0, -1), IntPoint::new(0, 1)));
    let _ = degenerate.shorten(0, 1.0);
    let _ = degenerate.shorten(3, 1.0);
}

// ---------------------------------------------------------------------------
// exact-arith: LineSegment::contains rewrite, checked through Polyline
// ---------------------------------------------------------------------------

/// `LineSegment::contains` was rewritten from "perpendicular line through the
/// rounded point" to an exact between-test. Its main consumer is
/// `Polyline::contains`, so this checks the *composed* behaviour: every corner
/// of a polyline, including the rational ones produced by arbitrary-angle line
/// crossings, must be reported as on the polyline, and points just beyond an
/// end must not be.
#[test]
fn polyline_contains_every_one_of_its_own_corners() {
    let mut rng = Rng::new(0x1357_9BDF_2468_ACE0);
    let mut rational_corners = 0u32;
    let mut checked = 0u32;

    for round in 0..2000u32 {
        // Built from arbitrary-angle *lines*, so the corners are the exact
        // rational crossings rather than the integer points fed in.
        let lines: Vec<Line> = (0..5)
            .map(|_| {
                let a = IntPoint::new(rng.signed(20_000), rng.signed(20_000));
                let mut b = IntPoint::new(rng.signed(20_000), rng.signed(20_000));
                if a == b {
                    b = IntPoint::new(a.x.wrapping_add(1), a.y);
                }
                Line::new(a, b)
            })
            .collect();
        let polyline = Polyline::new_from_lines(&lines);
        if polyline.corner_count() < 2 {
            continue;
        }
        for i in 0..polyline.corner_count() {
            let corner = polyline.corner(i);
            if corner.is_infinite() {
                continue;
            }
            if matches!(corner, Point::Rational(_)) {
                rational_corners += 1;
            }
            checked += 1;
            assert!(
                polyline.contains(&corner),
                "round {round}: corner {i} = {corner:?} not on its own polyline"
            );
        }
    }
    assert!(checked > 100, "probe checked too few corners");
    assert!(
        rational_corners > 0,
        "probe never produced a rational corner; it would not exercise the \
         rewritten exact path"
    );
}

/// The exact between-test must still reject collinear points beyond an end.
#[test]
fn line_segment_contains_rejects_points_beyond_the_ends() {
    let start = Line::new(IntPoint::new(0, -10), IntPoint::new(0, 10));
    let middle = Line::new(IntPoint::new(0, 0), IntPoint::new(100, 0));
    let end = Line::new(IntPoint::new(100, -10), IntPoint::new(100, 10));
    let seg = fr_geometry::planar::line_segment::LineSegment::new(start, middle, end);

    assert!(seg.contains(&Point::get_instance(0, 0)));
    assert!(seg.contains(&Point::get_instance(50, 0)));
    assert!(seg.contains(&Point::get_instance(100, 0)));
    assert!(!seg.contains(&Point::get_instance(-1, 0)));
    assert!(!seg.contains(&Point::get_instance(101, 0)));
    assert!(!seg.contains(&Point::get_instance(50, 1)));

    // A rational point half a unit past the end must be rejected, which is
    // exactly the case the old rounding-to-lattice implementation got wrong.
    let past_end = Point::Rational(RationalPoint::new(
        BigInt::from(201),
        BigInt::from(0),
        BigInt::from(2),
    ));
    assert!(!seg.contains(&past_end));
    let inside = Point::Rational(RationalPoint::new(
        BigInt::from(199),
        BigInt::from(0),
        BigInt::from(2),
    ));
    assert!(seg.contains(&inside));
}

// ---------------------------------------------------------------------------
// exact-arith: the CRIT_INT envelope contract
// ---------------------------------------------------------------------------

/// `Direction`-derived lines are the ones the router builds most; check that a
/// `Line` built from a point at the envelope corner and a 45° direction still
/// answers `side_of` exactly, and that `try_new` rejects the same point one
/// unit further out.
#[test]
fn envelope_corner_lines_stay_exact() {
    let c = Limits::CRIT_INT;
    let corner = IntPoint::new(c, c);
    assert!(corner.is_in_envelope());
    assert!(IntPoint::try_new(c + 1, c).is_none());
    assert!(Line::try_new_from_coords(c, c, c + 1, c).is_none());

    let line = Line::new_from_point_and_dir(IntPoint::new(-c, -c), &Direction::RIGHT45);
    assert_eq!(line.side_of(&corner), Side::Collinear);
    assert_eq!(line.side_of(&IntPoint::new(c, c - 1)), Side::OnTheLeft);
    assert_eq!(line.side_of(&IntPoint::new(c - 1, c)), Side::OnTheRight);
}
