//! Convex shape defined as the intersection of half-planes.
//!
//! Ported from `app.freerouting.geometry.planar.Simplex`.

use crate::planar::{
    area::Area, convex_shape::ConvexShape, direction::Direction, float_point::FloatPoint,
    int_box::IntBox, int_octagon::IntOctagon, int_point::IntPoint, line::Line,
    line_segment::LineSegment, point::Point, polyline::Polyline, shape::Shape, side::Side,
    vector::Vector,
};

/// Convex shape defined as the intersection of half-planes.
/// Each half-plane is the positive/right side of a directed line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Simplex {
    pub arr: Vec<Line>,
}

impl Simplex {
    /// Standard empty simplex.
    pub const EMPTY: Simplex = Simplex { arr: Vec::new() };

    /// Constructs a Simplex from directed lines without normalizing.
    pub const fn new(arr: Vec<Line>) -> Self {
        Simplex { arr }
    }

    /// Creates a normalized Simplex from directed lines.
    pub fn get_instance(line_arr: &[Line]) -> Self {
        if line_arr.is_empty() {
            return Simplex::EMPTY;
        }
        let mut curr_arr = line_arr.to_vec();
        curr_arr.sort();
        let s = Simplex::new(curr_arr);
        s.remove_redundant_lines()
    }

    /// Returns true if this simplex is empty.
    pub fn is_empty(&self) -> bool {
        self.arr.is_empty()
    }

    /// Returns the number of border lines.
    pub fn border_line_count(&self) -> usize {
        self.arr.len()
    }

    /// Returns the `no`-th border line.
    pub fn border_line(&self, no: usize) -> Line {
        self.arr[no]
    }

    /// Returns true if corner `corner_index` is bounded.
    pub fn corner_is_bounded(&self, corner_index: usize) -> bool {
        if self.arr.len() <= 1 {
            return false;
        }
        let no = corner_index % self.arr.len();
        let prev_no = if no == 0 {
            self.arr.len() - 1
        } else {
            no - 1
        };
        let prev_dir = self.arr[prev_no].direction().get_int_vector();
        let curr_dir = self.arr[no].direction().get_int_vector();
        prev_dir.determinant(&curr_dir) > 0
    }

    /// Returns true if the shape of this simplex is bounded.
    pub fn is_bounded(&self) -> bool {
        if self.arr.is_empty() {
            return true;
        }
        if self.arr.len() < 3 {
            return false;
        }
        for i in 0..self.arr.len() {
            if !self.corner_is_bounded(i) {
                return false;
            }
        }
        true
    }

    /// Returns the `no`-th corner of this simplex.
    pub fn corner(&self, corner_index: usize) -> Point {
        if self.arr.is_empty() {
            return Point::Int(IntPoint::ZERO);
        }
        let no = corner_index % self.arr.len();
        let prev = if no == 0 {
            self.arr[self.arr.len() - 1]
        } else {
            self.arr[no - 1]
        };
        self.arr[no].intersection(&prev)
    }

    /// Approximation of the `no`-th corner as `FloatPoint`.
    pub fn corner_approx(&self, corner_index: usize) -> FloatPoint {
        if self.arr.is_empty() {
            return FloatPoint::ZERO;
        }
        let no = corner_index % self.arr.len();
        let prev = if no == 0 {
            self.arr[self.arr.len() - 1]
        } else {
            self.arr[no - 1]
        };
        self.arr[no].intersection_approx(&prev)
    }

    /// Returns all corner approximations.
    pub fn corner_approx_arr(&self) -> Vec<FloatPoint> {
        (0..self.arr.len())
            .map(|i| self.corner_approx(i))
            .collect()
    }

    /// Dimension: 2 (2D), 1 (segment/line), 0 (point), -1 (empty).
    pub fn dimension(&self) -> i32 {
        if self.arr.is_empty() {
            return -1;
        }
        if self.arr.len() > 4 || self.arr.len() == 1 {
            return 2;
        }
        if self.arr.len() == 2 {
            if self.arr[0].overlaps(&self.arr[1]) {
                return 1;
            }
            return 2;
        }
        if self.arr.len() == 3 {
            if self.arr[0].overlaps(&self.arr[1])
                || self.arr[0].overlaps(&self.arr[2])
                || self.arr[1].overlaps(&self.arr[2])
            {
                return 1;
            }
            let is = self.arr[1].intersection(&self.arr[2]);
            let side0 = self.arr[0].side_of_point(&is);
            if side0 == Side::OnTheRight {
                return 2;
            }
            if side0 == Side::OnTheLeft {
                return -1;
            }
            return 0;
        }
        // 4 lines
        let col02 = self.arr[0].overlaps(&self.arr[2]);
        let col13 = self.arr[1].overlaps(&self.arr[3]);
        if col02 && col13 {
            0
        } else if col02 || col13 {
            1
        } else {
            2
        }
    }

