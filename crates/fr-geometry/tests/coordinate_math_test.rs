//! Comprehensive tests for planar geometry and coordinate math.

use fr_geometry::planar::{
    float_line::FloatLine, float_point::FloatPoint, int_box::IntBox, int_octagon::IntOctagon,
    int_point::IntPoint, line::Line, point::Point, side::Side,
};

#[test]
fn test_fortyfive_degree_corner_all_eight_octants() {
    let p0 = IntPoint::new(0, 0);

    // Octant 1: 0 < dy < dx -> (10, 4)
    let p = IntPoint::new(10, 4);
    let c_left = p0.fortyfive_degree_corner(&p, true).unwrap();
    let c_right = p0.fortyfive_degree_corner(&p, false).unwrap();
    assert_eq!(c_left, IntPoint::new(6, 0));
    assert_eq!(c_right, IntPoint::new(4, 4));

    // Octant 2: 0 < dx < dy -> (4, 10)
    let p = IntPoint::new(4, 10);
    let c_left = p0.fortyfive_degree_corner(&p, true).unwrap();
    let c_right = p0.fortyfive_degree_corner(&p, false).unwrap();
    assert_eq!(c_left, IntPoint::new(4, 4));
    assert_eq!(c_right, IntPoint::new(0, 6));

    // Collinear with 45° should return None
    assert_eq!(p0.fortyfive_degree_corner(&IntPoint::new(10, 10), true), None);
    assert_eq!(p0.fortyfive_degree_corner(&IntPoint::new(10, 0), true), None);
    assert_eq!(p0.fortyfive_degree_corner(&IntPoint::new(0, 10), true), None);
}

#[test]
fn test_ninety_degree_corner_quadrants() {
    let p0 = IntPoint::new(0, 0);

    // (+x, +y)
    let p = IntPoint::new(10, 20);
    assert_eq!(p0.ninety_degree_corner(&p, true).unwrap(), IntPoint::new(10, 0));
    assert_eq!(p0.ninety_degree_corner(&p, false).unwrap(), IntPoint::new(0, 20));

    // (-x, +y)
    let p = IntPoint::new(-10, 20);
    assert_eq!(p0.ninety_degree_corner(&p, true).unwrap(), IntPoint::new(0, 20));
    assert_eq!(p0.ninety_degree_corner(&p, false).unwrap(), IntPoint::new(-10, 0));

    // Already orthogonal -> None
    assert_eq!(p0.ninety_degree_corner(&IntPoint::new(10, 0), true), None);
    assert_eq!(p0.ninety_degree_corner(&IntPoint::new(0, 20), true), None);
}

#[test]
fn test_orthogonal_projection() {
    let p0 = IntPoint::new(10, 10);

    // dx < dy -> snaps to vertical line x = 10
    let p = IntPoint::new(12, 30);
    assert_eq!(p.orthogonal_projection(&p0), IntPoint::new(10, 30));

    // dx > dy -> snaps to horizontal line y = 10
    let p = IntPoint::new(30, 12);
    assert_eq!(p.orthogonal_projection(&p0), IntPoint::new(30, 10));
}

#[test]
fn test_perpendicular_projection_exact_grid_and_rational() {
    // Horizontal line y = 0
    let line = Line::new_from_coords(0, 0, 10, 0);
    let p = IntPoint::new(5, 7);
    assert_eq!(p.perpendicular_projection(&line), Point::Int(IntPoint::new(5, 0)));

    // Diagonal line through (0, 0) and (10, 10)
    let diag = Line::new_from_coords(0, 0, 10, 10);
    let p = IntPoint::new(0, 10);
    // Projection of (0, 10) onto y = x is (5, 5)
    assert_eq!(p.perpendicular_projection(&diag), Point::Int(IntPoint::new(5, 5)));
}

#[test]
fn test_int_box_operations() {
    let b1 = IntBox::new(0, 0, 100, 100);
    let b2 = IntBox::new(50, 50, 150, 150);

    assert_eq!(b1.width(), 100);
    assert_eq!(b1.height(), 100);
    assert_eq!(b1.area(), 10000.0);
    assert_eq!(b1.circumference(), 400.0);

    // Intersection
    let is = b1.intersection(&b2);
    assert_eq!(is, IntBox::new(50, 50, 100, 100));

    // Union
    let u = b1.union(&b2);
    assert_eq!(u, IntBox::new(0, 0, 150, 150));

    // Overlaps and intersects
    assert!(b1.intersects(&b2));
    assert!(b1.overlaps(&b2));

    // Non-overlapping
    let b3 = IntBox::new(200, 200, 300, 300);
    assert!(!b1.intersects(&b3));
    assert_eq!(b1.intersection(&b3), IntBox::EMPTY);

    // Section division
    let sections = b1.divide_into_sections(50.0);
    assert_eq!(sections.len(), 4);
    assert_eq!(sections[0], IntBox::new(0, 0, 50, 50));
    assert_eq!(sections[3], IntBox::new(50, 50, 100, 100));
}

#[test]
fn test_int_octagon_area_and_corners() {
    // Octagon corresponding to box [0, 0, 100, 100]
    let box_oct = IntBox::new(0, 0, 100, 100).to_int_octagon();
    assert_eq!(box_oct.area(), 10000.0);
    assert_eq!(box_oct.bounding_box(), IntBox::new(0, 0, 100, 100));

    // Chamfered octagon with 45° corners
    let oct = IntOctagon::new(0, 0, 100, 100, -80, 80, 20, 180);
    assert!(!oct.is_empty());
    assert_eq!(oct.dimension(), 2);
    assert!(oct.area() > 0.0);
}

#[test]
fn test_line_intersections_all_cases() {
    // Orthogonal: vertical and horizontal
    let l1 = Line::new_from_coords(50, 0, 50, 100);
    let l2 = Line::new_from_coords(0, 30, 100, 30);
    assert_eq!(l1.intersection(&l2), Point::Int(IntPoint::new(50, 30)));

    // 45-degree diagonals intersection
    let d1 = Line::new_from_coords(0, 0, 100, 100);
    let d2 = Line::new_from_coords(0, 100, 100, 0);
    assert_eq!(d1.intersection(&d2), Point::Int(IntPoint::new(50, 50)));

    // Distance and sides: Line.side_of(point) answers "is the LINE on the left/right of the point"
    // For a line pointing +y at x=50, the line is on the left of point (60, 50).
    assert_eq!(l1.side_of(&IntPoint::new(60, 50)), Side::OnTheLeft);
    assert_eq!(l1.side_of(&IntPoint::new(40, 50)), Side::OnTheRight);
    assert_eq!(l1.side_of(&IntPoint::new(50, 50)), Side::Collinear);
}

#[test]
fn test_float_line_segment_projection() {
    let fl1 = FloatLine::new(FloatPoint::new(0.0, 0.0), FloatPoint::new(100.0, 0.0));
    let fl2 = FloatLine::new(FloatPoint::new(20.0, 10.0), FloatPoint::new(80.0, 10.0));

    let proj = fl1.segment_projection(&fl2).unwrap();
    assert!((proj.a.x - 20.0).abs() < 1e-6);
    assert!((proj.b.x - 80.0).abs() < 1e-6);
    assert!((proj.a.y - 0.0).abs() < 1e-6);
    assert!((proj.b.y - 0.0).abs() < 1e-6);
}
