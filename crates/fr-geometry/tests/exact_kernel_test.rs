//! Exactness of the planar kernel's decision predicates.
//!
//! Every test here pins down a *topological* decision — orientation, ordering,
//! containment, coordinate comparison — against exact arithmetic (`BigInt`
//! oracles written independently of the kernel) rather than against a float.
//! The properties are exercised at the corners of the `CRIT_INT` coordinate
//! envelope, where the `i64` products reach 2^53 and the `i128` tier takes
//! over, and beyond it, where the old `f64` predicates provably lost the sign.

use num_bigint::{BigInt, Sign as BigSign};

use fr_datastructures::Signum;
use fr_geometry::planar::{
    int_point::IntPoint,
    int_vector::IntVector,
    limits::{determinant_exact, scalar_product_exact, side_of_exact, signum_of_exact, Limits},
    line::Line,
    line_segment::LineSegment,
    point::Point,
    rational_point::RationalPoint,
    side::Side,
    vector::Vector,
};

const CRIT: i32 = Limits::CRIT_INT;

/// SplitMix64 — deterministic, so a failure is always reproducible and the
/// suite never depends on an external rng crate.
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

    fn below(&mut self, bound: u64) -> u64 {
        self.next_u64() % bound
    }

    /// A coordinate biased towards the envelope corners: three quarters of the
    /// draws sit next to `+-CRIT_INT`, next to 0 or next to `+-CRIT_INT / 2`.
    fn envelope_coord(&mut self) -> i32 {
        let sign = if self.next_u64() & 1 == 0 { 1 } else { -1 };
        match self.below(4) {
            0 => sign * (CRIT - self.below(4) as i32),
            1 => sign * self.below(4) as i32,
            2 => sign * (CRIT / 2 + self.below(8) as i32),
            _ => sign * self.below(CRIT as u64 + 1) as i32,
        }
    }

    fn envelope_point(&mut self) -> IntPoint {
        IntPoint::new(self.envelope_coord(), self.envelope_coord())
    }

    fn envelope_line(&mut self) -> Line {
        loop {
            let a = self.envelope_point();
            let b = self.envelope_point();
            if a != b {
                return Line::new(a, b);
            }
        }
    }
}

fn big(value: i32) -> BigInt {
    BigInt::from(value)
}

fn as_triple(point: &Point) -> (BigInt, BigInt, BigInt) {
    match point {
        Point::Int(p) => (big(p.x), big(p.y), BigInt::from(1)),
        Point::Rational(r) => (r.x.clone(), r.y.clone(), r.z.clone()),
    }
}

/// Exact `BigInt` oracle for `Line::side_of_point`, written from the
/// definition: the sign of `det(b - a, p - a)`, with the (non-negative)
/// homogeneous denominator cleared, negated as `Line::side_of` negates
/// `Point::side_of`.
fn oracle_side_of_line(line: &Line, x: &BigInt, y: &BigInt, z: &BigInt) -> Side {
    let vx = big(line.b.x) - big(line.a.x);
    let vy = big(line.b.y) - big(line.a.y);
    let rel_x = x - big(line.a.x) * z;
    let rel_y = y - big(line.a.y) * z;
    match (vx * rel_y - vy * rel_x).sign() {
        BigSign::Plus => Side::OnTheRight,
        BigSign::Minus => Side::OnTheLeft,
        BigSign::NoSign => Side::Collinear,
    }
}

/// Exact `BigInt` oracle for "`p` lies on the segment from `s` to `e`":
/// collinear with the middle line, and beyond neither end. All denominators
/// are positive, so clearing them preserves both scalar-product signs.
fn oracle_contains(middle: &Line, s: &Point, e: &Point, p: &Point) -> bool {
    let (px, py, pz) = as_triple(p);
    if oracle_side_of_line(middle, &px, &py, &pz) != Side::Collinear {
        return false;
    }
    let (sx, sy, sz) = as_triple(s);
    let (ex, ey, ez) = as_triple(e);
    // p - s, e - s, p - e and s - e with their positive denominators dropped.
    let ps_x = &px * &sz - &sx * &pz;
    let ps_y = &py * &sz - &sy * &pz;
    let es_x = &ex * &sz - &sx * &ez;
    let es_y = &ey * &sz - &sy * &ez;
    let pe_x = &px * &ez - &ex * &pz;
    let pe_y = &py * &ez - &ey * &pz;
    let se_x = &sx * &ez - &ex * &sz;
    let se_y = &sy * &ez - &ey * &sz;
    let t = &ps_x * &es_x + &ps_y * &es_y;
    let u = &pe_x * &se_x + &pe_y * &se_y;
    t.sign() != BigSign::Minus && u.sign() != BigSign::Minus
}