    /// Checks if this simplex can be converted to an `IntBox`.
    pub fn is_int_box(&self) -> bool {
        for i in 0..self.arr.len() {
            if !self.arr[i].is_orthogonal() || !self.corner_is_bounded(i) {
                return false;
            }
        }
        true
    }

    /// Checks if this simplex can be converted to an `IntOctagon`.
    pub fn is_int_octagon(&self) -> bool {
        for i in 0..self.arr.len() {
            if !self.arr[i].is_multiple_of_45_degree() || !self.corner_is_bounded(i) {
                return false;
            }
        }
        true
    }

    /// Converts this simplex to an `IntOctagon` if all lines are multiples of 45°.
    pub fn to_int_octagon(&self) -> Option<IntOctagon> {
        if !self.is_int_octagon() {
            return None;
        }
        if self.is_empty() {
            return Some(IntOctagon::EMPTY);
        }

        let mut rx = i32::MAX;
        let mut uy = i32::MAX;
        let mut lrx = i32::MAX;
        let mut urx = i32::MAX;
        let mut lx = i32::MIN;
        let mut ly = i32::MIN;
        let mut llx = i32::MIN;
        let mut ulx = i32::MIN;

        for line in &self.arr {
            let a = line.a;
            let b = line.b;
            if a.y == b.y {
                if b.x >= a.x {
                    ly = a.y;
                }
                if b.x <= a.x {
                    uy = a.y;
                }
            }
            if a.x == b.x {
                if b.y >= a.y {
                    rx = a.x;
                }
                if b.y <= a.y {
                    lx = a.x;
                }
            }
            if a.y < b.y {
                if a.x < b.x {
                    lrx = a.x - a.y;
                } else if a.x > b.x {
                    urx = a.x + a.y;
                }
            } else if a.y > b.y {
                if a.x < b.x {
                    llx = a.x + a.y;
                } else if a.x > b.x {
                    ulx = a.x - a.y;
                }
            }
        }
        Some(IntOctagon::new(lx, ly, rx, uy, ulx, lrx, llx, urx).normalize())
    }

    /// Translates this simplex by `vector`.
    pub fn translate_by(&self, vector: &Vector) -> Simplex {
        if vector.is_zero() {
            return self.clone();
        }
        let new_arr: Vec<Line> = self.arr.iter().map(|l| l.translate_by(vector)).collect();
        Simplex::new(new_arr)
    }

    /// Turns this simplex by `factor` * 90° around `pole`.
    pub fn turn_90_degree(&self, factor: i32, pole: &IntPoint) -> Simplex {
        let new_arr: Vec<Line> = self
            .arr
            .iter()
            .map(|l| l.turn_90_degree(factor, pole))
            .collect();
        Simplex::get_instance(&new_arr)
    }

    /// Mirrors this simplex at the vertical line through `pole`.
    pub fn mirror_vertical(&self, pole: &IntPoint) -> Simplex {
        let new_arr: Vec<Line> = self.arr.iter().map(|l| l.mirror_vertical(pole)).collect();
        Simplex::get_instance(&new_arr)
    }

    /// Mirrors this simplex at the horizontal line through `pole`.
    pub fn mirror_horizontal(&self, pole: &IntPoint) -> Simplex {
        let new_arr: Vec<Line> = self.arr.iter().map(|l| l.mirror_horizontal(pole)).collect();
        Simplex::get_instance(&new_arr)
    }

    /// Smallest box containing all corners of this simplex.
    pub fn bounding_box(&self) -> IntBox {
        if self.arr.is_empty() {
            return IntBox::EMPTY;
        }
        let mut llx = f64::MAX;
        let mut lly = f64::MAX;
        let mut urx = f64::MIN;
        let mut ury = f64::MIN;
        for i in 0..self.arr.len() {
            let curr = self.corner_approx(i);
            llx = llx.min(curr.x);
            lly = lly.min(curr.y);
            urx = urx.max(curr.x);
            ury = ury.max(curr.y);
        }
        IntBox::new(
            llx.floor() as i32,
            lly.floor() as i32,
            urx.ceil() as i32,
            ury.ceil() as i32,
        )
    }

    /// Smallest octagon containing all corners of this simplex.
    pub fn bounding_octagon(&self) -> Option<IntOctagon> {
        if self.arr.is_empty() {
            return Some(IntOctagon::EMPTY);
        }
        let mut lx = f64::MAX;
        let mut ly = f64::MAX;
        let mut rx = f64::MIN;
        let mut uy = f64::MIN;
        let mut ulx = f64::MAX;
        let mut lrx = f64::MIN;
        let mut llx = f64::MAX;
        let mut urx = f64::MIN;
        for i in 0..self.arr.len() {
            let curr = self.corner_approx(i);
            lx = lx.min(curr.x);
            ly = ly.min(curr.y);
            rx = rx.max(curr.x);
            uy = uy.max(curr.y);

            let tmp = curr.x - curr.y;
            ulx = ulx.min(tmp);
            lrx = lrx.max(tmp);

            let tmp = curr.x + curr.y;
            llx = llx.min(tmp);
            urx = urx.max(tmp);
        }
        Some(IntOctagon::new(
            lx.floor() as i32,
            ly.floor() as i32,
            rx.ceil() as i32,
            uy.ceil() as i32,
            ulx.floor() as i32,
            lrx.ceil() as i32,
            llx.floor() as i32,
            urx.ceil() as i32,
        ))
    }

