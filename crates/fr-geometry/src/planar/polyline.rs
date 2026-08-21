//! Open polyline made of line segments.
//!
//! Ported from `app.freerouting.geometry.planar.Polyline`.

use crate::planar::{
    direction::Direction, float_point::FloatPoint, int_box::IntBox, int_octagon::IntOctagon,
    int_point::IntPoint, line::Line, line_segment::LineSegment, point::Point, polygon::Polygon,
    side::Side, simplex::Simplex, vector::Vector,
};

/// A sequence of lines where no 2 consecutive lines are parallel.
/// A Polyline of `n` lines defines `n - 1` corner points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Polyline {
    pub arr: Vec<Line>,
}

impl Polyline {
    /// Creates a polyline from a polygon.
    pub fn new_from_polygon(polygon: &Polygon) -> Self {
        let point_arr = polygon.corner_array();
        if point_arr.len() < 2 {
            return Polyline { arr: Vec::new() };
        }
        let mut arr = Vec::with_capacity(point_arr.len() + 1);

        let dir_start = Direction::get_instance(&point_arr[0], &point_arr[1]);
        let pt0 = match &point_arr[0] {
            Point::Int(ip) => *ip,
            Point::Rational(_) => point_arr[0].to_float().round(),
        };
        arr.push(Line::new_from_point_and_dir(pt0, &dir_start.turn_45_degree(2)));

        for i in 1..point_arr.len() {
            let p_prev = match &point_arr[i - 1] {
                Point::Int(ip) => *ip,
                Point::Rational(_) => point_arr[i - 1].to_float().round(),
            };
            let p_curr = match &point_arr[i] {
                Point::Int(ip) => *ip,
                Point::Rational(_) => point_arr[i].to_float().round(),
            };
            arr.push(Line::new(p_prev, p_curr));
        }

        let last_idx = point_arr.len() - 1;
        let dir_end = Direction::get_instance(&point_arr[last_idx], &point_arr[last_idx - 1]);
        let pt_last = match &point_arr[last_idx] {
            Point::Int(ip) => *ip,
            Point::Rational(_) => point_arr[last_idx].to_float().round(),
        };
        arr.push(Line::new_from_point_and_dir(pt_last, &dir_end.turn_45_degree(2)));

        Polyline { arr }
    }

    /// Creates a polyline from a slice of points.
    pub fn new_from_points(points: &[Point]) -> Self {
        Polyline::new_from_polygon(&Polygon::new(points))
    }

    /// Creates a polyline from an array of IntPoints.
    pub fn from_int_points(points: &[IntPoint]) -> Self {
        let pts: Vec<Point> = points.iter().map(|p| Point::Int(*p)).collect();
        Polyline::new_from_points(&pts)
    }

    /// Creates a polyline connecting two corners.
    pub fn new_from_two_points(from_corner: &Point, to_corner: &Point) -> Self {
        if from_corner == to_corner {
            return Polyline { arr: Vec::new() };
        }
        let p_from = match from_corner {
            Point::Int(ip) => *ip,
            Point::Rational(_) => from_corner.to_float().round(),
        };
        let p_to = match to_corner {
            Point::Int(ip) => *ip,
            Point::Rational(_) => to_corner.to_float().round(),
        };
        let dir = Direction::get_instance(from_corner, to_corner);
        let l0 = Line::new_from_point_and_dir(p_from, &dir.turn_45_degree(2));
        let l1 = Line::new(p_from, p_to);
        let l2 = Line::new_from_point_and_dir(p_to, &dir.turn_45_degree(2));
        Polyline {
            arr: vec![l0, l1, l2],
        }
    }

