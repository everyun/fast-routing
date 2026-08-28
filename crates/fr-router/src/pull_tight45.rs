//! The 45° local optimization rules — `board/PullTightAlgo45.java` (622 L).
//!
//! Spec `docs/upstream-study/pull-tight-insert.md` §6.2, conformance appendix
//! "45° pipeline order" (which confirms the release runs exactly this order and
//! that `smoothen_corner` is tried before `smoothen_sharp_corner`).
//!
//! ```text
//! pull_tight: avoid_acid_traps once (dead), then
//!             loop { reduce_corners; smoothen_corners; reposition_lines }   (:27-37)
//! ```
//!
//! `avoid_acid_traps` is `if (true) return p_polyline;` in the release
//! (`PullTightAlgo.java:454-457`, conformance row "avoid_acid_traps dead"), so
//! it is *not* reproduced — there is nothing to reproduce.
//!
//! Fork/end smoothing (`smoothen_start/end_corner_at_trace`, `:415`/`:520`) is
//! not ported; see [`crate::pull_tight`]'s module docs.

use fr_datastructures::signum::Signum;
use fr_geometry::planar::{Direction, FloatPoint, IntPoint, Limits, Line, Point, Polyline, Side};

use crate::insert_forced::ChangedArea;
use crate::pull_tight::{octagon_is_outside, signum_as_int, PullTightAlgo, NUMERIC_BACKOFF};