    /// Offsets the boundary of the simplex by `width`. Positive = expand outward.
    pub fn offset(&self, width: f64) -> Simplex {
        if width == 0.0 {
            return self.clone();
        }
        let new_arr: Vec<Line> = self.arr.iter().map(|l| l.translate(-width)).collect();
        let offset_simplex = Simplex::new(new_arr);
        if width < 0.0 {
            offset_simplex.remove_redundant_lines()
        } else {
            offset_simplex
        }
    }

    /// Enlarges the simplex by `offset` and clips against enlarged bounding octagon.
    pub fn enlarge(&self, offset: f64) -> Simplex {
        if offset == 0.0 {
            return self.clone();
        }
        let offset_simplex = self.offset(offset);
        if let Some(bounding_oct) = self.bounding_octagon() {
            let offset_oct = bounding_oct.offset(offset);
            offset_simplex.intersection(&offset_oct.to_simplex())
        } else {
            Simplex::EMPTY
        }
    }

    /// Shrinks the simplex by `offset`.
    pub fn shrink(&self, offset: f64) -> Simplex {
        let result = self.offset(-offset);
        if result.is_empty() {
            let centre_box = self.centre_of_gravity().round().surrounding_box();
            self.intersection(&centre_box.to_simplex())
        } else {
            result
        }
    }

    /// Intersection of two simplices.
    pub fn intersection(&self, other: &Simplex) -> Simplex {
        if self.is_empty() || other.is_empty() {
            return Simplex::EMPTY;
        }
        let mut new_arr = Vec::with_capacity(self.arr.len() + other.arr.len());
        new_arr.extend_from_slice(&self.arr);
        new_arr.extend_from_slice(&other.arr);
        new_arr.sort();
        let result = Simplex::new(new_arr);
        result.remove_redundant_lines()
    }

    /// Checks if this simplex intersects `other`.
    pub fn intersects(&self, other: &Simplex) -> bool {
        !self.intersection(other).is_empty()
    }

    /// Checks if this simplex intersects an `IntBox`.
    pub fn intersects_box(&self, box_: &IntBox) -> bool {
        self.intersects(&box_.to_simplex())
    }

    /// Checks if this simplex intersects an `IntOctagon`.
    pub fn intersects_octagon(&self, oct: &IntOctagon) -> bool {
        self.intersects(&oct.to_simplex())
    }

    /// Edge number if `line` is a border line.
    pub fn border_line_index(&self, line: &Line) -> Option<usize> {
        self.arr.iter().position(|l| l == line)
    }

    /// Removes border line `no`.
    pub fn remove_border_line(&self, no: usize) -> Simplex {
        if no >= self.arr.len() {
            return self.clone();
        }
        let mut new_arr = self.arr.clone();
        new_arr.remove(no);
        Simplex::new(new_arr)
    }

    /// Removes redundant border lines. Assumes `self.arr` is sorted.
    pub fn remove_redundant_lines(&self) -> Simplex {
        if self.arr.is_empty() {
            return Simplex::EMPTY;
        }
        let mut line_arr: Vec<Line> = Vec::with_capacity(self.arr.len());
        line_arr.push(self.arr[0]);
        for line in &self.arr[1..] {
            if !line.fast_equals(line_arr.last().unwrap()) {
                line_arr.push(*line);
            }
        }

        let mut new_length = line_arr.len();
        let mut intersection_sides: Vec<Option<Side>> = vec![None; new_length];

        let mut try_again = new_length > 2;
        let mut index_of_last_removed_line = new_length;

        while try_again {
            try_again = false;
            let mut ind = 0;
            while ind < new_length {
                let prev_ind = if ind == 0 { new_length - 1 } else { ind - 1 };
                let next_ind = if ind == new_length - 1 { 0 } else { ind + 1 };

                let prev_line = line_arr[prev_ind];
                let curr_line = line_arr[ind];
                let next_line = line_arr[next_ind];

                let prev_dir = prev_line.direction().get_int_vector();
                let next_dir = next_line.direction().get_int_vector();
                let det = prev_dir.determinant(&next_dir);

                let mut remove_line = false;
                if det != 0 {
                    if intersection_sides[ind].is_none() {
                        intersection_sides[ind] =
                            Some(curr_line.side_of_intersection(&prev_line, &next_line));
                    }
                    let side = intersection_sides[ind].unwrap();
                    if det > 0 {
                        remove_line = side != Side::OnTheLeft;
                    } else if side == Side::OnTheLeft {
                        let curr_dir = curr_line.direction().get_int_vector();
                        if prev_dir.determinant(&curr_dir) > 0 {
                            new_length = 0;
                            try_again = false;
                            break;
                        }
                    }
                } else if prev_line.side_of(&next_line.a) == Side::OnTheLeft {
                    new_length = 0;
                    try_again = false;
                    break;
                }

                if remove_line {
                    try_again = true;
                    new_length -= 1;
                    line_arr.remove(ind);
                    intersection_sides.remove(ind);

                    if new_length < 3 {
                        try_again = false;
                        break;
                    }
                    let p_ind = if ind == 0 { new_length - 1 } else { ind - 1 };
                    intersection_sides[p_ind] = None;
                    let n_ind = if ind >= new_length { 0 } else { ind };
                    intersection_sides[n_ind] = None;

                    if ind > 0 {
                        ind -= 1;
                    }
                    index_of_last_removed_line = ind;
                } else {
                    ind += 1;
                }

                if !try_again && ind >= index_of_last_removed_line {
                    break;
                }
            }
        }

        if new_length == 2 && line_arr[0].is_parallel(&line_arr[1]) {
            if line_arr[0].direction() == line_arr[1].direction() {
                if line_arr[1].side_of(&line_arr[0].a) == Side::OnTheLeft {
                    line_arr[0] = line_arr[1];
                }
                new_length = 1;
            } else if line_arr[1].side_of(&line_arr[0].a) == Side::OnTheLeft {
                new_length = 0;
            }
        }

        if new_length == 0 {
            Simplex::EMPTY
        } else {
            line_arr.truncate(new_length);
            Simplex::new(line_arr)
        }
    }

