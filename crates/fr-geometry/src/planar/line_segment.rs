//! Directed line segment between two points, ported from
//! `app.freerouting.geometry.planar.LineSegment`.

use crate::planar::{
    direction::Direction, float_point::FloatPoint, int_box::IntBox, int_octagon::IntOctagon,
    int_point::IntPoint, int_vector::IntVector, line::Line, point::Point, polyline::Polyline,
    side::Side, simplex::Simplex,
};
use fr_datastructures::Signum;

/// A line segment defined by 3 lines: starts at intersection of `start` and `middle`,
/// ends at intersection of `middle` and `end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LineSegment {
    pub start: Line,
    pub middle: Line,
    pub end: Line,
}

impl LineSegment {
    /// Creates a line segment from 3 lines.
    pub const fn new(start: Line, middle: Line, end: Line) -> Self {
        LineSegment { start, middle, end }
    }

    /// Creates a line segment from 2 points.
    pub fn from_points(start_point: &Point, end_point: &Point) -> Self {
        let dir = Direction::get_instance(start_point, end_point);
        let middle = Line::new_from_point_and_dir(
            match start_point {
                Point::Int(ip) => *ip,
                Point::Rational(_) => start_point.to_float().round(),
            },
            &dir,
        );
        let start = Line::new_from_point_and_dir(
            match start_point {
                Point::Int(ip) => *ip,
                Point::Rational(_) => start_point.to_float().round(),
            },
            &dir.turn_45_degree(2),
        );
        let end = Line::new_from_point_and_dir(
            match end_point {
                Point::Int(ip) => *ip,
                Point::Rational(_) => end_point.to_float().round(),
            },
            &dir.turn_45_degree(2),
        );
        LineSegment { start, middle, end }
    }

    /// Creates the `no`-th line segment of a polyline (for `1 <= no <= arr.len() - 2`).
    pub fn from_polyline(polyline: &Polyline, no: usize) -> Option<Self> {
        if no == 0 || no >= polyline.arr.len().saturating_sub(1) {
            return None;
        }
        Some(LineSegment {
            start: polyline.arr[no - 1],
            middle: polyline.arr[no],
            end: polyline.arr[no + 1],
        })
    }

    /// Returns the intersection of `start` and `middle`.
    pub fn start_point(&self) -> Point {
        self.middle.intersection(&self.start)
    }

    /// Returns the intersection of `middle` and `end`.
    pub fn end_point(&self) -> Point {
        self.middle.intersection(&self.end)
    }

    /// Approximation of the start point as `FloatPoint`.
    pub fn start_point_approx(&self) -> FloatPoint {
        self.start.intersection_approx(&self.middle)
    }

    /// Approximation of the end point as `FloatPoint`.
    pub fn end_point_approx(&self) -> FloatPoint {
        self.end.intersection_approx(&self.middle)
    }

    /// Returns the middle line of this segment.
    pub fn get_line(&self) -> Line {
        self.middle
    }

    /// Returns the start closing line.
    pub fn get_start_closing_line(&self) -> Line {
        self.start
    }

    /// Returns the end closing line.
    pub fn get_end_closing_line(&self) -> Line {
        self.end
    }

    /// Returns the line segment with opposite direction.
    pub fn opposite(&self) -> LineSegment {
        LineSegment {
            start: self.end.opposite(),
            middle: self.middle.opposite(),
            end: self.start.opposite(),
        }
    }

    /// Transforms this LineSegment into a polyline of 3 lines.
    pub fn to_polyline(&self) -> Polyline {
        Polyline::new_from_lines(&[self.start, self.middle, self.end])
    }

    /// Creates a 1-dimensional Simplex from this line segment.
    pub fn to_simplex(&self) -> Simplex {
        let mut line_arr = [Line::new_from_coords(0, 0, 0, 0); 4];
        if self.end_point().side_of(&self.start) == Side::OnTheRight {
            line_arr[0] = self.start.opposite();
        } else {
            line_arr[0] = self.start;
        }
        line_arr[1] = self.middle;
        line_arr[2] = self.middle.opposite();
        if self.start_point().side_of(&self.end) == Side::OnTheRight {
            line_arr[3] = self.end.opposite();
        } else {
            line_arr[3] = self.end;
        }
        Simplex::get_instance(&line_arr)
    }

    /// Checks if `point` is contained in this line segment.
    pub fn contains(&self, point: &Point) -> bool {
        if self.middle.side_of_point(point) != Side::Collinear {
            return false;
        }
        let dir = self.middle.direction().turn_45_degree(2);
        let pt_int = match point {
            Point::Int(ip) => *ip,
            Point::Rational(_) => point.to_float().round(),
        };
        let perp_line = Line::new_from_point_and_dir(pt_int, &dir);
        let start_side = self.start_point().side_of(&perp_line);
        let end_side = self.end_point().side_of(&perp_line);
        start_side != end_side || start_side == Side::Collinear
    }