impl PullTightAlgo<'_> {
    /// `PullTightAlgo45.pull_tight` (`:27-37`) — the fixed-point loop.
    ///
    /// Upstream's "fixed point by object identity" (`while (new_result !=
    /// prev_result)`) becomes an explicit per-round change flag, as spec §11
    /// prescribes: a round that produces no new polyline ends the loop.
    ///
    /// Returns `None` when the polyline was not changed at all — upstream's
    /// "returned the input object", which is what
    /// [`PullTightAlgo::trace_pull_tight`] branches on.
    pub fn pull_tight_polyline(
        &mut self,
        polyline: &Polyline,
        changed_area: &mut ChangedArea,
    ) -> Option<Polyline> {
        let mut curr = polyline.clone();
        let mut any_changed = false;
        while !self.is_stop_requested() && self.take_round() {
            fr_metrics::counters::bump::pull_tight_iterations(1);
            let mut round_changed = false;
            if let Some(next) = self.reduce_corners(&curr, changed_area) {
                curr = next;
                round_changed = true;
            }
            if let Some(next) = self.smoothen_corners(&curr, changed_area) {
                curr = next;
                round_changed = true;
            }
            if let Some(next) = self.reposition_lines(&curr, changed_area) {
                curr = next;
                round_changed = true;
            }
            if !round_changed {
                break;
            }
            any_changed = true;
        }
        // A move that collapsed the polyline below three lines is not a valid
        // trace; upstream's `new Polyline(...)` would have produced the same
        // empty array and `change` would have rejected it downstream.
        if !any_changed || curr.arr.len() < 3 {
            return None;
        }
        Some(curr)
    }

    /// `reduce_corners` (`:46-183`) — the staircase remover.
    ///
    /// Corner-array based, and it **bails out entirely** the moment a corner is
    /// not an `IntPoint` (`:52-56`, `:75-77`, `:84-86`): the whole rule is
    /// integer translation arithmetic. A sliding window of four corners tries,
    /// in upstream's order:
    ///
    /// 1. drop `corner[2]` outright when it is a duplicate of `corner[1]` or
    ///    collinear with `corner[1..3]` (`:78-90`);
    /// 2. translate the segment `corner[2] -> corner[1]` by `corner[3] -
    ///    corner[2]`; accept iff the new corner is collinear with
    ///    `(corner[0], corner[1])` — the two segments merge into one legal 45°
    ///    segment — and both check polylines clear (`:91-122`);
    /// 3. failing that, the mirrored translation toward `corner[0]`
    ///    (`:123-150`).
    ///
    /// Every corner touched must be inside the clip shape, checked per corner
    /// (`:58-66`), which is why the `in_clip_shape` flags travel with the window.
    pub(crate) fn reduce_corners(
        &mut self,
        polyline: &Polyline,
        changed_area: &mut ChangedArea,
    ) -> Option<Polyline> {
        if polyline.arr.len() <= 4 {
            return None;
        }
        let clip = self.clip_shape();
        // The whole rule is integer translation arithmetic; a rational corner
        // aborts it (`:52-56`).
        let mut curr_corner: [IntPoint; 4] = [IntPoint::ZERO; 4];
        for (i, slot) in curr_corner.iter_mut().enumerate() {
            let Point::Int(corner) = polyline.corner(i) else {
                return None;
            };
            *slot = corner;
        }
        let mut in_clip: [bool; 4] = [true; 4];
        for i in 0..4 {
            in_clip[i] = clip.is_none_or(|c| !octagon_is_outside(&c, &Point::Int(curr_corner[i])));
        }

        let mut polyline_changed = false;
        let mut new_corners: Vec<IntPoint> = Vec::with_capacity(polyline.arr.len());
        new_corners.push(curr_corner[0]);
        let mut new_corner = curr_corner[0];
        let mut corner_no = 3usize;

        while corner_no < polyline.arr.len() - 1 {
            let mut corner_removed = false;
            let Point::Int(corner3) = polyline.corner(corner_no) else {
                return None;
            };
            curr_corner[3] = corner3;
            // `:78-90` — corners in the middle of a straight run are skipped.
            if curr_corner[1] == curr_corner[2]
                || (corner_no < polyline.arr.len() - 2
                    && collinear(&curr_corner[3], &curr_corner[1], &curr_corner[2]))
            {
                corner_no += 1;
                curr_corner[2] = curr_corner[3];
                in_clip[2] = in_clip[3];
                if corner_no < polyline.arr.len() - 1 {
                    let Point::Int(next) = polyline.corner(corner_no) else {
                        return None;
                    };
                    curr_corner[3] = next;
                }
                polyline_changed = true;
            }
            in_clip[3] = clip.is_none_or(|c| !octagon_is_outside(&c, &Point::Int(curr_corner[3])));

            // `:91-122` — translate the segment corner[2]->corner[1] toward corner[3].
            if in_clip[1] && in_clip[2] && in_clip[3] {
                let delta = curr_corner[3].difference_by_int(&curr_corner[2]);
                new_corner = curr_corner[1].translate_by_int(&delta);
                if curr_corner[3] == curr_corner[2] {
                    corner_removed = true;
                } else if collinear(&new_corner, &curr_corner[0], &curr_corner[1]) {
                    corner_removed =
                        self.check_reduce_pair(new_corner, curr_corner[1], curr_corner[3]);
                }
            }
            // `:123-150` — the mirrored try toward corner[0].
            if !corner_removed && in_clip[0] && in_clip[1] && in_clip[2] {
                let delta = curr_corner[0].difference_by_int(&curr_corner[1]);
                new_corner = curr_corner[2].translate_by_int(&delta);
                if curr_corner[0] == curr_corner[1] {
                    corner_removed = true;
                } else if collinear(&new_corner, &curr_corner[2], &curr_corner[3]) {
                    corner_removed =
                        self.check_reduce_pair(new_corner, curr_corner[0], curr_corner[2]);
                }
            }

            if corner_removed {
                polyline_changed = true;
                let layer = self.curr_layer();
                changed_area.join_point(new_corner.to_float(), layer);
                changed_area.join_point(curr_corner[1].to_float(), layer);
                changed_area.join_point(curr_corner[2].to_float(), layer);
                curr_corner[1] = new_corner;
                in_clip[1] =
                    clip.is_none_or(|c| !octagon_is_outside(&c, &Point::Int(curr_corner[1])));
            } else {
                new_corners.push(curr_corner[1]);
                curr_corner[0] = curr_corner[1];
                curr_corner[1] = curr_corner[2];
                in_clip[0] = in_clip[1];
                in_clip[1] = in_clip[2];
            }
            curr_corner[2] = curr_corner[3];
            in_clip[2] = in_clip[3];
            corner_no += 1;
        }

        if !polyline_changed {
            return None;
        }
        new_corners.push(curr_corner[1]);
        new_corners.push(curr_corner[2]);
        let result = Polyline::from_int_points(&new_corners);
        if result.arr.len() < 3 {
            return None;
        }
        Some(result)
    }

    /// The two-shape clearance gate both `reduce_corners` branches share
    /// (`:100-121`, `:132-149`): the segment `new_corner -> near` must clear,
    /// and then the segment `new_corner -> far`.
    ///
    /// A check polyline that degenerates (`arr.length != 3`) counts as
    /// **accepted** upstream (`:117-119`, the `else corner_removed = true`
    /// branch) — the two points coincided, so there is no segment to violate.
    fn check_reduce_pair(&mut self, new_corner: IntPoint, near: IntPoint, far: IntPoint) -> bool {
        let first = Polyline::from_int_points(&[new_corner, near]);
        if first.arr.len() != 3 {
            return true;
        }
        let Some(shape) = first.offset_shape(self.curr_half_width(), 0) else {
            return true;
        };
        if !self.check_trace_shape(&shape) {
            return false;
        }
        if new_corner == far {
            return true;
        }
        let second = Polyline::from_int_points(&[new_corner, far]);
        if second.arr.len() != 3 {
            return true;
        }
        match second.offset_shape(self.curr_half_width(), 0) {
            Some(shape) => self.check_trace_shape(&shape),
            None => true,
        }
    }

    /// `smoothen_corners` (`:189-225`) — chamfer every 90°-or-sharper corner
    /// between two 45°-multiple lines.
    ///
    /// The turn test is upstream's `d1.projection(d2) != POSITIVE` (`:203`): a
    /// non-positive projection means the turn is 90° or more. For each such
    /// corner [`PullTightAlgo::smoothen_corner`] (the clearance-checked binary
    /// search) is tried first and [`PullTightAlgo::smoothen_sharp_corner`] (the
    /// fixed, deliberately tiny cut that needs no check) is the fallback —
    /// confirmed by the conformance appendix.
    pub(crate) fn smoothen_corners(
        &mut self,
        polyline: &Polyline,
        changed_area: &mut ChangedArea,
    ) -> Option<Polyline> {
        let mut result: Option<Polyline> = None;
        let mut line_arr: Vec<Line> = polyline.arr.clone();
        loop {
            if line_arr.len() < 4 {
                break;
            }
            let mut polyline_changed = false;
            let mut i = 1usize;
            while i + 2 < line_arr.len() {
                let d1 = line_arr[i].direction();
                let d2 = line_arr[i + 1].direction();
                if d1.is_multiple_of_45_degree()
                    && d2.is_multiple_of_45_degree()
                    && d1.projection(&d2) != Signum::Positive
                {
                    let new_line = self
                        .smoothen_corner(&line_arr, i, changed_area)
                        .or_else(|| self.smoothen_sharp_corner(&line_arr, i, changed_area));
                    if let Some(new_line) = new_line {
                        polyline_changed = true;
                        line_arr.insert(i + 1, new_line);
                        i += 1;
                    }
                }
                i += 1;
            }
            if !polyline_changed {
                break;
            }
            let candidate = Polyline::new_from_lines(&line_arr);
            if candidate.arr.len() < 3 {
                break;
            }
            line_arr = candidate.arr.clone();
            result = Some(candidate);
        }
        result
    }

    /// `smoothen_corner` (`:333-407`) — binary-search the largest diagonal cut
    /// of the corner at `no` that still clears.
    ///
    /// The cut line's direction is the vector sum of the two segment directions
    /// (`:339`), so it is the 45° bisector. The search is capped at
    /// `max(nearest_dist - 1, 1)` (`:356`) and stops at `min_translate_dist`,
    /// with the same [`NUMERIC_BACKOFF`] overshoot rule as
    /// `reposition_line`.
    pub(crate) fn smoothen_corner(
        &mut self,
        line_arr: &[Line],
        no: usize,
        changed_area: &mut ChangedArea,
    ) -> Option<Line> {
        if no < 1 || no + 2 >= line_arr.len() {
            return None;
        }
        let prev_corner = line_arr[no].intersection_approx(&line_arr[no - 1]);
        let curr_corner = line_arr[no].intersection_approx(&line_arr[no + 1]);
        let next_corner = line_arr[no + 1].intersection_approx(&line_arr[no + 2]);

        let translate_line = bisector_line(&line_arr[no], &line_arr[no + 1], &curr_corner)?;
        let prev_dist = translate_line.signed_distance(&prev_corner).abs();
        let next_dist = translate_line.signed_distance(&next_corner).abs();
        if prev_dist == 0.0 || next_dist == 0.0 {
            return None;
        }
        let (mut max_translate_dist, nearest_corner) = if prev_dist <= next_dist {
            (prev_dist, prev_corner)
        } else {
            (next_dist, next_corner)
        };
        if max_translate_dist < 1.0 {
            return None;
        }
        max_translate_dist = (max_translate_dist - 1.0).max(1.0);
        if translate_line.side_of_float(&next_corner) == Side::OnTheLeft {
            max_translate_dist = -max_translate_dist;
        }
        let mut translate_dist = max_translate_dist;
        let mut delta_dist = max_translate_dist;
        let side_of_nearest_corner = translate_line.side_of_float(&nearest_corner);
        let sign = signum_as_int(max_translate_dist);
        let mut result: Option<Line> = None;

        while delta_dist.abs() > self.min_translate_dist() {
            let new_line = translate_line.translate(translate_dist);
            let new_side = new_line.side_of_float(&nearest_corner);
            if new_side == side_of_nearest_corner || new_side == Side::Collinear {
                let mut check_ok = false;
                let check_lines = [line_arr[no], new_line, line_arr[no + 1]];
                let tmp = Polyline::new_from_lines(&check_lines);
                if tmp.arr.len() == 3 {
                    if let Some(shape) = tmp.offset_shape(self.curr_half_width(), 0) {
                        check_ok = self.check_trace_shape(&shape);
                    }
                }
                delta_dist /= 2.0;
                if check_ok {
                    result = Some(new_line);
                    if translate_dist == max_translate_dist {
                        // `:390-393` — biggest possible change. The comparison
                        // is against the *current* `max_translate_dist`, which
                        // the backoff branch below mutates; that is why the
                        // decrement there is kept (see the base
                        // `reposition_line`, where it is dead).
                        break;
                    }
                    translate_dist += delta_dist;
                } else {
                    translate_dist -= delta_dist;
                }
            } else {
                // `:398-403` — overshot at the first try; back off.
                let shorten = f64::from(sign) * NUMERIC_BACKOFF;
                max_translate_dist -= shorten;
                translate_dist -= shorten;
                delta_dist -= shorten;
            }
        }

        if let Some(line) = result {
            let layer = self.curr_layer();
            changed_area.join_point(line_arr[no].intersection_approx(&line), layer);
            changed_area.join_point(line_arr[no + 1].intersection_approx(&line), layer);
            changed_area.join_point(curr_corner, layer);
        }
        result
    }

    /// `smoothen_sharp_corner` (`:233-267`) — the fallback cut.
    ///
    /// The distance is `(sqrt(2) - 1) * half_width`, clamped by both neighbour
    /// distances, rejected below `0.99`, then `max(d - 1, 1)` (`:251-259`). It
    /// is deliberately small enough that **no clearance check is needed**: the
    /// chamfer stays strictly inside the corner's own copper.
    ///
    /// A corner where two diagonals meet at a half-integer is handed to
    /// [`PullTightAlgo::smoothen_non_integer_corner`] first (`:235-242`).
    pub(crate) fn smoothen_sharp_corner(
        &mut self,
        line_arr: &[Line],
        no: usize,
        changed_area: &mut ChangedArea,
    ) -> Option<Line> {
        if no < 1 || no + 2 >= line_arr.len() {
            return None;
        }
        let curr_corner = line_arr[no].intersection_approx(&line_arr[no + 1]);
        if curr_corner.x != curr_corner.x.trunc() {
            if let Some(result) = self.smoothen_non_integer_corner(line_arr, no) {
                return Some(result);
            }
        }
        let prev_corner = line_arr[no].intersection_approx(&line_arr[no - 1]);
        let next_corner = line_arr[no + 1].intersection_approx(&line_arr[no + 2]);

        let translate_line = bisector_line(&line_arr[no], &line_arr[no + 1], &curr_corner)?;
        let mut translate_dist = (Limits::SQRT2 - 1.0) * f64::from(self.curr_half_width());
        translate_dist = translate_dist.min(translate_line.signed_distance(&prev_corner).abs());
        translate_dist = translate_dist.min(translate_line.signed_distance(&next_corner).abs());
        if translate_dist < 0.99 {
            return None;
        }
        translate_dist = (translate_dist - 1.0).max(1.0);
        if translate_line.side_of_float(&next_corner) == Side::OnTheLeft {
            translate_dist = -translate_dist;
        }
        let result = translate_line.translate(translate_dist);
        changed_area.join_point(curr_corner, self.curr_layer());
        Some(result)
    }

    /// `smoothen_non_integer_corner` (`:273-327`).
    ///
    /// Two diagonal lines can intersect at a half-integer, which breaks the
    /// all-`IntPoint` invariant [`PullTightAlgo::reduce_corners`] needs. The fix
    /// is a short axis-parallel line through the ceil/floor lattice point,
    /// picked by the quadrant the two neighbours sit in.
    pub(crate) fn smoothen_non_integer_corner(
        &mut self,
        line_arr: &[Line],
        no: usize,
    ) -> Option<Line> {
        let prev_line = line_arr[no];
        let next_line = line_arr[no + 1];
        if prev_line.is_equal_or_opposite(&next_line) {
            return None;
        }
        if !(prev_line.is_diagonal() && next_line.is_diagonal()) {
            return None;
        }
        let curr_corner = prev_line.intersection_approx(&next_line);
        let prev_corner = prev_line.intersection_approx(&line_arr[no - 1]);
        let next_corner = next_line.intersection_approx(&line_arr[no + 2]);

        let (new_x, new_y, vertical, horizontal) = if prev_corner.x > curr_corner.x
            && next_corner.x > curr_corner.x
        {
            (curr_corner.x.ceil(), curr_corner.y.ceil(), true, false)
        } else if prev_corner.x < curr_corner.x && next_corner.x < curr_corner.x {
            (curr_corner.x.floor(), curr_corner.y.floor(), true, false)
        } else if prev_corner.y > curr_corner.y && next_corner.y > curr_corner.y {
            (curr_corner.x.ceil(), curr_corner.y.ceil(), false, true)
        } else if prev_corner.y < curr_corner.y && next_corner.y < curr_corner.y {
            (curr_corner.x.floor(), curr_corner.y.floor(), false, true)
        } else {
            return None;
        };

        let new_line_dir = if vertical {
            if prev_corner.y < next_corner.y {
                Direction::UP
            } else {
                Direction::DOWN
            }
        } else if horizontal {
            if prev_corner.x < next_corner.x {
                Direction::RIGHT
            } else {
                Direction::LEFT
            }
        } else {
            return None;
        };
        let anchor = IntPoint::new(new_x as i32, new_y as i32);
        Some(Line::new_from_point_and_dir(anchor, &new_line_dir))
    }
}

/// The chamfer line through `corner`, rounded to the lattice, whose direction is
/// the vector sum of the two segment directions (`PullTightAlgo45.java:248-250`,
/// `:338-340`).
///
/// Returns `None` when the two directions cancel — upstream's
/// `Direction.get_instance` would produce the null direction, from which no line
/// can be built.
/// Exact collinearity of three lattice points — the only orientation question
/// `reduce_corners` asks, and it asks it about integers, so it is decided by an
/// exact determinant and never by an `f64`.
fn collinear(point: &IntPoint, p1: &IntPoint, p2: &IntPoint) -> bool {
    point.signed_area_exact(p1, p2) == 0
}

fn bisector_line(prev: &Line, next: &Line, corner: &FloatPoint) -> Option<Line> {
    let sum = prev
        .direction()
        .get_vector()
        .add(&next.direction().get_vector());
    let dir = Direction::from_vector(&sum);
    if dir == Direction::NULL {
        return None;
    }
    Some(Line::new_from_point_and_dir(corner.round(), &dir))
}