    /// Calculates division lines for `inner_corner_no` relative to `outer_simplex`.
    fn calc_division_lines(
        &self,
        inner_corner_no: usize,
        outer_simplex: &Simplex,
    ) -> Option<Vec<Line>> {
        let curr_inner_line = self.arr[inner_corner_no];
        let prev_inner_line = if inner_corner_no != 0 {
            self.arr[inner_corner_no - 1]
        } else {
            self.arr[self.arr.len() - 1]
        };
        let intersection = curr_inner_line.intersection_approx(&prev_inner_line);
        if intersection.x >= i32::MAX as f64 {
            return None;
        }
        let inner_corner = intersection.round();
        let ctolerance = 0.0001;
        let is_exact = (inner_corner.x as f64 - intersection.x).abs() < ctolerance
            && (inner_corner.y as f64 - intersection.y).abs() < ctolerance;

        if !is_exact {
            return Some(vec![prev_inner_line]);
        }

        let mut first_projection_dir = Direction::NULL;
        let mut second_projection_dir = Direction::NULL;
        let prev_inner_dir = prev_inner_line.direction().opposite().get_int_vector();
        let next_inner_dir = curr_inner_line.direction().get_int_vector();
        let mut outer_line_no = 0;
        let mut min_distance = f64::MAX;

        for _ in 0..outer_simplex.arr.len() {
            let outer_line = outer_simplex.arr[outer_line_no];
            let pt = Point::Int(inner_corner);
            let curr_proj_dir_opt = outer_line.perpendicular_direction(&pt);
            if curr_proj_dir_opt.is_none() {
                return Some(vec![Line::new(inner_corner, inner_corner)]);
            }
            let curr_projection_dir = curr_proj_dir_opt.unwrap();
            let curr_proj_vec = curr_projection_dir.get_int_vector();
            let projection_visible = prev_inner_dir.determinant(&curr_proj_vec) >= 0;

            if projection_visible {
                let mut curr_distance = outer_line.signed_distance(&inner_corner.to_float()).abs();
                let second_division_necessary = curr_proj_vec.determinant(&next_inner_dir) < 0;
                let mut curr_second_projection_dir = curr_projection_dir;

                if second_division_necessary {
                    let mut second_projection_visible = false;
                    let mut tmp_outer_line_no = outer_line_no;
                    while !second_projection_visible {
                        if tmp_outer_line_no == outer_simplex.arr.len() - 1 {
                            tmp_outer_line_no = 0;
                        } else {
                            tmp_outer_line_no += 1;
                        }
                        let next_outer = outer_simplex.arr[tmp_outer_line_no];
                        let next_proj_opt = next_outer.perpendicular_direction(&pt);
                        if next_proj_opt.is_none() {
                            return Some(vec![Line::new(inner_corner, inner_corner)]);
                        }
                        curr_second_projection_dir = next_proj_opt.unwrap();
                        let sec_proj_vec = curr_second_projection_dir.get_int_vector();
                        if curr_proj_vec.determinant(&sec_proj_vec) < 0 {
                            curr_distance = f64::MAX;
                            break;
                        }
                        second_projection_visible = sec_proj_vec.determinant(&next_inner_dir) >= 0;
                    }
                    if curr_distance < f64::MAX {
                        curr_distance += outer_simplex.arr[tmp_outer_line_no]
                            .signed_distance(&inner_corner.to_float())
                            .abs();
                    }
                }

                if curr_distance < min_distance {
                    min_distance = curr_distance;
                    first_projection_dir = curr_projection_dir;
                    second_projection_dir = curr_second_projection_dir;
                }
            }

            if outer_line_no == outer_simplex.arr.len() - 1 {
                outer_line_no = 0;
            } else {
                outer_line_no += 1;
            }
        }

        if min_distance == f64::MAX {
            return None;
        }

        if first_projection_dir == second_projection_dir {
            Some(vec![Line::new_from_point_and_dir(
                inner_corner,
                &first_projection_dir,
            )])
        } else {
            Some(vec![
                Line::new_from_point_and_dir(inner_corner, &first_projection_dir),
                Line::new_from_point_and_dir(inner_corner, &second_projection_dir),
            ])
        }
    }