    /// Creates a polyline from a slice of lines, normalizing overlaps and directions.
    pub fn new_from_lines(line_arr: &[Line]) -> Self {
        let lines_no_parallel = Self::remove_consecutive_parallel_lines(line_arr);
        let mut lines = Self::remove_overlaps(&lines_no_parallel);
        if lines.len() < 3 {
            return Polyline { arr: Vec::new() };
        }

        // Adjust directions so lines point from previous corner to next corner
        for i in 1..lines.len() - 1 {
            let float_corner = lines[i].intersection_approx(&lines[i + 1]);
            let side_of_line = lines[i - 1].side_of_float(&float_corner);
            if side_of_line != Side::Collinear {
                let d0 = lines[i - 1].direction();
                let d1 = lines[i].direction();
                let side1 = d0.side_of(&d1);
                if side1 != side_of_line {
                    lines[i] = lines[i].opposite();
                }
            }
        }
        Polyline { arr: lines }
    }

    fn remove_consecutive_parallel_lines(line_arr: &[Line]) -> Vec<Line> {
        if line_arr.len() < 3 {
            return line_arr.to_vec();
        }
        let mut tmp = Vec::with_capacity(line_arr.len());
        tmp.push(line_arr[0]);
        for line in &line_arr[1..] {
            if !tmp.last().unwrap().is_parallel(line) {
                tmp.push(*line);
            }
        }
        if tmp.len() < 3 {
            Vec::new()
        } else {
            tmp
        }
    }

    fn remove_overlaps(line_arr: &[Line]) -> Vec<Line> {
        if line_arr.len() < 4 {
            return line_arr.to_vec();
        }
        let mut tmp = Vec::with_capacity(line_arr.len());
        if !line_arr[0].is_equal_or_opposite(&line_arr[2]) {
            tmp.push(line_arr[0]);
        }
        tmp.push(line_arr[1]);

        for i in 2..line_arr.len() - 2 {
            if !tmp.is_empty() && tmp.last().unwrap().is_equal_or_opposite(&line_arr[i + 1]) {
                tmp.pop();
            } else {
                tmp.push(line_arr[i]);
            }
        }
        tmp.push(line_arr[line_arr.len() - 2]);

        if tmp.len() >= 2
            && !line_arr[line_arr.len() - 1].is_equal_or_opposite(&tmp[tmp.len() - 2])
        {
            tmp.push(line_arr[line_arr.len() - 1]);
        }

        if tmp.len() < 3 {
            Vec::new()
        } else {
            tmp
        }
    }

    /// Number of corners: `arr.len() - 1`.
    pub fn corner_count(&self) -> usize {
        self.arr.len().saturating_sub(1)
    }

    /// Returns true if empty.
    pub fn is_empty(&self) -> bool {
        self.arr.len() < 3
    }

    /// Returns true if polyline is reduced to a single point.
    pub fn is_point(&self) -> bool {
        if self.arr.len() < 3 {
            return true;
        }
        let first = self.corner(0);
        for i in 1..self.corner_count() {
            if self.corner(i) != first {
                return false;
            }
        }
        true
    }

    /// First corner point.
    pub fn first_corner(&self) -> Point {
        self.corner(0)
    }

    /// Last corner point.
    pub fn last_corner(&self) -> Point {
        self.corner(self.arr.len() - 2)
    }

    /// `no`-th corner of the polyline.
    pub fn corner(&self, corner_index: usize) -> Point {
        if self.arr.len() < 2 {
            return Point::Int(IntPoint::ZERO);
        }
        let no = corner_index.min(self.arr.len() - 2);
        self.arr[no].intersection(&self.arr[no + 1])
    }

    /// Approximation of the `no`-th corner as `FloatPoint`.
    pub fn corner_approx(&self, corner_index: usize) -> FloatPoint {
        if self.arr.len() < 2 {
            return FloatPoint::ZERO;
        }
        let no = corner_index.min(self.arr.len() - 2);
        self.arr[no].intersection_approx(&self.arr[no + 1])
    }

    /// Array of all corner points.
    pub fn corner_arr(&self) -> Vec<Point> {
        (0..self.corner_count()).map(|i| self.corner(i)).collect()
    }

    /// Array of all corner approximations.
    pub fn corner_approx_arr(&self) -> Vec<FloatPoint> {
        (0..self.corner_count())
            .map(|i| self.corner_approx(i))
            .collect()
    }