// ---------------------------------------------------------------- orientation

/// `side_of` must be antisymmetric: reversing the reference direction flips the
/// answer, and `Collinear` is its own opposite. A float determinant breaks this
/// as soon as the two orderings of the products round differently.
#[test]
fn side_of_is_antisymmetric_over_the_whole_envelope() {
    let mut rng = Rng::new(0xC0FF_EE01);
    for _ in 0..20_000 {
        let p = Point::Int(rng.envelope_point());
        let a = Point::Int(rng.envelope_point());
        let b = Point::Int(rng.envelope_point());
        assert_eq!(
            p.side_of_points(&a, &b),
            p.side_of_points(&b, &a).negate(),
            "p={p:?} a={a:?} b={b:?}"
        );
    }
}

/// The vector-level predicates must agree with the exact determinant and the
/// exact scalar product, and stay antisymmetric.
#[test]
fn vector_predicates_agree_with_exact_arithmetic() {
    let mut rng = Rng::new(0x5EED_0002);
    for _ in 0..20_000 {
        let a = IntVector::new(rng.envelope_coord(), rng.envelope_coord());
        let b = IntVector::new(rng.envelope_coord(), rng.envelope_coord());
        let expected = side_of_exact(determinant_exact(
            b.x as i64, b.y as i64, a.x as i64, a.y as i64,
        ));
        let actual = Vector::Int(a).side_of(&Vector::Int(b));
        assert_eq!(actual, expected, "a={a:?} b={b:?}");
        assert_eq!(actual, a.side_of_int(&b));
        assert_eq!(actual, b.side_of_int(&a).negate(), "a={a:?} b={b:?}");
        assert_eq!(
            Vector::Int(a).projection(&Vector::Int(b)),
            signum_of_exact(scalar_product_exact(
                a.x as i64, a.y as i64, b.x as i64, b.y as i64
            ))
        );
    }
}

/// The constructors are unchecked, exactly as upstream's are, so the predicates
/// have to stay exact outside the coordinate envelope as well. Around 2^30 the
/// determinant terms reach 2^60, where an `f64` has an ulp of 256 and the old
/// float predicate reported `Collinear` for a triangle of doubled area 1.
#[test]
fn side_of_stays_exact_beyond_the_double_mantissa() {
    let n = 1i32 << 30;
    let a = Point::Int(IntPoint::new(0, 0));
    let b = Point::Int(IntPoint::new(n + 1, n));
    let p = Point::Int(IntPoint::new(n + 2, n + 1));
    // det((n + 1, n), (n + 2, n + 1)) = (n + 1)^2 - n * (n + 2) = 1.
    assert_eq!(p.side_of_points(&a, &b), Side::OnTheLeft);
    assert_eq!(p.side_of_points(&b, &a), Side::OnTheRight);
    let float_det = ((n + 1) as f64) * ((n + 1) as f64) - (n as f64) * ((n + 2) as f64);
    assert_eq!(float_det, 0.0);

    // The same property over a random population. The coordinates stay in
    // `[0, 2^30]`, so their `i32` differences cannot overflow while the
    // determinant terms still reach 2^60.
    let mut rng = Rng::new(0x5EED_000A);
    fn far_coord(rng: &mut Rng, n: i32) -> i32 {
        match rng.below(3) {
            0 => n - rng.below(4) as i32,
            1 => rng.below(4) as i32,
            _ => rng.below(n as u64 + 1) as i32,
        }
    }
    for _ in 0..10_000 {
        let far = |rng: &mut Rng| IntPoint::new(far_coord(rng, n), far_coord(rng, n));
        let (pa, pb, pp) = (far(&mut rng), far(&mut rng), far(&mut rng));
        if pa == pb {
            continue;
        }
        let line = Line::new(pa, pb);
        let expected =
            oracle_side_of_line(&line, &big(pp.x), &big(pp.y), &BigInt::from(1)).negate();
        assert_eq!(
            Point::Int(pp).side_of_points(&Point::Int(pa), &Point::Int(pb)),
            expected,
            "a={pa:?} b={pb:?} p={pp:?}"
        );
    }
}