    /// Cuts this simplex out of `outer_simplex`, returning convex pieces.
    pub fn cutout_from(&self, outer_simplex: &Simplex) -> Vec<Simplex> {
        if self.dimension() < 2 {
            return vec![outer_simplex.clone()];
        }
        let inner_simplex = self.intersection(outer_simplex);
        if inner_simplex.dimension() < 2 {
            return vec![outer_simplex.clone()];
        }
        let inner_corner_count = inner_simplex.arr.len();
        let mut division_line_arr = Vec::with_capacity(inner_corner_count);
        for inner_corner_no in 0..inner_corner_count {
            match inner_simplex.calc_division_lines(inner_corner_no, outer_simplex) {
                Some(div_lines) => division_line_arr.push(div_lines),
                None => return vec![outer_simplex.clone()],
            }
        }

        let mut check_cross_first_line = false;
        let mut prev_division_line: Option<Line> = None;
        let first_division_line = division_line_arr[0][0];
        let first_direction = first_division_line.direction().get_int_vector();
        let mut result_list = Vec::new();

        for inner_corner_no in 0..inner_corner_count {
            let next_corner_no = (inner_corner_no + 1) % inner_corner_count;
            let next_division_line = division_line_arr[next_corner_no][0];
            let curr_division_lines = &division_line_arr[inner_corner_no];

            if curr_division_lines.len() == 2 {
                let curr_dir = curr_division_lines[0].direction().get_int_vector();
                let mut merge_prev_division_line = false;
                let mut merge_first_division_line = false;
                if let Some(prev_div) = prev_division_line {
                    let prev_dir = prev_div.direction().get_int_vector();
                    if curr_dir.determinant(&prev_dir) > 0 {
                        merge_prev_division_line = true;
                    }
                }
                if !check_cross_first_line {
                    check_cross_first_line =
                        inner_corner_no > 0 && curr_dir.determinant(&first_direction) > 0;
                }
                if check_cross_first_line {
                    let curr_dir2 = curr_division_lines[1].direction().get_int_vector();
                    if curr_dir2.determinant(&first_direction) < 0 {
                        merge_first_division_line = true;
                    }
                }
                let mut piece_lines = Vec::new();
                piece_lines.push(Line::new(
                    curr_division_lines[1].b,
                    curr_division_lines[1].a,
                ));
                piece_lines.push(curr_division_lines[0]);
                if merge_prev_division_line {
                    piece_lines.push(prev_division_line.unwrap());
                }
                if merge_first_division_line {
                    piece_lines.push(Line::new(first_division_line.b, first_division_line.a));
                }
                let curr_piece = Simplex::new(piece_lines);
                let is = curr_piece.intersection(outer_simplex);
                if !is.is_empty() && is.dimension() == 2 {
                    result_list.push(is);
                }
            }

            let merge_next_division_line = next_division_line.b != next_division_line.a;
            let last_curr_division_line = curr_division_lines[curr_division_lines.len() - 1];
            let last_curr_dir = last_curr_division_line.direction().get_int_vector();
            let merge_last_curr_division_line =
                last_curr_division_line.b != last_curr_division_line.a;
            let mut merge_prev_division_line = false;
            let mut merge_first_division_line = false;

            if let Some(prev_div) = prev_division_line {
                let prev_dir = prev_div.direction().get_int_vector();
                if last_curr_dir.determinant(&prev_dir) > 0 {
                    merge_prev_division_line = true;
                }
            }
            if !check_cross_first_line {
                check_cross_first_line = inner_corner_no > 0
                    && last_curr_dir.determinant(&first_direction) > 0
                    && (last_curr_dir.x as i64 * first_direction.x as i64
                        + last_curr_dir.y as i64 * first_direction.y as i64)
                        < 0;
            }
            if check_cross_first_line {
                let next_dir = next_division_line.direction().get_int_vector();
                if next_dir.determinant(&first_direction) < 0 {
                    merge_first_division_line = true;
                }
            }

            let mut piece_lines = Vec::new();
            let curr_line = inner_simplex.arr[inner_corner_no];
            piece_lines.push(Line::new(curr_line.b, curr_line.a));
            if merge_next_division_line {
                piece_lines.push(Line::new(next_division_line.b, next_division_line.a));
            }
            if merge_last_curr_division_line {
                piece_lines.push(last_curr_division_line);
            }
            if merge_prev_division_line {
                piece_lines.push(prev_division_line.unwrap());
            }
            if merge_first_division_line {
                piece_lines.push(Line::new(first_division_line.b, first_division_line.a));
            }
            let curr_piece = Simplex::new(piece_lines);
            let is = curr_piece.intersection(outer_simplex);
            if !is.is_empty() && is.dimension() == 2 {
                result_list.push(is);
            }
            prev_division_line = Some(next_division_line);
        }

        result_list
    }