    /// Cumulative length between `from_corner` and `to_corner`.
    pub fn length(&self, from_corner: usize, to_corner: usize) -> f64 {
        let from = from_corner.min(self.corner_count().saturating_sub(1));
        let to = to_corner.min(self.corner_count().saturating_sub(1));
        let mut result = 0.0;
        for i in from..to {
            result += self.corner_approx(i + 1).distance(&self.corner_approx(i));
        }
        result
    }

    /// Total length of the polyline.
    pub fn length_total(&self) -> f64 {
        if self.corner_count() < 2 {
            0.0
        } else {
            self.length(0, self.corner_count() - 1)
        }
    }

    /// Calculates offset shape around the polyline with `half_width`.
    pub fn offset_shapes(&self, half_width: i32) -> Vec<Simplex> {
        if self.arr.len() < 3 {
            return Vec::new();
        }
        self.offset_shapes_range(half_width, 0, self.arr.len() - 1)
    }

    /// Calculates offset shapes for lines in range `[from_no, to_no]`.
    pub fn offset_shapes_range(
        &self,
        half_width: i32,
        requested_from_no: usize,
        requested_to_no: usize,
    ) -> Vec<Simplex> {
        let from_no = requested_from_no.min(self.arr.len() - 1);
        let to_no = requested_to_no.min(self.arr.len() - 1);
        if to_no <= from_no + 1 {
            return Vec::new();
        }

        let shape_count = to_no - from_no - 1;
        let mut shape_arr = Vec::with_capacity(shape_count);

        let mut prev_dir = self.arr[from_no].direction().get_vector();
        let mut curr_dir = self.arr[from_no + 1].direction().get_vector();

        for i in (from_no + 1)..to_no {
            let next_dir = self.arr[i + 1].direction().get_vector();
            let mut lines = [Line::new_from_coords(0, 0, 0, 0); 4];

            lines[0] = self.arr[i].translate(-(half_width as f64));
            let next_dir_from_curr_dir = next_dir.side_of(&curr_dir);
            if next_dir_from_curr_dir == Side::OnTheLeft {
                lines[1] = self.arr[i + 1].translate(-(half_width as f64));
            } else {
                lines[1] = self.arr[i + 1].opposite().translate(-(half_width as f64));
            }
            lines[2] = self.arr[i].opposite().translate(-(half_width as f64));

            let curr_dir_from_prev_dir = curr_dir.side_of(&prev_dir);
            if curr_dir_from_prev_dir == Side::OnTheLeft {
                lines[3] = self.arr[i - 1].translate(-(half_width as f64));
            } else {
                lines[3] = self.arr[i - 1].opposite().translate(-(half_width as f64));
            }

            let mut cut_dog_ear_lines: Vec<Line> = Vec::new();

            // Cut off outstanding corners with following shapes
            let check_distance_corner = self.corner_approx(i);
            let check_dist_square = 2.0 * (half_width as f64) * (half_width as f64);
            let mut curr_line = lines[1];
            let check_line = if next_dir_from_curr_dir == Side::OnTheLeft {
                lines[2]
            } else {
                lines[0]
            };
            let mut tmp_curr_dir = next_dir.clone();
            let mut direction_changed = false;
            let mut corner_to_check = curr_line.intersection_approx(&check_line);

            for j in (i + 2)..self.arr.len().saturating_sub(1) {
                if self.corner_approx(j - 1).distance_square(&check_distance_corner)
                    > check_dist_square
                {
                    break;
                }
                if !direction_changed {
                    corner_to_check = curr_line.intersection_approx(&check_line);
                }
                let tmp_next_dir = self.arr[j].direction().get_vector();
                let tmp_next_from_tmp_curr = tmp_next_dir.side_of(&tmp_curr_dir);
                direction_changed = tmp_next_from_tmp_curr != next_dir_from_curr_dir;
                if !direction_changed {
                    let next_border_line = if tmp_next_from_tmp_curr == Side::OnTheLeft {
                        self.arr[j].translate(-(half_width as f64))
                    } else {
                        self.arr[j].opposite().translate(-(half_width as f64))
                    };
                    if next_border_line.side_of_float(&corner_to_check) == Side::OnTheLeft
                        && next_border_line.side_of_point(&self.corner(i)) == Side::OnTheRight
                        && next_border_line.side_of_point(&self.corner(i - 1)) == Side::OnTheRight
                    {
                        cut_dog_ear_lines.push(next_border_line);
                    }
                    tmp_curr_dir = tmp_next_dir;
                    curr_line = next_border_line;
                }
            }

            // Cut off outstanding corners with previous shapes
            let check_distance_corner_prev = self.corner_approx(i - 1);
            let check_line_prev = if curr_dir_from_prev_dir == Side::OnTheLeft {
                lines[2]
            } else {
                lines[0]
            };
            curr_line = lines[3];
            tmp_curr_dir = prev_dir.clone();
            direction_changed = false;

            if i >= 2 {
                for j in (1..=i - 2).rev() {
                    if self.corner_approx(j).distance_square(&check_distance_corner_prev)
                        > check_dist_square
                    {
                        break;
                    }
                    if !direction_changed {
                        corner_to_check = curr_line.intersection_approx(&check_line_prev);
                    }
                    let tmp_prev_dir = self.arr[j].direction().get_vector();
                    let tmp_curr_from_tmp_prev = tmp_curr_dir.side_of(&tmp_prev_dir);
                    direction_changed = tmp_curr_from_tmp_prev != curr_dir_from_prev_dir;
                    if !direction_changed {
                        let prev_border_line = if tmp_curr_dir.side_of(&tmp_prev_dir) == Side::OnTheLeft {
                            self.arr[j].translate(-(half_width as f64))
                        } else {
                            self.arr[j].opposite().translate(-(half_width as f64))
                        };
                        if prev_border_line.side_of_float(&corner_to_check) == Side::OnTheLeft
                            && prev_border_line.side_of_point(&self.corner(i)) == Side::OnTheRight
                            && prev_border_line.side_of_point(&self.corner(i - 1)) == Side::OnTheRight
                        {
                            cut_dog_ear_lines.push(prev_border_line);
                        }
                        tmp_curr_dir = tmp_prev_dir;
                        curr_line = prev_border_line;
                    }
                }
            }

            let mut s1 = Simplex::get_instance(&lines);
            if !cut_dog_ear_lines.is_empty() {
                s1 = s1.intersection(&Simplex::get_instance(&cut_dog_ear_lines));
            }

            let surr_oct = self.bounding_octagon_range(i - 1, i);
            let bounding_shape = surr_oct.offset(half_width as f64).to_simplex();
            let final_shape = bounding_shape.intersection(&s1);
            shape_arr.push(final_shape);

            prev_dir = curr_dir;
            curr_dir = next_dir;
        }

        shape_arr
    }