/// A line's side of a point must invert when the line is reversed, and must
/// match the independent `BigInt` oracle.
#[test]
fn line_side_of_point_matches_the_oracle_and_inverts_with_direction() {
    let mut rng = Rng::new(0x5EED_0003);
    for _ in 0..10_000 {
        let line = rng.envelope_line();
        let point = rng.envelope_point();
        let expected = oracle_side_of_line(&line, &big(point.x), &big(point.y), &BigInt::from(1));
        assert_eq!(line.side_of(&point), expected, "line={line:?} p={point:?}");
        assert_eq!(
            line.side_of_point(&Point::Int(point)),
            expected,
            "line={line:?} p={point:?}"
        );
        assert_eq!(
            line.opposite().side_of_point(&Point::Int(point)),
            expected.negate(),
            "line={line:?} p={point:?}"
        );
    }
}

// -------------------------------------------------------------- intersections

/// The exact intersection of two lines has to be *on* both lines: the exact
/// `side_of` predicate must report `Collinear` for each of them, for integral
/// and rational crossings alike.
#[test]
fn line_intersection_lies_exactly_on_both_lines() {
    let check = |l1: &Line, l2: &Line| {
        let is = l1.intersection(l2);
        assert_eq!(l1.side_of_point(&is), Side::Collinear, "{l1:?} {l2:?}");
        assert_eq!(l2.side_of_point(&is), Side::Collinear, "{l1:?} {l2:?}");
        // Independent BigInt oracle, so a shared bug in `side_of` cannot make
        // the assertions above vacuous.
        let (x, y, z) = as_triple(&is);
        assert_eq!(oracle_side_of_line(l1, &x, &y, &z), Side::Collinear);
        assert_eq!(oracle_side_of_line(l2, &x, &y, &z), Side::Collinear);
        is
    };

    // Deterministic cases covering every integral fast path of
    // `Line::intersection` (orthogonal / +-45 degree).
    let integral = [
        (
            Line::new_from_coords(CRIT, -CRIT, CRIT, CRIT),
            Line::new_from_coords(-CRIT, 7, CRIT, 7),
        ),
        (
            Line::new_from_coords(0, 0, 0, 1),
            Line::new_from_coords(-CRIT, -CRIT, CRIT, CRIT),
        ),
        (
            Line::new_from_coords(-5, 3, 7, 3),
            Line::new_from_coords(0, 0, 1, -1),
        ),
        (
            Line::new_from_coords(-CRIT, -CRIT, CRIT, CRIT),
            Line::new_from_coords(CRIT, -CRIT, -CRIT, CRIT),
        ),
    ];
    for (l1, l2) in &integral {
        assert!(
            matches!(check(l1, l2), Point::Int(_)),
            "expected an integral crossing for {l1:?} {l2:?}"
        );
    }

    let mut rng = Rng::new(0x5EED_0004);
    let mut rational_seen = 0;
    for _ in 0..5_000 {
        let l1 = rng.envelope_line();
        let l2 = rng.envelope_line();
        if l1.is_parallel(&l2) {
            continue;
        }
        if matches!(check(&l1, &l2), Point::Rational(_)) {
            rational_seen += 1;
        }
    }
    assert!(rational_seen > 100, "rational crossings never exercised");
}

/// `side_of_intersection` must agree with "intersect exactly, then ask for the
/// side" — including at the envelope corners, where the exact evaluation runs
/// entirely in the `i128` tier (the intermediates reach 2^105).
#[test]
fn side_of_intersection_matches_the_exact_point_predicate() {
    let mut rng = Rng::new(0x5EED_0005);
    let mut checked = 0;
    for _ in 0..5_000 {
        let l = rng.envelope_line();
        let p1 = rng.envelope_line();
        let p2 = rng.envelope_line();
        if p1.is_parallel(&p2) {
            continue;
        }
        let intersection = p1.intersection(&p2);
        let (x, y, z) = as_triple(&intersection);
        assert_eq!(
            l.side_of_intersection(&p1, &p2),
            oracle_side_of_line(&l, &x, &y, &z),
            "l={l:?} p1={p1:?} p2={p2:?} is={intersection:?}"
        );
        checked += 1;
    }
    assert!(checked > 1_000);
}

