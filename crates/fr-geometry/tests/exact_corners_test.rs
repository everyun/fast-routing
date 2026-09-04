//! `Polyline::from_exact_corners` — the session round-trip constructor for
//! corners that sit on the half-integer grid (diagonal∩diagonal meets).
//!
//! The motivating board is Issue102-Mars-64-revE-rot00: a pull-tightened GND
//! trace corner at (664434.5, -706045.5) whose session-file rounding moved it
//! 0.5 units into a clearance-exact gap (REPORT-ses-fidelity).

use fr_geometry::planar::{IntPoint, Point, Polyline};

#[test]
fn integer_corners_build_the_same_polyline_as_from_int_points() {
    let ints = [
        IntPoint::new(0, 0),
        IntPoint::new(100, 0),
        IntPoint::new(100, 100),
    ];
    let exact = from_ints(&ints);
    let via_ints = Polyline::from_int_points(&ints);
    // The representative points on each line may differ; the geometry — the
    // corner sequence — must not.
    assert_eq!(exact.corner_count(), via_ints.corner_count());
    for i in 0..exact.corner_count() {
        assert_eq!(exact.corner(i), via_ints.corner(i), "corner {i}");
    }
}

fn from_ints(points: &[IntPoint]) -> Polyline {
    let corners: Vec<(f64, f64)> = points.iter().map(|p| (f64::from(p.x), f64::from(p.y))).collect();
    Polyline::from_exact_corners(&corners).expect("integer corners are representable")
}

#[test]
fn a_diagonal_diagonal_corner_keeps_its_half_unit() {
    // (0,0) → (0.5, 0.5) → (1, 0): a +45° segment meeting a -45° segment.
    let polyline =
        Polyline::from_exact_corners(&[(0.0, 0.0), (0.5, 0.5), (1.0, 0.0)]).expect("representable");
    let apex = polyline.corner(1);
    assert!(
        matches!(apex, Point::Rational(_)),
        "the apex must stay off the integer grid: {apex:?}"
    );
    let apex = apex.to_float();
    assert_eq!((apex.x, apex.y), (0.5, 0.5));
    assert_eq!(polyline.first_corner(), Point::Int(IntPoint::new(0, 0)));
    assert_eq!(polyline.last_corner(), Point::Int(IntPoint::new(1, 0)));
}

#[test]
fn exact_corners_reproduce_a_polyline_built_from_lines() {
    // Round trip: polyline -> corner sequence (with a .5 corner) -> polyline.
    // y = x meets y = -x + 7 at (3.5, 3.5).
    use fr_geometry::planar::{Direction, Line};
    let lines = [
        Line::new_from_point_and_dir(IntPoint::new(0, 0), &Direction::DOWN),
        Line::new_from_point_and_dir(IntPoint::new(0, 0), &Direction::RIGHT45),
        Line::new_from_point_and_dir(IntPoint::new(7, 0), &Direction::UP45),
        Line::new_from_point_and_dir(IntPoint::new(0, 0), &Direction::DOWN45),
        Line::new_from_point_and_dir(IntPoint::new(0, 0), &Direction::RIGHT),
    ];
    let original = Polyline::new_from_lines(&lines);
    let corners: Vec<(f64, f64)> = (0..original.corner_count())
        .map(|i| {
            let c = original.corner(i).to_float();
            (c.x, c.y)
        })
        .collect();
    assert!(
        corners.iter().any(|&(x, y)| x.fract() != 0.0 || y.fract() != 0.0),
        "the fixture must contain a half-unit corner: {corners:?}"
    );
    let rebuilt = Polyline::from_exact_corners(&corners).expect("representable");
    for i in 0..original.corner_count() {
        assert_eq!(
            rebuilt.corner(i),
            original.corner(i),
            "corner {i} differs after the exact round trip"
        );
    }
}

#[test]
fn unrepresentable_corners_are_refused() {
    // An arbitrary-angle segment has no 45° direction.
    assert!(Polyline::from_exact_corners(&[(0.0, 0.0), (1.0, 2.0)]).is_none());
    // A horizontal segment on a half-integer row holds no integer point.
    assert!(Polyline::from_exact_corners(&[(0.5, 0.5), (3.5, 0.5)]).is_none());
    // Fewer than two distinct corners.
    assert!(Polyline::from_exact_corners(&[(1.0, 1.0), (1.0, 1.0)]).is_none());
    assert!(Polyline::from_exact_corners(&[]).is_none());
}
