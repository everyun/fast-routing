//! Robustness of the planar primitives on the degenerate inputs the router
//! actually produces: clipped octagon corners, empty polylines, the zero
//! rational vector, simplices whose border lines are all parallel, and polygon
//! orientation.
//!
//! Every test here reproduces a defect that panicked or answered wrongly
//! before the fix, phrased as the behaviour the callers need.

use num_bigint::BigInt;

use fr_geometry::planar::{
    area::Area, direction::Direction, float_point::FloatPoint, int_box::IntBox,
    int_octagon::IntOctagon, int_point::IntPoint, int_vector::IntVector, limits::Limits,
    line::Line, point::Point, polygon::Polygon, polyline::Polyline, rational_point::RationalPoint,
    rational_vector::RationalVector, simplex::Simplex, vector::Vector,
};

fn ip(x: i32, y: i32) -> Point {
    Point::Int(IntPoint::new(x, y))
}

fn rp(x: i64, y: i64, z: i64) -> Point {
    Point::Rational(RationalPoint::new(
        BigInt::from(x),
        BigInt::from(y),
        BigInt::from(z),
    ))
}

/// Octagon inscribed in `(0, 0, 100, 100)` whose 4 corners are clipped by the
/// diagonal border lines `20 <= x + y <= 180` and `-80 <= x - y <= 80`.
fn clipped_octagon() -> IntOctagon {
    IntOctagon::new(0, 0, 100, 100, -80, 80, 20, 180)
}

// ---------------------------------------------------------------------------
// IntOctagon: containment has to honour all 8 half-planes, not just the box
// ---------------------------------------------------------------------------

#[test]
fn octagon_containment_rejects_the_clipped_corners_for_every_point_kind() {
    let oct = clipped_octagon();
    // One point per clipped corner: inside the bounding box, outside exactly
    // one diagonal constraint.
    for (x, y) in [(5, 5), (95, 5), (95, 95), (5, 95)] {
        assert!(
            !oct.contains_point(&ip(x, y)),
            "int ({x}, {y}) is in a clipped corner"
        );
        assert!(
            !oct.contains_point(&rp(x as i64 * 3, y as i64 * 3, 3)),
            "rational ({x}, {y}) is in a clipped corner"
        );
        assert!(
            !oct.contains_float_point(&FloatPoint::new(x as f64, y as f64)),
            "float ({x}, {y}) is in a clipped corner"
        );
        // The bounding box still contains them, which is what made the
        // box-only test look plausible.
        assert!(Area::contains_point(&oct.bounding_box(), &ip(x, y)));
    }

    let (x, y) = (50i64, 50i64);
    assert!(oct.contains_point(&ip(x as i32, y as i32)));
    assert!(oct.contains_point(&rp(x * 3, y * 3, 3)));
    assert!(oct.contains_float_point(&FloatPoint::new(x as f64, y as f64)));
}

#[test]
fn octagon_containment_agrees_across_point_kinds_on_a_grid() {
    let oct = clipped_octagon();
    for x in -20..=120 {
        for y in -20..=120 {
            let expected = oct.contains_point(&ip(x, y));
            assert_eq!(
                oct.contains_point(&rp(x as i64 * 7, y as i64 * 7, 7)),
                expected,
                "rational disagrees at ({x}, {y})"
            );
            assert_eq!(
                oct.contains_float_point(&FloatPoint::new(x as f64, y as f64)),
                expected,
                "float disagrees at ({x}, {y})"
            );
        }
    }
}

#[test]
fn octagon_containment_of_rational_points_is_exact_on_the_diagonal_border() {
    let oct = clipped_octagon();
    // (9.5, 10.5) sits exactly on `x + y == 20`, the lower left border line.
    assert!(oct.contains_point(&rp(19, 21, 2)));
    // (9.5, 10.0) is half a unit short of it; a float round-trip through a
    // box-only test called this contained.
    assert!(!oct.contains_point(&rp(19, 20, 2)));
}