/// Three concrete triples (found by search) where upstream's float pre-filter
/// answers outright — its error exceeds its own tolerance of 1.0 — and gets the
/// side wrong. The exact `i128` evaluation gets all three right.
#[test]
fn side_of_intersection_survives_the_float_filters_blind_spot() {
    let cases = [
        (
            Line::new_from_coords(1508348599, 1536846818, 1508348595, 1536846814),
            Line::new_from_coords(1508348598, 1536846816, 1508348598, 1536846817),
            Line::new_from_coords(1508348598, 1536846816, 1508348601, 1536846817),
            Side::OnTheRight,
        ),
        (
            Line::new_from_coords(1558564678, 936675857, 1558564672, 936675859),
            Line::new_from_coords(1558564675, 936675858, 1558564672, 936675858),
            Line::new_from_coords(1558564675, 936675858, 1558564674, 936675856),
            Side::Collinear,
        ),
        (
            Line::new_from_coords(1387617979, 1509669972, 1387617975, 1509669974),
            Line::new_from_coords(1387617977, 1509669973, 1387617979, 1509669975),
            Line::new_from_coords(1387617977, 1509669973, 1387617978, 1509669975),
            Side::Collinear,
        ),
    ];
    for (l, p1, p2, want) in cases {
        assert_eq!(l.side_of_intersection(&p1, &p2), want, "l={l:?}");
        // The float pre-filter upstream trusts is off by more than its own
        // tolerance here, which is what used to decide the topology.
        let approx = p1.intersection_approx(&p2);
        assert_ne!(l.side_of_float_tol(&approx, 1.0), want);
    }
}

// ---------------------------------------------------------- point comparisons

/// `Point::compare_x` / `compare_y` across the `Int`/`Rational` boundary must
/// equal the `BigInt` cross-multiplication oracle. The rational operands are
/// real line intersections at the envelope corners, so the `i128` fast path is
/// what answers.
#[test]
fn mixed_point_comparison_matches_the_bigint_oracle() {
    let mut rng = Rng::new(0x5EED_0006);
    let mut mixed = 0;
    for _ in 0..5_000 {
        let l1 = rng.envelope_line();
        let l2 = rng.envelope_line();
        if l1.is_parallel(&l2) {
            continue;
        }
        let rational = l1.intersection(&l2);
        let integral = Point::Int(rng.envelope_point());
        if matches!(rational, Point::Rational(_)) {
            mixed += 1;
        }

        let (x, y, z) = as_triple(&rational);
        let (ox, oy, oz) = as_triple(&integral);
        // (x / z) <=> (ox / oz), both denominators non-negative.
        let expect_x = (&x * &oz).cmp(&(&ox * &z)) as i32;
        let expect_y = (&y * &oz).cmp(&(&oy * &z)) as i32;

        assert_eq!(rational.compare_x(&integral), expect_x, "{rational:?}");
        assert_eq!(rational.compare_y(&integral), expect_y, "{rational:?}");
        assert_eq!(integral.compare_x(&rational), -expect_x, "{rational:?}");
        assert_eq!(integral.compare_y(&rational), -expect_y, "{rational:?}");
        assert_eq!(
            rational.compare_xy(&integral).signum(),
            -integral.compare_xy(&rational).signum()
        );
    }
    assert!(
        mixed > 100,
        "mixed Int/Rational comparisons never exercised"
    );
}

/// Rational-vs-rational comparison, again against the `BigInt` oracle, using
/// two independent intersections as operands.
#[test]
fn rational_point_comparison_matches_the_bigint_oracle() {
    let mut rng = Rng::new(0x5EED_0009);
    let mut compared = 0;
    for _ in 0..3_000 {
        let (a, b) = (rng.envelope_line(), rng.envelope_line());
        let (c, d) = (rng.envelope_line(), rng.envelope_line());
        if a.is_parallel(&b) || c.is_parallel(&d) {
            continue;
        }
        let p = a.intersection(&b);
        let q = c.intersection(&d);
        let (px, py, pz) = as_triple(&p);
        let (qx, qy, qz) = as_triple(&q);
        assert_eq!(
            p.compare_x(&q),
            (&px * &qz).cmp(&(&qx * &pz)) as i32,
            "{p:?} {q:?}"
        );
        assert_eq!(
            p.compare_y(&q),
            (&py * &qz).cmp(&(&qy * &pz)) as i32,
            "{p:?} {q:?}"
        );
        compared += 1;
    }
    assert!(compared > 500);
}