    /// Calculates the offset shape for segment `no`.
    pub fn offset_shape(&self, half_width: i32, no: usize) -> Option<Simplex> {
        if no > self.arr.len().saturating_sub(3) {
            return None;
        }
        let shapes = self.offset_shapes_range(half_width, no, no + 2);
        shapes.into_iter().next()
    }

    /// Offset box for line segment `no`.
    pub fn offset_box(&self, half_width: i32, no: usize) -> IntBox {
        let seg = LineSegment::from_polyline(self, no + 1).unwrap();
        seg.bounding_box().offset(half_width as f64)
    }

    /// Translates polyline by `vector`.
    pub fn translate_by(&self, vector: &Vector) -> Polyline {
        if vector.is_zero() {
            return self.clone();
        }
        let new_arr: Vec<Line> = self.arr.iter().map(|l| l.translate_by(vector)).collect();
        Polyline::new_from_lines(&new_arr)
    }

    /// Turns polyline by `factor` * 90° around `pole`.
    pub fn turn_90_degree(&self, factor: i32, pole: &IntPoint) -> Polyline {
        let new_arr: Vec<Line> = self
            .arr
            .iter()
            .map(|l| l.turn_90_degree(factor, pole))
            .collect();
        Polyline::new_from_lines(&new_arr)
    }

