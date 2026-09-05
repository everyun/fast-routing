//! `board/CalcFromSide.java` (159 L) — which border edge of a tile shape the
//! shove comes from, plus an approximate point on that edge.
//!
//! All four upstream constructors are here, including the release's
//! nearest-side fallback in constructor 3 (spec §13.1, conformance table 3:
//! *present* in 2.3.0 under the v19 class name, with the `0.01` box tolerance
//! and without v19's warning). The dead `border_intersection = projection`
//! write inside that fallback loop is dropped, as the conformance appendix
//! prescribes.

use fr_geometry::planar::{FloatLine, FloatPoint, LineSegment, Point, Polyline, Side, Simplex};

use super::geom;

/// `CalcFromSide`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FromSide {
    /// Border-line index of the shape, negative when not calculated.
    pub no: i32,
    /// Approximate point on that border line.
    pub border_intersection: Option<FloatPoint>,
}

impl FromSide {
    /// `CalcFromSide.NOT_CALCULATED`.
    pub const NOT_CALCULATED: FromSide = FromSide {
        no: -1,
        border_intersection: None,
    };

    /// Constructor 4 — values already calculated.
    pub fn raw(no: i32, border_intersection: Option<FloatPoint>) -> FromSide {
        FromSide {
            no,
            border_intersection,
        }
    }

    /// Constructor 1 (`:22-61`): the edge of `shape` where `polyline` enters,
    /// scanning segments backwards from `no`.
    ///
    /// `no` is expected between 1 and `polyline.arr.len() - 2` inclusive.
    pub fn from_polyline(polyline: &Polyline, no: usize, shape: &Simplex) -> FromSide {
        let mut fromside_no = -1i32;
        let mut intersection = None;
        let corners: Vec<Point> = (0..shape.border_line_count())
            .map(|i| shape.corner(i))
            .collect();
        let bounding_box = shape.bounding_box();

        let mut curr_no = no;
        while curr_no > 0 {
            if let Some(segment) = LineSegment::from_polyline(polyline, curr_no) {
                let intersections =
                    segment.border_intersections_lines(&shape.arr, &corners, &bounding_box);
                if let Some(&border_no) = intersections.first() {
                    fromside_no = border_no as i32;
                    intersection = Some(
                        segment
                            .get_line()
                            .intersection_approx(&shape.border_line(border_no)),
                    );
                    break;
                }
            }
            curr_no -= 1;
        }

        if intersection.is_none() && fromside_no < 0 {
            // The first corner of the polyline is inside the shape: take the
            // border line whose intersection with `arr[1]` is nearest to it.
            let from_point = polyline.corner_approx(0);
            if polyline.arr.len() > 1 {
                let check_line = polyline.arr[1];
                let mut min_dist = f64::MAX;
                for i in 0..shape.border_line_count() {
                    let curr_intersection = check_line.intersection_approx(&shape.border_line(i));
                    let curr_dist = curr_intersection.distance(&from_point).abs();
                    if curr_dist < min_dist {
                        fromside_no = i as i32;
                        intersection = Some(curr_intersection);
                        min_dist = curr_dist;
                    }
                }
            }
        }

        FromSide {
            no: fromside_no,
            border_intersection: intersection,
        }
    }

    /// Constructor 2 (`:64-71`): the nearest border side of `shape` to
    /// `from_point` — the shove direction when translating a drill item.
    pub fn from_point(from_point: &Point, shape: &Simplex) -> FromSide {
        let Some(border_projection) = geom::nearest_border_point(shape, from_point) else {
            return FromSide::NOT_CALCULATED;
        };
        let no = geom::contains_on_border_line_no(shape, &border_projection)
            .map(|no| no as i32)
            .unwrap_or(-1);
        FromSide {
            no,
            border_intersection: Some(geom::point_to_float(&border_projection)),
        }
    }