/// The `i128` fast path and the `BigInt` fallback must answer identically. The
/// scaled point's coordinates need 200 bits, so `to_i128` fails and the cold
/// path runs.
#[test]
fn comparison_falls_back_to_bignums_without_changing_the_answer() {
    let huge: BigInt = BigInt::from(1u8) << 200u32;
    let scaled = Point::Rational(RationalPoint::new(
        &huge * 3 + BigInt::from(1),
        &huge * 5,
        huge.clone(),
    ));
    let small = Point::Rational(RationalPoint::new(
        BigInt::from(3),
        BigInt::from(5),
        BigInt::from(1),
    ));
    // 3 + 1/2^200 > 3, and the y coordinates are equal.
    assert_eq!(scaled.compare_x(&small), 1);
    assert_eq!(small.compare_x(&scaled), -1);
    assert_eq!(scaled.compare_y(&small), 0);
    assert_eq!(scaled.compare_xy(&small), 1);

    let int_point = Point::Int(IntPoint::new(3, 5));
    assert_eq!(scaled.compare_x(&int_point), 1);
    assert_eq!(scaled.compare_y(&int_point), 0);
    assert_eq!(int_point.compare_x(&scaled), -1);
}

/// `Point::get_instance` used to build an `IntPoint` before deciding, and to
/// call `i32::abs`, which overflows on `i32::MIN`.
#[test]
fn get_instance_promotes_out_of_envelope_coordinates() {
    assert!(matches!(Point::get_instance(CRIT, -CRIT), Point::Int(_)));
    assert!(matches!(
        Point::get_instance(CRIT + 1, 0),
        Point::Rational(_)
    ));
    let extreme = Point::get_instance(i32::MIN, i32::MAX);
    assert!(matches!(extreme, Point::Rational(_)));
    assert_eq!(extreme.compare_x(&Point::get_instance(0, 0)), -1);
    assert_eq!(extreme.compare_y(&Point::get_instance(0, 0)), 1);
    assert!(matches!(
        Vector::get_instance(i32::MIN, 0),
        Vector::Rational(_)
    ));
    assert!(matches!(Vector::get_instance(CRIT, CRIT), Vector::Int(_)));
}

/// Regression: the "surrounding" shapes of a rational point were derived from
/// its `f64` approximation. A point one ulp below the lattice converts to
/// exactly that lattice value, so the box collapsed to `[n, n]` and no longer
/// contained the point it was supposed to surround.
#[test]
fn surrounding_shapes_of_a_near_integer_rational_contain_it() {
    let n: i64 = 1 << 25; // CRIT_INT
    let z: BigInt = BigInt::from(1u8) << 60u32;
    let exact_n = BigInt::from(n) * &z;
    // x / z = n - 2^-60, y / z = n.
    let point = RationalPoint::new(&exact_n - BigInt::from(1), exact_n.clone(), z.clone());

    // The f64 quotient rounds up to exactly n, which is what used to be floored.
    assert_eq!(point.to_float().x, n as f64);

    let surrounding = point.surrounding_box();
    assert_eq!(surrounding.ll.x, (n - 1) as i32);
    assert_eq!(surrounding.ur.x, n as i32);
    assert_eq!(surrounding.ll.y, n as i32);
    assert_eq!(surrounding.ur.y, n as i32);
    assert!(
        point.is_contained_in(&surrounding),
        "the surrounding box must contain its point"
    );

    // Same for the diagonal bounds of the surrounding octagon:
    // (x - y) / z = -2^-60 floors to -1, (x + y) / z = 2n - 2^-60.
    let octagon = point.surrounding_octagon();
    assert_eq!(octagon.left_x, (n - 1) as i32);
    assert_eq!(octagon.right_x, n as i32);
    assert_eq!(octagon.upper_left_diagonal_x, -1);
    assert_eq!(octagon.lower_right_diagonal_x, 0);
    assert_eq!(octagon.lower_left_diagonal_x, (2 * n - 1) as i32);
    assert_eq!(octagon.upper_right_diagonal_x, (2 * n) as i32);
}