    /// Rotates polyline approximation by `angle` around `pole`.
    pub fn rotate_approx(&self, angle: f64, pole: &FloatPoint) -> Polyline {
        if angle == 0.0 {
            return self.clone();
        }
        let new_corners: Vec<Point> = (0..self.corner_count())
            .map(|i| Point::Int(self.corner_approx(i).rotate(angle, pole).round()))
            .collect();
        Polyline::new_from_points(&new_corners)
    }

    /// Mirrors polyline at vertical line through `pole`.
    pub fn mirror_vertical(&self, pole: &IntPoint) -> Polyline {
        let new_arr: Vec<Line> = self.arr.iter().map(|l| l.mirror_vertical(pole)).collect();
        Polyline::new_from_lines(&new_arr)
    }

    /// Mirrors polyline at horizontal line through `pole`.
    pub fn mirror_horizontal(&self, pole: &IntPoint) -> Polyline {
        let new_arr: Vec<Line> = self.arr.iter().map(|l| l.mirror_horizontal(pole)).collect();
        Polyline::new_from_lines(&new_arr)
    }

    /// Bounding box of corners in range `[from, to]`.
    pub fn bounding_box_range(&self, from: usize, to: usize) -> IntBox {
        let from_idx = from.min(self.corner_count().saturating_sub(1));
        let to_idx = to.min(self.corner_count().saturating_sub(1));
        let mut llx = f64::MAX;
        let mut lly = f64::MAX;
        let mut urx = f64::MIN;
        let mut ury = f64::MIN;
        for i in from_idx..=to_idx {
            let c = self.corner_approx(i);
            llx = llx.min(c.x);
            lly = lly.min(c.y);
            urx = urx.max(c.x);
            ury = ury.max(c.y);
        }
        IntBox::new(
            llx.floor() as i32,
            lly.floor() as i32,
            urx.ceil() as i32,
            ury.ceil() as i32,
        )
    }

    /// Bounding box of the entire polyline.
    pub fn bounding_box(&self) -> IntBox {
        if self.corner_count() == 0 {
            IntBox::EMPTY
        } else {
            self.bounding_box_range(0, self.corner_count() - 1)
        }
    }

    /// Bounding octagon of corners in range `[from, to]`.
    pub fn bounding_octagon_range(&self, from: usize, to: usize) -> IntOctagon {
        let from_idx = from.min(self.corner_count().saturating_sub(1));
        let to_idx = to.min(self.corner_count().saturating_sub(1));
        let mut lx = f64::MAX;
        let mut ly = f64::MAX;
        let mut rx = f64::MIN;
        let mut uy = f64::MIN;
        let mut ulx = f64::MAX;
        let mut lrx = f64::MIN;
        let mut llx = f64::MAX;
        let mut urx = f64::MIN;
        for i in from_idx..=to_idx {
            let c = self.corner_approx(i);
            lx = lx.min(c.x);
            ly = ly.min(c.y);
            rx = rx.max(c.x);
            uy = uy.max(c.y);
            let tmp = c.x - c.y;
            ulx = ulx.min(tmp);
            lrx = lrx.max(tmp);
            let tmp2 = c.x + c.y;
            llx = llx.min(tmp2);
            urx = urx.max(tmp2);
        }
        IntOctagon::new(
            lx.floor() as i32,
            ly.floor() as i32,
            rx.ceil() as i32,
            uy.ceil() as i32,
            ulx.floor() as i32,
            lrx.ceil() as i32,
            llx.floor() as i32,
            urx.ceil() as i32,
        )
    }

    /// Bounding octagon of the entire polyline.
    pub fn bounding_octagon(&self) -> IntOctagon {
        if self.corner_count() == 0 {
            IntOctagon::EMPTY
        } else {
            self.bounding_octagon_range(0, self.corner_count() - 1)
        }
    }