    /// Cuts `inner` out of this simplex.
    pub fn cutout(&self, inner: &Simplex) -> Vec<Simplex> {
        inner.cutout_from(self)
    }

    /// Calculates area using surveyor's formula.
    pub fn area(&self) -> f64 {
        if !self.is_bounded() {
            return f64::MAX;
        }
        if self.dimension() < 2 {
            return 0.0;
        }
        let corner_count = self.arr.len();
        let mut result = 0.0;
        let mut prev_corner = self.corner_approx(corner_count - 2);
        let mut curr_corner = self.corner_approx(corner_count - 1);
        for i in 0..corner_count {
            let next_corner = self.corner_approx(i);
            result += curr_corner.x * (next_corner.y - prev_corner.y);
            prev_corner = curr_corner;
            curr_corner = next_corner;
        }
        0.5 * result.abs()
    }

    /// Circumference of the bounded simplex.
    pub fn circumference(&self) -> f64 {
        if !self.is_bounded() {
            return i32::MAX as f64;
        }
        let corner_count = self.arr.len();
        let mut result = 0.0;
        let mut prev_corner = self.corner_approx(corner_count - 1);
        for i in 0..corner_count {
            let curr_corner = self.corner_approx(i);
            result += curr_corner.distance(&prev_corner);
            prev_corner = curr_corner;
        }
        result
    }

    /// Center of gravity (centroid) of the simplex corners.
    pub fn centre_of_gravity(&self) -> FloatPoint {
        let corner_count = self.arr.len();
        if corner_count == 0 {
            return FloatPoint::ZERO;
        }
        let mut x = 0.0;
        let mut y = 0.0;
        for i in 0..corner_count {
            let p = self.corner_approx(i);
            x += p.x;
            y += p.y;
        }
        FloatPoint::new(x / corner_count as f64, y / corner_count as f64)
    }

    /// Returns true if `point` is outside this simplex.
    pub fn is_outside(&self, point: &Point) -> bool {
        if self.arr.is_empty() {
            return true;
        }
        for line in &self.arr {
            if line.side_of_point(point) == Side::OnTheLeft {
                return true;
            }
        }
        false
    }

    /// Returns true if `point` is inside or on the border.
    pub fn contains(&self, point: &Point) -> bool {
        !self.is_outside(point)
    }

    /// Returns true if `point` is strictly inside.
    pub fn contains_inside(&self, point: &Point) -> bool {
        if self.arr.is_empty() {
            return false;
        }
        for line in &self.arr {
            if line.side_of_point(point) != Side::OnTheRight {
                return false;
            }
        }
        true
    }

    /// Returns true if `point` is on the border.
    pub fn contains_on_border(&self, point: &Point) -> bool {
        if self.arr.is_empty() {
            return false;
        }
        let mut on_border = false;
        for line in &self.arr {
            let side = line.side_of_point(point);
            if side == Side::OnTheLeft {
                return false;
            }
            if side == Side::Collinear {
                on_border = true;
            }
        }
        on_border
    }

    /// Returns true if `point` is contained within `tolerance`.
    pub fn contains_float(&self, point: &FloatPoint, tolerance: f64) -> bool {
        if self.arr.is_empty() {
            return false;
        }
        for line in &self.arr {
            if line.side_of_float_tol(point, tolerance) != Side::OnTheRight {
                return false;
            }
        }
        true
    }

    /// Distance between `point` and nearest point on this simplex.
    pub fn distance(&self, point: &FloatPoint) -> f64 {
        let nearest = self.nearest_point_approx(point);
        nearest.distance(point)
    }

    /// Distance between `point` and nearest border point.
    pub fn border_distance(&self, point: &FloatPoint) -> f64 {
        if let Some(nearest) = self.nearest_border_point_approx(point) {
            nearest.distance(point)
        } else {
            0.0
        }
    }

    /// Smallest radius from center of gravity to border.
    pub fn smallest_radius(&self) -> f64 {
        self.border_distance(&self.centre_of_gravity())
    }

    /// Nearest point on this simplex to `from_point`.
    pub fn nearest_point_approx(&self, from_point: &FloatPoint) -> FloatPoint {
        if self.contains_float(from_point, 0.0) {
            return *from_point;
        }
        self.nearest_border_point_approx(from_point)
            .unwrap_or(*from_point)
    }