#[test]
fn octagon_contains_no_point_at_infinity() {
    // `Line::intersection` of two parallel lines yields a triple with `z == 0`,
    // which is not an affine point and must not cross-multiply into a hit.
    let parallel = Line::new_from_coords(0, 10, 1, 10);
    let also_parallel = Line::new_from_coords(0, 20, 1, 20);
    let at_infinity = parallel.intersection(&also_parallel);
    assert!(matches!(&at_infinity, Point::Rational(r) if r.z == BigInt::from(0)));
    assert!(!clipped_octagon().contains_point(&at_infinity));
    assert!(!clipped_octagon().contains_point(&rp(0, 0, 0)));
}

// ---------------------------------------------------------------------------
// Polyline: a degenerate polyline has no lines, so `arr.len() - 1` underflows
// ---------------------------------------------------------------------------

/// `new_from_two_points` with two equal corners keeps no line at all.
fn degenerate_polyline() -> Polyline {
    let p = ip(7, 9);
    let polyline = Polyline::new_from_two_points(&p, &p);
    assert!(polyline.arr.is_empty(), "the repro needs an empty polyline");
    polyline
}

#[test]
fn degenerate_polyline_has_no_corners() {
    let polyline = degenerate_polyline();
    assert_eq!(polyline.corner_count(), 0);
    // `last_corner` used to panic on `arr.len() - 2`; it now reports the same
    // placeholder `corner` and `first_corner` already return.
    assert_eq!(polyline.last_corner(), ip(0, 0));
    assert_eq!(polyline.last_corner(), polyline.first_corner());
}

#[test]
fn degenerate_polyline_contains_nothing() {
    let polyline = degenerate_polyline();
    // Used to panic on `1..arr.len() - 1`.
    assert!(!polyline.contains(&ip(7, 9)));
    assert!(!polyline.contains(&ip(0, 0)));
}

#[test]
fn degenerate_polyline_has_no_projection_line() {
    // Used to panic on `1..arr.len() - 1`.
    assert!(degenerate_polyline().projection_line(&ip(7, 9)).is_none());
}

#[test]
fn degenerate_polyline_has_no_offset_shapes() {
    let polyline = degenerate_polyline();
    // Used to panic on `arr.len() - 1` while clamping the requested range.
    assert!(polyline.offset_shapes_range(10, 0, 5).is_empty());
    assert!(polyline.offset_shapes_range(10, 3, 3).is_empty());
    assert!(polyline.offset_shapes(10).is_empty());
    assert_eq!(polyline.offset_shape(10, 0), None);
}

#[test]
fn two_point_polyline_still_reports_its_end_corners() {
    let from = ip(0, 0);
    let to = ip(100, 0);
    let polyline = Polyline::new_from_two_points(&from, &to);
    assert_eq!(polyline.corner_count(), 2);
    assert_eq!(polyline.first_corner(), from);
    assert_eq!(polyline.last_corner(), to);
    assert!(polyline.contains(&ip(50, 0)));
    assert!(!polyline.contains(&ip(50, 7)));
    assert!(polyline.projection_line(&ip(50, 7)).is_some());
}

// ---------------------------------------------------------------------------
// RationalVector: the zero vector has no direction
// ---------------------------------------------------------------------------

#[test]
fn zero_rational_vector_normalizes_to_the_null_direction() {
    // `gcd(0, 0) == 0`, so dividing by it panicked inside `BigInt`.
    for z in [1, 5] {
        let zero = RationalVector::new(BigInt::from(0), BigInt::from(0), BigInt::from(z));
        assert!(zero.is_zero());
        assert_eq!(zero.to_normalized_direction(), Direction::NULL);
        assert_eq!(
            Vector::Rational(zero).to_normalized_direction(),
            Direction::NULL
        );
    }
    // Same answer the integer path has always given.
    assert_eq!(
        IntVector::new(0, 0).to_normalized_direction(),
        Direction::NULL
    );
}

#[test]
fn non_zero_rational_vectors_still_normalize_by_the_gcd() {
    let v = RationalVector::new(BigInt::from(6), BigInt::from(3), BigInt::from(2));
    assert_eq!(v.to_normalized_direction(), Direction::new(2, 1));
    let axis = RationalVector::new(BigInt::from(0), BigInt::from(-5), BigInt::from(1));
    assert_eq!(axis.to_normalized_direction(), Direction::new(0, -1));
}

// ---------------------------------------------------------------------------
// Simplex: all border lines parallel means no corner at all
// ---------------------------------------------------------------------------