    /// Calculates the smallest surrounding box of this line segment.
    pub fn bounding_box(&self) -> IntBox {
        let start_corner = self.middle.intersection_approx(&self.start);
        let end_corner = self.middle.intersection_approx(&self.end);
        let llx = start_corner.x.min(end_corner.x);
        let lly = start_corner.y.min(end_corner.y);
        let urx = start_corner.x.max(end_corner.x);
        let ury = start_corner.y.max(end_corner.y);
        let lower_left = IntPoint::new(llx.floor() as i32, lly.floor() as i32);
        let upper_right = IntPoint::new(urx.ceil() as i32, ury.ceil() as i32);
        IntBox::new_from_points(lower_left, upper_right)
    }

    /// Calculates the smallest surrounding octagon of this line segment.
    pub fn bounding_octagon(&self) -> IntOctagon {
        let start_corner = self.middle.intersection_approx(&self.start);
        let end_corner = self.middle.intersection_approx(&self.end);
        let lx = start_corner.x.min(end_corner.x).floor() as i32;
        let ly = start_corner.y.min(end_corner.y).floor() as i32;
        let rx = start_corner.x.max(end_corner.x).ceil() as i32;
        let uy = start_corner.y.max(end_corner.y).ceil() as i32;
        let start_x_minus_y = start_corner.x - start_corner.y;
        let end_x_minus_y = end_corner.x - end_corner.y;
        let ulx = start_x_minus_y.min(end_x_minus_y).floor() as i32;
        let lrx = start_x_minus_y.max(end_x_minus_y).ceil() as i32;
        let start_x_plus_y = start_corner.x + start_corner.y;
        let end_x_plus_y = end_corner.x + end_corner.y;
        let llx = start_x_plus_y.min(end_x_plus_y).floor() as i32;
        let urx = start_x_plus_y.max(end_x_plus_y).ceil() as i32;
        IntOctagon::new(lx, ly, rx, uy, ulx, lrx, llx, urx).normalize()
    }

    /// Creates a new line segment with the same start and middle line,
    /// shortened or lengthened to approximately `new_length`.
    pub fn change_length_approx(&self, new_length: f64) -> LineSegment {
        let new_end_point = self
            .start_point_approx()
            .change_length(&self.end_point_approx(), new_length);
        let perp_dir = self.middle.direction().turn_45_degree(2);
        let new_end_line = Line::new_from_point_and_dir(new_end_point.round(), &perp_dir);
        LineSegment::new(self.start, self.middle, new_end_line)
    }

    /// Inverts direction if start_point has larger x, or equal x and larger y.
    pub fn sort_endpoints_in_xy(&self) -> LineSegment {
        let swap = self.start_point().compare_xy(&self.end_point()) > 0;
        if swap {
            LineSegment::new(self.end, self.middle, self.start)
        } else {
            *self
        }
    }

    /// Intersections of this line segment with `other`.
    pub fn intersection(&self, other: &LineSegment) -> Vec<Line> {
        if !self.bounding_box().intersects(&other.bounding_box()) {
            return Vec::new();
        }
        let start_point_side = self.start_point().side_of(&other.middle);
        let end_point_side = self.end_point().side_of(&other.middle);
        if start_point_side == Side::Collinear && end_point_side == Side::Collinear {
            let this_sorted = self.sort_endpoints_in_xy();
            let other_sorted = other.sort_endpoints_in_xy();
            let (left_line, right_line) =
                if this_sorted.start_point().compare_xy(&other_sorted.start_point()) <= 0 {
                    (this_sorted, other_sorted)
                } else {
                    (other_sorted, this_sorted)
                };
            let cmp = left_line.end_point().compare_xy(&right_line.start_point());
            if cmp < 0 {
                return Vec::new();
            }
            if cmp == 0 {
                return vec![left_line.end];
            }
            let mut result = vec![right_line.start];
            if right_line.end_point().compare_xy(&left_line.end_point()) >= 0 {
                result.push(left_line.end);
            } else {
                result.push(right_line.end);
            }
            return result;
        }
        if start_point_side == end_point_side
            || other.start_point().side_of(&self.middle) == other.end_point().side_of(&self.middle)
        {
            return Vec::new();
        }
        vec![other.middle]
    }

    /// Checks if this LineSegment and `other` contain a common point.
    pub fn intersects(&self, other: &LineSegment) -> bool {
        !self.intersection(other).is_empty()
    }

    /// Checks if this LineSegment and `other` overlap along a segment.
    pub fn overlaps(&self, other: &LineSegment) -> bool {
        self.intersection(other).len() > 1
    }