    /// Nearest point on this polyline to `from_point`.
    pub fn nearest_point_approx(&self, from_point: &FloatPoint) -> Option<FloatPoint> {
        let corners = self.corner_approx_arr();
        if corners.is_empty() {
            return None;
        }
        let mut min_distance = f64::MAX;
        let mut nearest = corners[0];

        for c in &corners {
            let d = c.distance(from_point);
            if d < min_distance {
                min_distance = d;
                nearest = *c;
            }
        }

        let ctolerance = 1.0;
        for i in 1..self.arr.len() - 1 {
            let projection = from_point.projection_approx(&self.arr[i]);
            let curr_distance = projection.distance(from_point);
            if curr_distance < min_distance {
                let segment_length = corners[i].distance(&corners[i - 1]);
                if projection.distance(&corners[i]) + projection.distance(&corners[i - 1])
                    < segment_length + ctolerance
                {
                    min_distance = curr_distance;
                    nearest = projection;
                }
            }
        }
        Some(nearest)
    }

    /// Distance from `from_point` to this polyline.
    pub fn distance(&self, from_point: &FloatPoint) -> f64 {
        if let Some(p) = self.nearest_point_approx(from_point) {
            from_point.distance(&p)
        } else {
            f64::MAX
        }
    }

    /// Combines two polylines if they share an end corner.
    pub fn combine(&self, other: &Polyline) -> Polyline {
        if other.arr.len() < 3 || self.arr.len() < 3 {
            return self.clone();
        }
        let combine_at_start;
        let combine_other_at_start;
        if self.first_corner() == other.first_corner() {
            combine_at_start = true;
            combine_other_at_start = true;
        } else if self.first_corner() == other.last_corner() {
            combine_at_start = true;
            combine_other_at_start = false;
        } else if self.last_corner() == other.first_corner() {
            combine_at_start = false;
            combine_other_at_start = true;
        } else if self.last_corner() == other.last_corner() {
            combine_at_start = false;
            combine_other_at_start = false;
        } else {
            return self.clone();
        }

        let mut line_arr = Vec::with_capacity(self.arr.len() + other.arr.len() - 2);
        if combine_at_start {
            if combine_other_at_start {
                for i in 0..other.arr.len() - 1 {
                    line_arr.push(other.arr[other.arr.len() - 1 - i].opposite());
                }
            } else {
                line_arr.extend_from_slice(&other.arr[0..other.arr.len() - 1]);
            }
            line_arr.extend_from_slice(&self.arr[1..]);
        } else {
            line_arr.extend_from_slice(&self.arr[0..self.arr.len() - 1]);
            if combine_other_at_start {
                line_arr.extend_from_slice(&other.arr[1..]);
            } else {
                for i in 1..other.arr.len() {
                    line_arr.push(other.arr[other.arr.len() - 1 - i].opposite());
                }
            }
        }

        Polyline::new_from_lines(&line_arr)
    }

    /// Splits polyline at `line_no` using `end_line`.
    pub fn split(&self, line_no: usize, end_line: &Line) -> Option<[Polyline; 2]> {
        if line_no < 1 || line_no > self.arr.len().saturating_sub(2) {
            return None;
        }
        if self.arr[line_no].is_parallel(end_line) {
            return None;
        }
        let new_end_corner = self.arr[line_no].intersection(end_line);
        if (line_no == 1 && new_end_corner == self.first_corner())
            || (line_no >= self.arr.len() - 2 && new_end_corner == self.last_corner())
        {
            return None;
        }

        let mut first_piece = Vec::new();
        if self.corner(line_no - 1) == new_end_corner {
            first_piece.extend_from_slice(&self.arr[0..=line_no]);
        } else {
            first_piece.extend_from_slice(&self.arr[0..=line_no]);
            first_piece.push(*end_line);
        }

        let mut second_piece = Vec::new();
        if self.corner(line_no) == new_end_corner {
            second_piece.extend_from_slice(&self.arr[line_no..]);
        } else {
            second_piece.push(*end_line);
            second_piece.extend_from_slice(&self.arr[line_no..]);
        }

        let p1 = Polyline::new_from_lines(&first_piece);
        let p2 = Polyline::new_from_lines(&second_piece);
        if p1.is_point() || p2.is_point() {
            return None;
        }
        Some([p1, p2])
    }