    /// Constructor 3 (`:77-149`) — the maze-time one: the side of `shape` at the
    /// start of `line_segment`, rotated by ±2 so that shoving left and right
    /// walk around opposite sides of the trace shape.
    pub fn from_line_segment(
        line_segment: &LineSegment,
        shape: &Simplex,
        shove_to_the_left: bool,
    ) -> FromSide {
        let start_corner = line_segment.start_point_approx();
        let end_corner = line_segment.end_point_approx();
        let border_line_count = shape.border_line_count();
        if border_line_count == 0 {
            return FromSide::NOT_CALCULATED;
        }
        let check_line = line_segment.get_line();
        let first_corner = shape.corner_approx(0);
        let mut prev_side = check_line.side_of_float(&first_corner);
        let mut front_side_no = -1i32;

        for i in 1..=border_line_count {
            let next_corner = if i == border_line_count {
                first_corner
            } else {
                shape.corner_approx(i)
            };
            let next_side = check_line.side_of_float(&next_corner);
            if prev_side != next_side {
                let curr_intersection = shape.border_line(i - 1).intersection_approx(&check_line);
                if curr_intersection.distance_square(&start_corner)
                    < curr_intersection.distance_square(&end_corner)
                {
                    front_side_no = (i - 1) as i32;
                    break;
                }
            }
            prev_side = next_side;
        }

        let base_side = if front_side_no < 0 {
            // The release fallback (`:106-142`, spec §13.1): nearest side by
            // perpendicular projection, accepting a projection inside the
            // side's corner box with tolerance 0.01. `nearest_side` starts at
            // 0 and is never validated upstream, so a shape where nothing
            // projects in still yields side 0 — kept, it is load bearing for
            // parity.
            let mut min_distance = f64::MAX;
            let mut nearest_side = 0usize;
            for i in 0..border_line_count {
                let line = shape.border_line(i);
                let border_line = FloatLine::new(line.a.to_float(), line.b.to_float());
                let projection = border_line.perpendicular_projection(&start_corner);
                let side_start = shape.corner_approx(i);
                let side_end = shape.corner_approx((i + 1) % border_line_count);
                if projection.is_contained_in_box(&side_start, &side_end, 0.01) {
                    let distance = start_corner.distance(&projection);
                    if distance < min_distance {
                        min_distance = distance;
                        nearest_side = i;
                    }
                }
            }
            nearest_side as i32
        } else {
            front_side_no
        };

        let count = border_line_count as i32;
        let no = if shove_to_the_left {
            (base_side + 2) % count
        } else {
            (base_side + count - 2) % count
        };
        let prev_corner = shape.corner_approx(no as usize);
        let next_corner = shape.corner_approx(((no + 1) % count) as usize);
        FromSide {
            no,
            border_intersection: Some(prev_corner.middle_point(&next_corner)),
        }
    }
}

/// `Side` is compared by value in constructor 3; this keeps the import honest.
const _: fn() -> Side = || Side::Collinear;

#[cfg(test)]
mod tests {
    use super::*;
    use fr_geometry::planar::{IntBox, IntPoint};

    fn square() -> Simplex {
        IntBox::new(0, 0, 100, 100).to_simplex()
    }

    #[test]
    fn the_left_and_right_from_sides_are_two_edges_apart() {
        let shape = square();
        let segment = LineSegment::from_points(
            &Point::Int(IntPoint::new(-50, 50)),
            &Point::Int(IntPoint::new(150, 50)),
        );
        let left = FromSide::from_line_segment(&segment, &shape, true);
        let right = FromSide::from_line_segment(&segment, &shape, false);
        assert!(left.no >= 0 && right.no >= 0);
        let count = shape.border_line_count() as i32;
        assert_eq!((left.no - right.no).rem_euclid(count), 4 % count);
    }

    #[test]
    fn a_point_from_side_lands_on_a_real_border_line() {
        let shape = square();
        let from = FromSide::from_point(&Point::Int(IntPoint::new(50, 50)), &shape);
        assert!(from.no >= 0, "an interior point projects onto a border");
        assert!(from.border_intersection.is_some());
    }

    #[test]
    fn a_segment_that_misses_the_shape_still_yields_a_side_through_the_fallback() {
        let shape = square();
        // Far away and parallel: no border line separates the corners, so the
        // front-side scan fails and the release fallback must answer.
        let segment = LineSegment::from_points(
            &Point::Int(IntPoint::new(1000, 1000)),
            &Point::Int(IntPoint::new(1000, 2000)),
        );
        let from = FromSide::from_line_segment(&segment, &shape, false);
        assert!(from.no >= 0);
        assert!((from.no as usize) < shape.border_line_count());
    }
}