    /// Constructs orthogonal stair approximation with integer coordinates.
    pub fn stair_approximation(&self, width: f64, to_the_right: bool) -> Vec<IntPoint> {
        let start_point = self.start_point().to_float().round();
        let end_point = self.end_point().to_float().round();
        if start_point == end_point {
            return Vec::new();
        }
        if start_point.x == end_point.x || start_point.y == end_point.y {
            return vec![start_point, end_point];
        }

        let dx = end_point.x - start_point.x;
        let dy = end_point.y - start_point.y;
        let abs_dx = dx.abs();
        let abs_dy = dy.abs();
        let function_of_x = abs_dx >= abs_dy;

        let stair_width = if function_of_x {
            let mut sw = ((width * abs_dx as f64) / abs_dy as f64).round() as i32;
            if sw == 0 {
                sw = 1;
            }
            if end_point.x < start_point.x {
                -sw
            } else {
                sw
            }
        } else {
            let mut sw = ((width * abs_dy as f64) / abs_dx as f64).round() as i32;
            if sw == 0 {
                sw = 1;
            }
            if end_point.y < start_point.y {
                -sw
            } else {
                sw
            }
        };

        let stair_count = if function_of_x {
            (abs_dx - 1) / stair_width.abs() + 1
        } else {
            (abs_dy - 1) / stair_width.abs() + 1
        } as usize;

        let mut result = Vec::with_capacity(2 * stair_count + 1);
        result.push(start_point);
        let det = (dx as f64) * (dy as f64);
        let change_x_first = (to_the_right && det > 0.0) || (!to_the_right && det < 0.0);

        let mut prev_line_point_x = start_point.x;
        let mut prev_line_point_y = start_point.y;

        for i in 1..stair_count {
            let (curr_line_point_x, curr_line_point_y) = if function_of_x {
                let cx = start_point.x + (i as i32) * stair_width;
                let cy = self.get_line().function_value_approx(cx as f64).round() as i32;
                (cx, cy)
            } else {
                let cy = start_point.y + (i as i32) * stair_width;
                let cx = self.get_line().function_in_y_value_approx(cy as f64).round() as i32;
                (cx, cy)
            };
            if change_x_first {
                result.push(IntPoint::new(curr_line_point_x, prev_line_point_y));
            } else {
                result.push(IntPoint::new(prev_line_point_x, curr_line_point_y));
            }
            result.push(IntPoint::new(curr_line_point_x, curr_line_point_y));
            prev_line_point_x = curr_line_point_x;
            prev_line_point_y = curr_line_point_y;
        }

        if change_x_first {
            result.push(IntPoint::new(end_point.x, prev_line_point_y));
        } else {
            result.push(IntPoint::new(prev_line_point_x, end_point.y));
        }
        result.push(end_point);
        result
    }

    /// Constructs 45-degree stair approximation with integer coordinates.
    pub fn stair_approximation45(&self, width: f64, to_the_right: bool) -> Vec<IntPoint> {
        let start_point = self.start_point().to_float().round();
        let end_point = self.end_point().to_float().round();
        if start_point == end_point {
            return Vec::new();
        }
        let delta = end_point.difference_by_int(&start_point);
        if delta.is_multiple_of_45_degree() {
            return vec![start_point, end_point];
        }
        let abs_delta = IntVector::new(delta.x.abs(), delta.y.abs());
        let function_of_x = abs_delta.x >= abs_delta.y;
        let det = (delta.x as f64) * (delta.y as f64);

        let stair_width = if function_of_x {
            let mut sw = ((width * abs_delta.x as f64) / abs_delta.y as f64).round() as i32;
            if sw == 0 {
                sw = 1;
            }
            if end_point.x < start_point.x {
                -sw
            } else {
                sw
            }
        } else {
            let mut sw = ((width * abs_delta.y as f64) / abs_delta.x as f64).round() as i32;
            if sw == 0 {
                sw = 1;
            }
            if end_point.y < start_point.y {
                -sw
            } else {
                sw
            }
        };

        let stair_count = if function_of_x {
            (abs_delta.x - 1) / stair_width.abs() + 1
        } else {
            (abs_delta.y - 1) / stair_width.abs() + 1
        } as usize;

        let mut result = Vec::with_capacity(2 * stair_count + 1);
        result.push(start_point);
        let mut prev_line_point = start_point;

        for i in 1..=stair_count {
            let curr_line_point = if i == stair_count {
                end_point
            } else {
                let (cx, cy) = if function_of_x {
                    let x = start_point.x + (i as i32) * stair_width;
                    let y = self.get_line().function_value_approx(x as f64).round() as i32;
                    (x, y)
                } else {
                    let y = start_point.y + (i as i32) * stair_width;
                    let x = self.get_line().function_in_y_value_approx(y as f64).round() as i32;
                    (x, y)
                };
                IntPoint::new(cx, cy)
            };

            let sw_sign = Signum::as_int(stair_width as f64);
            let (mid_x, mid_y) = if function_of_x {
                let diagonal_first = (to_the_right && det < 0.0) || (!to_the_right && det > 0.0);
                if diagonal_first {
                    (
                        prev_line_point.x + sw_sign * (curr_line_point.y - prev_line_point.y).abs(),
                        curr_line_point.y,
                    )
                } else {
                    (
                        curr_line_point.x - sw_sign * (curr_line_point.y - prev_line_point.y).abs(),
                        prev_line_point.y,
                    )
                }
            } else {
                let diagonal_first = (to_the_right && det > 0.0) || (!to_the_right && det < 0.0);
                if diagonal_first {
                    (
                        curr_line_point.x,
                        prev_line_point.y + sw_sign * (curr_line_point.x - prev_line_point.x).abs(),
                    )
                } else {
                    (
                        prev_line_point.x,
                        curr_line_point.y - sw_sign * (curr_line_point.x - prev_line_point.x).abs(),
                    )
                }
            };
            result.push(IntPoint::new(mid_x, mid_y));
            result.push(curr_line_point);
            prev_line_point = curr_line_point;
        }

        result
    }