// -------------------------------------------------------------- line segments

/// Regression: the segment's end point is the rational `(3, 3/2)`. Rounding it
/// to `(3, 2)` to build the perpendicular closing line — the only way to phrase
/// upstream's test with an integer-valued `Line` — pushes both end points onto
/// the same side, so `contains` used to reject the segment's own end point.
#[test]
fn contains_accepts_a_rational_end_point() {
    let middle = Line::new_from_coords(0, 0, 2, 1);
    let start = Line::new_from_coords(0, 0, 0, 1);
    let end = Line::new_from_coords(3, 0, 3, 1);
    let segment = LineSegment::new(start, middle, end);

    let end_point = segment.end_point();
    assert_eq!(
        end_point,
        Point::Rational(RationalPoint::new(
            BigInt::from(6),
            BigInt::from(3),
            BigInt::from(2)
        ))
    );
    assert!(segment.contains(&end_point), "end point must be contained");
    assert!(segment.contains(&segment.start_point()));
    assert!(segment.contains(&Point::Int(IntPoint::new(2, 1))));
    // Beyond either end, and off the line.
    assert!(!segment.contains(&Point::Int(IntPoint::new(4, 2))));
    assert!(!segment.contains(&Point::Int(IntPoint::new(-2, -1))));
    assert!(!segment.contains(&Point::Int(IntPoint::new(0, 1))));
}

/// `contains` must agree with the exact `BigInt` "collinear and between"
/// oracle, for integral candidates on and off the segment and for rational
/// candidates produced by intersecting the middle line with a third line.
#[test]
fn contains_matches_the_exact_between_oracle() {
    let mut rng = Rng::new(0x5EED_0007);
    let mut rational_candidates = 0;
    for _ in 0..2_000 {
        let middle = rng.envelope_line();
        let perpendicular = middle.direction().turn_45_degree(2);
        let start = Line::new_from_point_and_dir(middle.a, &perpendicular);
        let end = Line::new_from_point_and_dir(middle.b, &perpendicular);
        let segment = LineSegment::new(start, middle, end);
        let (s, e) = (segment.start_point(), segment.end_point());

        let cross = rng.envelope_line();
        let mut candidates = vec![
            s.clone(),
            e.clone(),
            Point::Int(rng.envelope_point()),
            Point::Int(middle.a),
            Point::Int(middle.b),
        ];
        if !cross.is_parallel(&middle) {
            let on_line = middle.intersection(&cross);
            if matches!(on_line, Point::Rational(_)) {
                rational_candidates += 1;
            }
            candidates.push(on_line);
        }

        for candidate in &candidates {
            assert_eq!(
                segment.contains(candidate),
                oracle_contains(&middle, &s, &e, candidate),
                "segment={segment:?} candidate={candidate:?}"
            );
        }
    }
    assert!(
        rational_candidates > 100,
        "rational candidates never exercised"
    );
}

// -------------------------------------------------------------- line ordering

/// Regression: `cmp_line`'s tie-breaking determinant is
/// `(n + 2) * n - (n + 1)^2 = -1` for `n = 2^30`. Both products round to the
/// same `f64`, so the old comparison reported "equal directions" for two
/// different directions — an `Ord` implementation that silently merges lines.
#[test]
fn cmp_line_keeps_its_sign_beyond_the_double_mantissa() {
    let n = 1i32 << 30;
    let l1 = Line::new_from_coords(0, 0, n + 1, n);
    let l2 = Line::new_from_coords(0, 0, n + 2, n + 1);
    assert_eq!(l1.cmp_line(&l2), -1);
    assert_eq!(l2.cmp_line(&l1), 1);
    assert!(l1 < l2);

    // The float form of the same determinant collapses to zero.
    let float_det = ((n + 2) as f64) * (n as f64) - ((n + 1) as f64) * ((n + 1) as f64);
    assert_eq!(float_det, 0.0);
}