/// Directed line whose half-plane is `y >= level`.
fn above(level: i32) -> Line {
    Line::new_from_coords(0, level, 1, level)
}

/// Directed line whose half-plane is `y <= level`.
fn below(level: i32) -> Line {
    above(level).opposite()
}

#[test]
fn parallel_border_lines_define_no_corner() {
    let simplex = Simplex::new(vec![above(0), above(10), above(20)]);
    for i in 0..simplex.border_line_count() {
        assert!(!simplex.corner_is_defined(i), "corner {i} cannot exist");
        assert!(!simplex.corner_is_bounded(i));
    }
    // A box simplex is the control: every corner exists.
    let box_simplex = IntBox::new(0, 0, 100, 100).to_simplex();
    for i in 0..box_simplex.border_line_count() {
        assert!(box_simplex.corner_is_defined(i));
    }
}

#[test]
fn nested_parallel_half_planes_are_two_dimensional() {
    // `y >= 0 && y >= 10 && y >= 20` is the half-plane `y >= 20`, not a point:
    // the 3-line branch used to derive the answer from the intersection of two
    // parallel lines and reported dimension 0.
    assert_eq!(
        Simplex::new(vec![above(0), above(10), above(20)]).dimension(),
        2
    );
    // Same for the `arr.len() > 4` shortcut, which answered 2 by accident.
    let many = Simplex::new((0..6).map(|i| above(10 * i)).collect::<Vec<_>>());
    assert_eq!(many.dimension(), 2);
}

#[test]
fn opposing_parallel_half_planes_report_strip_line_or_empty() {
    // `y >= 0 && y <= 10 && y >= -5` is a proper strip.
    assert_eq!(
        Simplex::new(vec![above(0), below(10), above(-5)]).dimension(),
        2
    );
    // `y >= 10 && y <= 10 && y >= 0` collapses onto a single line.
    assert_eq!(
        Simplex::new(vec![above(10), below(10), above(0)]).dimension(),
        1
    );
    // `y >= 20 && y <= 10 && y >= 0` has no solution.
    assert_eq!(
        Simplex::new(vec![above(20), below(10), above(0)]).dimension(),
        -1
    );
}

#[test]
fn centre_of_gravity_of_a_corner_less_simplex_stays_finite() {
    let simplex = Simplex::new(vec![above(0), above(10), above(20), above(30), above(40)]);
    let centre = simplex.centre_of_gravity();
    assert!(
        centre.x.is_finite() && centre.y.is_finite(),
        "centroid must not be NaN or infinite: {centre:?}"
    );
    // `corner_approx` reports `(i32::MAX, i32::MAX)` for parallel lines;
    // averaging those put the centroid at the coordinate limit.
    assert!(
        centre.x.abs() < i32::MAX as f64 && centre.y.abs() < i32::MAX as f64,
        "centroid must not be dragged to the coordinate limit: {centre:?}"
    );
    // Skipping the non-existent corners must not divide by zero either.
    assert_eq!(centre, FloatPoint::new(0.0, 20.0));
}

#[test]
fn centre_of_gravity_of_a_bounded_simplex_is_unchanged() {
    let box_simplex = IntBox::new(0, 0, 100, 100).to_simplex();
    assert_eq!(box_simplex.centre_of_gravity(), FloatPoint::new(50.0, 50.0));
    let octagon = clipped_octagon().to_simplex();
    let centre = octagon.centre_of_gravity();
    assert!(octagon.contains_float(&centre, 0.0), "{centre:?}");
}

// ---------------------------------------------------------------------------
// Polygon: orientation must be decided by an exact integer area
// ---------------------------------------------------------------------------

fn square_ccw() -> Polygon {
    Polygon::new(&[ip(0, 0), ip(100, 0), ip(100, 100), ip(0, 100)])
}

#[test]
fn winding_number_of_an_axis_parallel_square_is_plus_or_minus_one() {
    // The `f64` angle sum answered 0 for *every* polygon, so no caller could
    // tell clockwise from counter-clockwise.
    let ccw = square_ccw();
    assert_eq!(ccw.winding_number_after_closing(), 1);
    assert_eq!(ccw.revert_corners().winding_number_after_closing(), -1);
    assert_eq!(ccw.is_clockwise(), Some(false));
    assert_eq!(ccw.revert_corners().is_clockwise(), Some(true));
}