    /// Borderline numbers of a convex shape intersected by this line segment.
    pub fn border_intersections_lines(
        &self,
        border_lines: &[Line],
        corners: &[Point],
        bounding_box: &IntBox,
    ) -> Vec<usize> {
        if !self.bounding_box().intersects(bounding_box) {
            return Vec::new();
        }
        let edge_count = border_lines.len();
        if edge_count == 0 {
            return Vec::new();
        }

        let mut prev_line = border_lines[edge_count - 1];
        let mut curr_line = border_lines[0];
        let mut result = Vec::new();
        let mut intersections: Vec<Point> = Vec::new();
        let line_start = self.start_point();
        let line_end = self.end_point();

        for edge_line_no in 0..edge_count {
            let next_line = if edge_line_no == edge_count - 1 {
                border_lines[0]
            } else {
                border_lines[edge_line_no + 1]
            };

            let start_point_side = curr_line.side_of_point(&line_start);
            let end_point_side = curr_line.side_of_point(&line_end);

            if start_point_side == Side::OnTheLeft && end_point_side == Side::OnTheLeft {
                return Vec::new();
            }

            if start_point_side == Side::Collinear && end_point_side != Side::OnTheRight {
                return Vec::new();
            }
            if end_point_side == Side::Collinear && start_point_side != Side::OnTheRight {
                return Vec::new();
            }

            if start_point_side != Side::OnTheRight || end_point_side != Side::OnTheRight {
                let is = self.middle.intersection(&curr_line);
                let prev_line_side_of_is = prev_line.side_of_point(&is);
                let next_line_side_of_is = next_line.side_of_point(&is);

                if prev_line_side_of_is != Side::OnTheLeft && next_line_side_of_is != Side::OnTheLeft {
                    if prev_line_side_of_is == Side::Collinear {
                        let prev_prev_corner = if edge_line_no == 0 {
                            &corners[edge_count - 1]
                        } else {
                            &corners[edge_line_no - 1]
                        };
                        let next_corner = if edge_line_no == edge_count - 1 {
                            &corners[0]
                        } else {
                            &corners[edge_line_no + 1]
                        };
                        let prev_prev_side = self.middle.side_of_point(prev_prev_corner);
                        let next_side = self.middle.side_of_point(next_corner);
                        if prev_prev_side == Side::Collinear
                            || next_side == Side::Collinear
                            || prev_prev_side == next_side
                        {
                            return Vec::new();
                        }
                    }
                    if next_line_side_of_is == Side::Collinear {
                        let prev_corner = &corners[edge_line_no];
                        let next_next_corner = if edge_line_no == edge_count - 2 {
                            &corners[0]
                        } else if edge_line_no == edge_count - 1 {
                            &corners[1]
                        } else {
                            &corners[edge_line_no + 2]
                        };
                        let prev_corner_side = self.middle.side_of_point(prev_corner);
                        let next_next_side = self.middle.side_of_point(next_next_corner);
                        if prev_corner_side == Side::Collinear
                            || next_next_side == Side::Collinear
                            || prev_corner_side == next_next_side
                        {
                            return Vec::new();
                        }
                    }

                    if !intersections.contains(&is) {
                        result.push(edge_line_no);
                        intersections.push(is);
                    }
                }
            }

            prev_line = curr_line;
            curr_line = next_line;
        }

        if result.len() == 2 {
            let is0 = intersections[0].to_float();
            let is1 = intersections[1].to_float();
            let curr_start = line_start.to_float();
            if curr_start.distance_square(&is1) < curr_start.distance_square(&is0) {
                result.swap(0, 1);
            }
        }
        result
    }
}