/// `cmp_line` must be a consistent (antisymmetric, reflexive) comparator.
#[test]
fn cmp_line_is_antisymmetric() {
    let mut rng = Rng::new(0x5EED_0008);
    for _ in 0..10_000 {
        let l1 = rng.envelope_line();
        let l2 = rng.envelope_line();
        assert_eq!(l1.cmp_line(&l2), -l2.cmp_line(&l1), "{l1:?} {l2:?}");
        assert_eq!(l1.cmp_line(&l1), 0);
    }
}

// ------------------------------------------------------------ envelope + tier

/// The checked constructors reject exactly what the envelope forbids.
#[test]
fn checked_constructors_enforce_the_envelope() {
    assert_eq!(
        IntPoint::try_new(CRIT, -CRIT),
        Some(IntPoint::new(CRIT, -CRIT))
    );
    assert_eq!(IntPoint::try_new(CRIT + 1, 0), None);
    assert_eq!(IntPoint::try_new(0, i32::MIN), None);
    assert!(IntPoint::new(CRIT, CRIT).is_in_envelope());

    assert!(Line::try_new_from_coords(CRIT, CRIT, -CRIT, -CRIT).is_some());
    assert!(Line::try_new_from_coords(CRIT + 1, 0, 0, 0).is_none());
    assert!(Line::try_new(IntPoint::new(0, 0), IntPoint::new(CRIT, 0)).is_some());

    // A vector may reach twice the point envelope: it is a difference.
    assert!(IntVector::try_new(2 * CRIT, -2 * CRIT).is_some());
    assert!(IntVector::try_new(2 * CRIT + 1, 0).is_none());
}

/// At the envelope corners the products reach 2^52 and the determinants 2^53:
/// the `i64` tier must still be exact, and the `i128` tier must take over
/// (rather than wrap) once the operands leave `i64`.
#[test]
fn arithmetic_tiers_are_exact_at_and_beyond_the_envelope() {
    let c = Limits::CRIT_INT_I64 * 2; // 2^26, the vector-coordinate bound
    assert_eq!(
        determinant_exact(c, c, c, -c),
        -2 * (c as i128) * (c as i128)
    );
    assert_eq!(
        scalar_product_exact(c, c, c, c),
        2 * (c as i128) * (c as i128)
    );

    let corners = [
        IntPoint::new(CRIT, CRIT),
        IntPoint::new(CRIT, -CRIT),
        IntPoint::new(-CRIT, CRIT),
        IntPoint::new(-CRIT, -CRIT),
        IntPoint::new(CRIT, CRIT - 1),
        IntPoint::new(-CRIT + 1, CRIT),
    ];
    for a in &corners {
        for b in &corners {
            if a == b {
                continue;
            }
            let line = Line::new(*a, *b);
            for p in &corners {
                let expected = oracle_side_of_line(&line, &big(p.x), &big(p.y), &BigInt::from(1));
                assert_eq!(line.side_of(p), expected, "{line:?} {p:?}");
                assert_eq!(
                    line.side_of_point(&Point::Int(*p)),
                    expected,
                    "{line:?} {p:?}"
                );
            }
            // Three corner lines meeting: the exact i128 evaluation of
            // `side_of_intersection` must match the exact point predicate.
            for c in &corners {
                let other = Line::new(*b, *c);
                if line.is_parallel(&other) || b == c {
                    continue;
                }
                let (x, y, z) = as_triple(&line.intersection(&other));
                for p in &corners {
                    if p == a || p == b {
                        continue;
                    }
                    let probe = Line::new(*a, *p);
                    assert_eq!(
                        probe.side_of_intersection(&line, &other),
                        oracle_side_of_line(&probe, &x, &y, &z),
                        "probe={probe:?} l1={line:?} l2={other:?}"
                    );
                }
            }
        }
    }

    // i64 would overflow on both of these; the promotion must keep the sign.
    assert_eq!(
        signum_of_exact(scalar_product_exact(i64::MAX, i64::MAX, 2, 2)),
        Signum::Positive
    );
    // (-2^63) * (-2) - (2^63 - 1) * 2 = 2: the products need i128, and the
    // wrapped i64 answer would have been 0.
    assert_eq!(
        determinant_exact(i64::MIN, i64::MAX, 2, -2),
        (i64::MIN as i128) * -2 - (i64::MAX as i128) * 2
    );
    assert_eq!(
        side_of_exact(determinant_exact(i64::MIN, i64::MAX, 2, -2)),
        Side::OnTheLeft
    );
}