#[test]
fn corner_area_is_twice_the_signed_area() {
    let ccw = square_ccw();
    assert_eq!(ccw.corner_area(), Some(2 * 100 * 100));
    assert_eq!(ccw.revert_corners().corner_area(), Some(-2 * 100 * 100));

    // Explicitly closing the polygon repeats a corner; the shoelace term for
    // the repeated edge is 0, so the area is unchanged.
    let closed = Polygon::new(&[ip(0, 0), ip(100, 0), ip(100, 100), ip(0, 100), ip(0, 0)]);
    assert_eq!(closed.corner_area(), Some(2 * 100 * 100));
    assert_eq!(closed.winding_number_after_closing(), 1);
}

#[test]
fn degenerate_polygons_have_no_orientation() {
    for corners in [
        vec![],
        vec![ip(0, 0)],
        vec![ip(0, 0), ip(10, 10)],
        // Collapses to 2 corners: the middle one is collinear.
        vec![ip(0, 0), ip(5, 5), ip(10, 10)],
    ] {
        let polygon = Polygon::new(&corners);
        assert_eq!(polygon.corner_area(), Some(0), "{corners:?}");
        assert_eq!(polygon.is_clockwise(), None, "{corners:?}");
        assert_eq!(polygon.winding_number_after_closing(), 0, "{corners:?}");
    }
}

#[test]
fn orientation_is_exact_for_a_needle_at_the_coordinate_envelope() {
    let k = Limits::CRIT_INT;
    // Doubled area 1: the smallest non-zero value there is, at the far corner
    // of the coordinate envelope where every intermediate product is ~2^50 and
    // the three terms `k`, `1 - 2k`, `k` all but cancel.
    let needle = Polygon::new(&[ip(k, k), ip(k - 1, k), ip(k, k - 1)]);
    assert_eq!(needle.corner_area(), Some(1));
    assert_eq!(needle.winding_number_after_closing(), 1);
    assert_eq!(needle.revert_corners().corner_area(), Some(-1));
    assert_eq!(needle.revert_corners().winding_number_after_closing(), -1);
}

#[test]
fn corner_area_is_exact_at_the_f64_integer_limit() {
    // The biggest polygon the coordinate envelope allows is the square
    // `[-CRIT_INT, CRIT_INT]^2`, whose doubled area is `8 * CRIT_INT^2 == 2^53`
    // — exactly where `f64` stops representing consecutive integers. `i128`
    // makes the accumulator exact regardless of where the sum lands.
    let k = Limits::CRIT_INT;
    let square = Polygon::new(&[ip(-k, -k), ip(k, -k), ip(k, k), ip(-k, k)]);
    let doubled_area = 8 * (k as i128) * (k as i128);
    assert_eq!(doubled_area, 1i128 << 53);
    assert_eq!(square.corner_area(), Some(doubled_area));

    // Clipping one corner takes half a unit off, so the exact answer becomes
    // the odd `2^53 - 1` while the running sum still peaks at `2^53`.
    let clipped = Polygon::new(&[ip(-k, -k), ip(k, -k), ip(k, k - 1), ip(k - 1, k), ip(-k, k)]);
    assert_eq!(clipped.corner_area(), Some(doubled_area - 1));
    assert_eq!(clipped.winding_number_after_closing(), 1);
}

#[test]
fn orientation_of_a_polygon_with_a_rational_corner_stays_exact() {
    // Twice the area is not an integer here, so `corner_area` declines and the
    // exact `BigInt` fallback decides the orientation.
    let corners = [ip(0, 0), ip(2, 0), rp(1, 3, 2)];
    let polygon = Polygon::new(&corners);
    assert_eq!(polygon.corner_area(), None);
    assert_eq!(polygon.is_clockwise(), Some(false));
    assert_eq!(polygon.winding_number_after_closing(), 1);

    let reverted = polygon.revert_corners();
    assert_eq!(reverted.is_clockwise(), Some(true));
    assert_eq!(reverted.winding_number_after_closing(), -1);
}