    /// Creates a new polyline by skipping lines in range `[from_no, to_no]`.
    pub fn skip_lines(&self, from_no: usize, to_no: usize) -> Polyline {
        if from_no > to_no || to_no >= self.arr.len() {
            return self.clone();
        }
        let mut new_lines = Vec::new();
        new_lines.extend_from_slice(&self.arr[0..from_no]);
        new_lines.extend_from_slice(&self.arr[to_no + 1..]);
        Polyline::new_from_lines(&new_lines)
    }

    /// Returns true if `point` is on this polyline.
    pub fn contains(&self, point: &Point) -> bool {
        for i in 1..self.arr.len() - 1 {
            if let Some(seg) = LineSegment::from_polyline(self, i) {
                if seg.contains(point) {
                    return true;
                }
            }
        }
        false
    }

    /// Perpendicular line segment from `point` onto nearest segment.
    pub fn projection_line(&self, point: &Point) -> Option<LineSegment> {
        let from_point = point.to_float();
        let mut min_distance = f64::MAX;
        let mut result_line = None;
        let mut nearest_line = None;

        for i in 1..self.arr.len() - 1 {
            let projection = from_point.projection_approx(&self.arr[i]);
            let curr_distance = projection.distance(&from_point);
            if curr_distance < min_distance {
                if let Some(dir) = self.arr[i].perpendicular_direction(point) {
                    let curr_result_line = Line::new_from_point_and_dir(
                        match point {
                            Point::Int(ip) => *ip,
                            Point::Rational(_) => from_point.round(),
                        },
                        &dir,
                    );
                    let prev_corner = self.corner(i - 1);
                    let next_corner = self.corner(i);
                    let prev_side = curr_result_line.side_of_point(&prev_corner);
                    let next_side = curr_result_line.side_of_point(&next_corner);
                    if prev_side == next_side && prev_side != Side::Collinear {
                        continue;
                    }
                    nearest_line = Some(self.arr[i]);
                    min_distance = curr_distance;
                    result_line = Some(curr_result_line);
                }
            }
        }

        if let (Some(nl), Some(rl)) = (nearest_line, result_line) {
            let p_int = match point {
                Point::Int(ip) => *ip,
                Point::Rational(_) => from_point.round(),
            };
            let start_line = Line::new_from_point_and_dir(p_int, &nl.direction());
            Some(LineSegment::new(start_line, rl, nl))
        } else {
            None
        }
    }

    /// Shortens this polyline to `new_line_count` lines with `last_segment_length`.
    pub fn shorten(&self, new_line_count: usize, last_segment_length: f64) -> Polyline {
        if new_line_count < 3 || new_line_count > self.arr.len() {
            return self.clone();
        }
        let last_corner = self.corner_approx(new_line_count - 2);
        let prev_last_corner = self.corner_approx(new_line_count - 3);
        let new_last_corner = prev_last_corner
            .change_length(&last_corner, last_segment_length)
            .round();

        if new_last_corner == self.corner(self.corner_count().saturating_sub(2)).to_float().round() {
            return self.skip_lines(new_line_count - 1, new_line_count - 1);
        }

        let mut new_lines = Vec::with_capacity(new_line_count);
        new_lines.extend_from_slice(&self.arr[0..new_line_count - 2]);

        let first_line_point = if self.arr[new_line_count - 2].a == new_last_corner {
            self.arr[new_line_count - 2].b
        } else {
            self.arr[new_line_count - 2].a
        };

        let new_prev_last_line = Line::new(first_line_point, new_last_corner);
        let last_line = Line::new_from_point_and_dir(
            new_last_corner,
            &new_prev_last_line.direction().turn_45_degree(6),
        );
        new_lines.push(new_prev_last_line);
        new_lines.push(last_line);

        Polyline::new_from_lines(&new_lines)
    }
}