    /// Nearest border point approximation.
    pub fn nearest_border_point_approx(&self, from_point: &FloatPoint) -> Option<FloatPoint> {
        let pts = self.nearest_border_points_approx(from_point, 1);
        pts.into_iter().next()
    }

    /// Approximate `count` nearest points on the border to `from_point`.
    pub fn nearest_border_points_approx(
        &self,
        from_point: &FloatPoint,
        count: usize,
    ) -> Vec<FloatPoint> {
        if count == 0 || self.arr.is_empty() {
            return Vec::new();
        }
        let line_count = self.arr.len();
        if line_count == 1 {
            return vec![from_point.projection_approx(&self.arr[0])];
        }
        if self.dimension() == 0 {
            return vec![self.corner_approx(0)];
        }
        let result_count = count.min(line_count);
        let mut nearest_points: Vec<FloatPoint> = Vec::with_capacity(result_count);
        let mut min_dists: Vec<f64> = vec![f64::MAX; result_count];

        for i in 0..line_count {
            if self.corner_is_bounded(i) {
                let curr_corner = self.corner_approx(i);
                let curr_dist = curr_corner.distance_square(from_point);
                for j in 0..result_count {
                    if curr_dist < min_dists[j] {
                        min_dists.insert(j, curr_dist);
                        min_dists.truncate(result_count);
                        nearest_points.insert(j, curr_corner);
                        nearest_points.truncate(result_count);
                        break;
                    }
                }
            }
        }

        let mut prev_ind = line_count - 2;
        let mut curr_ind = line_count - 1;

        for next_ind in 0..line_count {
            let projection = from_point.projection_approx(&self.arr[curr_ind]);
            let left_ok = !self.corner_is_bounded(curr_ind)
                || self.arr[prev_ind].side_of_float(&projection) == Side::OnTheRight;
            let right_ok = !self.corner_is_bounded(next_ind)
                || self.arr[next_ind].side_of_float(&projection) == Side::OnTheRight;

            if left_ok && right_ok {
                let curr_dist = projection.distance_square(from_point);
                for j in 0..result_count {
                    if curr_dist < min_dists[j] {
                        min_dists.insert(j, curr_dist);
                        min_dists.truncate(result_count);
                        nearest_points.insert(j, projection);
                        nearest_points.truncate(result_count);
                        break;
                    }
                }
            }
            prev_ind = curr_ind;
            curr_ind = next_ind;
        }

        nearest_points
    }

    /// Entrance points where `polyline` crosses the boundary of this simplex.
    pub fn entrance_points(&self, polyline: &Polyline) -> Vec<(usize, usize)> {
        let mut result = Vec::new();
        let mut prev_line_no: Option<usize> = None;
        let mut prev_edge_no: Option<usize> = None;
        let corners: Vec<Point> = (0..self.arr.len()).map(|i| self.corner(i)).collect();
        let bbox = self.bounding_box();

        for line_no in 1..polyline.arr.len().saturating_sub(1) {
            if let Some(seg) = LineSegment::from_polyline(polyline, line_no) {
                let curr_intersections =
                    seg.border_intersections_lines(&self.arr, &corners, &bbox);
                for edge_no in curr_intersections {
                    if prev_line_no != Some(line_no) || prev_edge_no != Some(edge_no) {
                        result.push((line_no, edge_no));
                        prev_line_no = Some(line_no);
                        prev_edge_no = Some(edge_no);
                    }
                }
            }
        }
        result
    }

    /// Cuts out parts of `polyline` in the interior of this simplex.
    pub fn cutout_polyline(&self, polyline: &Polyline) -> Vec<Polyline> {
        let intersection_no = self.entrance_points(polyline);
        let first_corner = polyline.first_corner();
        let first_corner_inside = self.contains_inside(&first_corner);

        if intersection_no.is_empty() {
            if first_corner_inside {
                return Vec::new();
            }
            return vec![polyline.clone()];
        }

        let mut pieces = Vec::new();
        let mut curr_intersection_no = 0;
        let first_tuple = intersection_no[0];
        let first_intersection = polyline.arr[first_tuple.0].intersection(&self.arr[first_tuple.1]);

        if !first_corner_inside {
            if first_corner != first_intersection {
                let curr_polyline_no = first_tuple.0;
                let mut curr_lines = Vec::with_capacity(curr_polyline_no + 2);
                curr_lines.extend_from_slice(&polyline.arr[0..=curr_polyline_no]);
                curr_lines.push(self.arr[first_tuple.1]);
                let curr_piece = Polyline::new_from_lines(&curr_lines);
                if !curr_piece.is_empty() {
                    pieces.push(curr_piece);
                }
            }
            curr_intersection_no += 1;
        }

        while curr_intersection_no + 1 < intersection_no.len() {
            let curr_tuple = intersection_no[curr_intersection_no];
            let next_tuple = intersection_no[curr_intersection_no + 1];
            let curr_poly_no = curr_tuple.0;
            let next_poly_no = next_tuple.0;

            let mut insert_piece = false;
            for i in (curr_poly_no + 1)..next_poly_no {
                if self.is_outside(&polyline.corner(i)) {
                    insert_piece = true;
                    break;
                }
            }

            if insert_piece {
                let mut curr_lines = Vec::new();
                curr_lines.push(self.arr[curr_tuple.1]);
                curr_lines.extend_from_slice(&polyline.arr[curr_poly_no..next_poly_no]);
                curr_lines.push(self.arr[next_tuple.1]);
                let curr_piece = Polyline::new_from_lines(&curr_lines);
                if !curr_piece.is_empty() {
                    pieces.push(curr_piece);
                }
            }
            curr_intersection_no += 2;
        }

        if curr_intersection_no < intersection_no.len() {
            let curr_tuple = intersection_no[curr_intersection_no];
            let curr_poly_no = curr_tuple.0;
            let mut curr_lines = Vec::new();
            curr_lines.push(self.arr[curr_tuple.1]);
            curr_lines.extend_from_slice(&polyline.arr[curr_poly_no..]);
            let curr_piece = Polyline::new_from_lines(&curr_lines);
            if !curr_piece.is_empty() {
                pieces.push(curr_piece);
            }
        }

        pieces
    }

    /// Divides this simplex into sections of about equal size with width/height <= `max_section_width`.
    pub fn divide_into_sections(&self, max_section_width: f64) -> Vec<Simplex> {
        if self.is_empty() {
            return vec![self.clone()];
        }
        let section_boxes = self.bounding_box().divide_into_sections(max_section_width);
        let mut result = Vec::new();
        for b in section_boxes {
            let section = self.intersection(&b.to_simplex());
            if section.dimension() == 2 {
                result.push(section);
            }
        }
        result
    }
}

impl Area for Simplex {
    fn is_empty(&self) -> bool {
        self.is_empty()
    }

    fn is_bounded(&self) -> bool {
        self.is_bounded()
    }

    fn dimension(&self) -> i32 {
        self.dimension()
    }

    fn is_contained_in(&self, box_: &IntBox) -> bool {
        box_.contains_box(&self.bounding_box())
    }

    fn bounding_box(&self) -> IntBox {
        self.bounding_box()
    }

    fn bounding_octagon(&self) -> IntOctagon {
        self.bounding_octagon().unwrap_or(IntOctagon::EMPTY)
    }

    fn contains_point(&self, point: &Point) -> bool {
        self.contains(point)
    }

    fn contains_float_point(&self, point: &FloatPoint) -> bool {
        self.contains_float(point, 0.0)
    }

    fn nearest_point_approx(&self, from_point: &FloatPoint) -> FloatPoint {
        self.nearest_point_approx(from_point)
    }

    fn corner_approx_arr(&self) -> Vec<FloatPoint> {
        self.corner_approx_arr()
    }
}

impl Shape for Simplex {
    fn circumference(&self) -> f64 {
        self.circumference()
    }

    fn area(&self) -> f64 {
        self.area()
    }

    fn centre_of_gravity(&self) -> FloatPoint {
        self.centre_of_gravity()
    }

    fn is_outside(&self, point: &Point) -> bool {
        self.is_outside(point)
    }

    fn contains_inside(&self, point: &Point) -> bool {
        self.contains_inside(point)
    }

    fn contains_on_border(&self, point: &Point) -> bool {
        self.contains_on_border(point)
    }

    fn distance(&self, point: &FloatPoint) -> f64 {
        self.distance(point)
    }

    fn border_distance(&self, point: &FloatPoint) -> f64 {
        self.border_distance(point)
    }

    fn smallest_radius(&self) -> f64 {
        self.smallest_radius()
    }
}

impl ConvexShape for Simplex {
    fn max_width(&self) -> f64 {
        if !self.is_bounded() {
            return i32::MAX as f64;
        }
        let gravity_point = self.centre_of_gravity();
        let mut max_dist = f64::MIN;
        let mut max_dist2 = f64::MIN;
        for line in &self.arr {
            let d = line.signed_distance(&gravity_point).abs();
            if d > max_dist {
                max_dist2 = max_dist;
                max_dist = d;
            } else if d > max_dist2 {
                max_dist2 = d;
            }
        }
        max_dist + max_dist2
    }

    fn min_width(&self) -> f64 {
        if !self.is_bounded() {
            return i32::MAX as f64;
        }
        let gravity_point = self.centre_of_gravity();
        let mut min_dist = f64::MAX;
        let mut min_dist2 = f64::MAX;
        for line in &self.arr {
            let d = line.signed_distance(&gravity_point).abs();
            if d < min_dist {
                min_dist2 = min_dist;
                min_dist = d;
            } else if d < min_dist2 {
                min_dist2 = d;
            }
        }
        min_dist + min_dist2
    }
}
