//! The any-angle local optimization rules — `board/PullTightAlgoAnyAngle.java`
//! (953 L).
//!
//! Spec `docs/upstream-study/pull-tight-insert.md` §6.1. This is the flavor
//! `PullTightAlgo.get_instance` picks when the board's angle restriction is
//! `NONE` (`PullTightAlgo.java:71-85`); [`crate::pull_tight45`] is the one it
//! picks for `NINETY_DEGREE` and `FORTYFIVE_DEGREE`. Until this module existed
//! an any-angle board was optimized with the 45° rules, which is sound (every
//! move is clearance-checked) but weaker — the 45° rules only ever emit corners
//! on the eight directions, so they cannot cut a corner off at an arbitrary
//! angle, which is the whole point of the any-angle mode.
//!
//! ```text
//! pull_tight: avoid_acid_traps once (dead), then
//!             loop { skip_segments_of_length_0; reduce_lines; skip_lines;
//!                    reduce_corners; reposition_lines; smoothen_corners }  (:28-48)
//! ```
//!
//! The pipeline order is **not** the 45° one: `reduce_lines` and `skip_lines`
//! run first and `smoothen_corners` runs last, and upstream's comment at
//! `:35-42` says why — `reduce_corners` introduces new directions and can
//! ping-pong with `smoothen_corners` when consecutive corners are closer than
//! one grid point, so the two direction-preserving rules get first refusal.
//!
//! `avoid_acid_traps` is `if (true) return p_polyline;` in the release
//! (`PullTightAlgo.java:454-457`), so there is nothing to reproduce.
//!
//! Fork/end smoothing (`smoothen_start_corner_at_trace` `:717`,
//! `smoothen_end_corner_at_trace` `:835`) is
//! [`PullTightAlgo::smoothen_start_corner_at_trace_any`] /
//! [`PullTightAlgo::smoothen_end_corner_at_trace_any`], landed in the same
//! change as the 45° pair so the two flavors never disagree for a reason that
//! has nothing to do with the angle mode. See [`crate::pull_tight`]'s module
//! docs for the driver.

use fr_board::item::ItemId;
use fr_datastructures::signum::Signum;
use fr_geometry::planar::{
    Direction, FloatPoint, IntOctagon, Line, Point, Polyline, Side, Vector,
};

use crate::insert_forced::ChangedArea;
use crate::pull_tight::{
    octagon_is_outside, polyline_from_lines, signum_as_int, PullTightAlgo, MAX_COS_ANGLE,
};

/// `c_epsilon` (`PullTightAlgoAnyAngle.java:468`) — a corner this close to the
/// line being translated counts as *on* it, and its neighbours are dragged
/// along.
const EPSILON: f64 = 0.001;

/// `SKIP_LENGTH` (`PullTightAlgoAnyAngle.java:20`) — the **squared** distance
/// under which a non-integer end segment is treated as a rounding artifact by
/// the fork/end smoothing rules (§6.4) and skipped over.
const SKIP_LENGTH: f64 = 10.0;

/// The `check_dist` of `reduce_corners` (`:106`) — a squared distance, so the
/// tolerance on where the rebuilt corner lands is 10 board units.
const REDUCE_CORNERS_CHECK_DIST: f64 = 100.0;

/// `1.5` added to the rebuilt length before comparing (`:150-152`): "because of
/// possible inaccuracy SQRT_2 by twice rounding". Without it the rule can
/// oscillate between two roundings forever.
const REDUCE_CORNERS_LENGTH_SLACK: f64 = 1.5;

impl PullTightAlgo<'_> {
    /// `PullTightAlgoAnyAngle.pull_tight` (`:28-48`) — the fixed-point loop.
    ///
    /// Upstream's "fixed point by object identity" (`while (new_result !=
    /// prev_result)`) becomes an explicit per-round change flag, as spec §11
    /// prescribes, and the deterministic round budget stands in for the wall
    /// clock exactly as it does in the 45° flavor.
    ///
    /// Returns `None` when the polyline was not changed at all — upstream's
    /// "returned the input object".
    pub(crate) fn pull_tight_polyline_any_angle(
        &mut self,
        polyline: &Polyline,
        changed_area: &mut ChangedArea,
    ) -> Option<Polyline> {
        let mut curr = polyline.clone();
        let mut any_changed = false;
        while !self.is_stop_requested() && self.take_round() {
            fr_metrics::counters::bump::pull_tight_iterations(1);
            let mut round_changed = false;
            if let Some(next) = self.skip_segments_of_length_0(&curr) {
                curr = next;
                round_changed = true;
            }
            if let Some(next) = self.reduce_lines_any(&curr, changed_area) {
                curr = next;
                round_changed = true;
            }
            if let Some(next) = self.skip_lines_any(&curr, changed_area) {
                curr = next;
                round_changed = true;
            }
            if let Some(next) = self.reduce_corners_any(&curr, changed_area) {
                curr = next;
                round_changed = true;
            }
            if let Some(next) = self.reposition_lines_any(&curr, changed_area) {
                curr = next;
                round_changed = true;
            }
            if let Some(next) = self.smoothen_corners_any(&curr, changed_area) {
                curr = next;
                round_changed = true;
            }
            if !round_changed {
                break;
            }
            any_changed = true;
        }
        // A move that collapsed the polyline below three lines is not a valid
        // trace; upstream's `new Polyline(...)` produces the same empty array
        // and `change` rejects it downstream.
        if !any_changed || curr.arr.len() < 3 {
            return None;
        }
        Some(curr)
    }

    /// `reduce_corners` (`:54-198`) — replace two consecutive lines by one line
    /// through integer points near the previous and the next corner.
    ///
    /// This is the rule that has no 45° counterpart: the replacement line is
    /// built from two *arbitrary* integer points, so it can take any direction.
    /// The two `check_dist` guards (`:104-121`) are the numerical-stability
    /// gate — a pair of near-parallel lines intersects far from where the
    /// rounding put the corner, and accepting that would move copper a long way
    /// from the geometry that was clearance-checked.
    fn reduce_corners_any(
        &mut self,
        polyline: &Polyline,
        changed_area: &mut ChangedArea,
    ) -> Option<Polyline> {
        if polyline.arr.len() < 4 {
            return None;
        }
        let last_index = polyline.arr.len() - 4;
        let clip = self.clip_shape();

        let mut new_lines: Vec<Line> = polyline.arr.clone();
        let mut new_line_index = 1usize;
        let mut polyline_changed = false;
        // `curr_lines` is upstream's reused three-element scratch array.
        let mut curr_lines = [polyline.arr[0]; 3];

        for i in 0..=last_index {
            let mut skip_line = false;
            let new_a = new_lines[new_line_index - 1].intersection_approx(&new_lines[new_line_index]);
            let new_b = polyline.corner_approx(i + 2);
            // `:73`: the third containment test reads the *original* polyline
            // at `new_line_index`, not at `i`. Transcribed as written.
            let in_clip_shape = match clip.as_ref() {
                None => true,
                Some(octagon) => {
                    octagon_contains_float(octagon, &new_a)
                        && octagon_contains_float(octagon, &new_b)
                        && octagon_contains_float(
                            octagon,
                            &polyline.corner_approx(new_line_index),
                        )
                }
            };

            if in_clip_shape {
                let skip_corner =
                    new_lines[new_line_index].intersection_approx(&polyline.arr[i + 2]);
                curr_lines[1] = Line::new(new_a.round(), new_b.round());
                let mut ok = true;
                if new_line_index == 1 {
                    match polyline.first_corner() {
                        // `:80-88`: the first corner must not move, so the new
                        // start line is the perpendicular through it.
                        Point::Int(first) => {
                            let dir = curr_lines[1].direction();
                            curr_lines[0] =
                                Line::new_from_point_and_dir(first, &dir.turn_45_degree(2));
                        }
                        _ => ok = false,
                    }
                } else {
                    curr_lines[0] = new_lines[new_line_index - 1];
                }
                if i == last_index {
                    match polyline.last_corner() {
                        Point::Int(last) => {
                            let dir = curr_lines[1].direction();
                            curr_lines[2] =
                                Line::new_from_point_and_dir(last, &dir.turn_45_degree(2));
                        }
                        _ => ok = false,
                    }
                } else {
                    curr_lines[2] = polyline.arr[i + 3];
                }

                // `:100-121`: the rebuilt corners must land where the rounding
                // put them, or the lines were too close to parallel.
                if ok {
                    let check_is = curr_lines[0].intersection_approx(&curr_lines[1]);
                    if check_is.distance_square(&new_a) > REDUCE_CORNERS_CHECK_DIST {
                        ok = false;
                    }
                }
                if ok {
                    let check_is = curr_lines[1].intersection_approx(&curr_lines[2]);
                    if check_is.distance_square(&new_b) > REDUCE_CORNERS_CHECK_DIST {
                        ok = false;
                    }
                }
                // `:122-139`: when an end corner is *not* integer there is a
                // trace joined there, and the new neighbour corner has to stay
                // on the same side of the end line or the later split fails.
                if ok && i == 1 && !matches!(polyline.first_corner(), Point::Int(_)) {
                    let new_corner = curr_lines[0].intersection(&curr_lines[1]);
                    if new_lines[0].side_of_point(&new_corner)
                        != new_lines[0].side_of_point(&polyline.corner(1))
                    {
                        ok = false;
                    }
                }
                if ok
                    && last_index > 0
                    && i == last_index - 1
                    && !matches!(polyline.last_corner(), Point::Int(_))
                {
                    let new_corner = curr_lines[1].intersection(&curr_lines[2]);
                    let second_last = polyline.corner(polyline.corner_count() - 2);
                    if new_lines[0].side_of_point(&new_corner)
                        != new_lines[0].side_of_point(&second_last)
                    {
                        ok = false;
                    }
                }

                if ok {
                    let curr_polyline = Polyline::new_from_lines(&curr_lines);
                    if curr_polyline.arr.len() != 3 {
                        ok = false;
                    } else {
                        let length_before =
                            skip_corner.distance(&new_a) + skip_corner.distance(&new_b);
                        let length_after =
                            curr_polyline.length_total() + REDUCE_CORNERS_LENGTH_SLACK;
                        // `:153-158`: a "shortening" that is not shorter comes
                        // from the rounding and would loop forever.
                        if length_after >= length_before {
                            ok = false;
                        }
                    }
                    if ok {
                        if let Some(shape) =
                            curr_polyline.offset_shape(self.curr_half_width(), 0)
                        {
                            skip_line = self.check_trace_shape(&shape);
                        }
                    }
                }
            }

            if skip_line {
                polyline_changed = true;
                new_lines[new_line_index] = curr_lines[1];
                if new_line_index == 1 {
                    // make the first line perpendicular to the current line
                    new_lines[0] = curr_lines[0];
                }
                if i == last_index {
                    // make the last line perpendicular to the current line
                    new_line_index += 1;
                    new_lines[new_line_index] = curr_lines[2];
                }
                let layer = self.curr_layer();
                changed_area.join_point(new_a, layer);
                changed_area.join_point(new_b, layer);
            } else {
                new_line_index += 1;
                new_lines[new_line_index] = polyline.arr[i + 2];
                if i == last_index {
                    new_line_index += 1;
                    new_lines[new_line_index] = polyline.arr[i + 3];
                }
            }
            // `:186-190`: drop a line that came out parallel to its predecessor.
            if new_line_index > 0
                && new_lines[new_line_index].is_parallel(&new_lines[new_line_index - 1])
            {
                new_line_index -= 1;
            }
        }

        if !polyline_changed {
            return None;
        }
        let result = Polyline::new_from_lines(&new_lines[..=new_line_index]);
        if result.arr.is_empty() {
            return None;
        }
        Some(result)
    }

    /// `smoothen_corners` (`:207-232`) — cut every corner that
    /// [`PullTightAlgo::smoothen_corner_any`] can cut, inserting the new line
    /// into the array as it goes.
    fn smoothen_corners_any(
        &mut self,
        polyline: &Polyline,
        changed_area: &mut ChangedArea,
    ) -> Option<Polyline> {
        if polyline.arr.len() < 4 {
            return None;
        }
        let mut line_arr: Vec<Line> = polyline.arr.clone();
        let mut polyline_changed = false;
        let mut i = 0usize;
        while i + 3 < line_arr.len() {
            if let Some(new_line) = self.smoothen_corner_any(&line_arr, i, changed_area) {
                polyline_changed = true;
                line_arr.insert(i + 2, new_line);
                // `:227`: `++i` on top of the loop's own increment — the corner
                // just cut is not offered again.
                i += 1;
            }
            i += 1;
        }
        if !polyline_changed {
            return None;
        }
        let result = Polyline::new_from_lines(&line_arr);
        if result.arr.is_empty() {
            return None;
        }
        Some(result)
    }

    /// `reposition_lines` (`:238-264`) — translate interior lines, keeping the
    /// first two and the last two fixed.
    ///
    /// The any-angle variant differs from the base one in two ways that matter:
    /// it walks **all** repositionable lines in one pass rather than returning
    /// on the first success, and it stops the walk as soon as a translated line
    /// came out parallel to a neighbour, because the corner arithmetic the rest
    /// of the pass needs is undefined then (`:255-261`).
    fn reposition_lines_any(
        &mut self,
        polyline: &Polyline,
        changed_area: &mut ChangedArea,
    ) -> Option<Polyline> {
        if polyline.arr.len() < 5 {
            return None;
        }
        let mut line_arr: Vec<Line> = polyline.arr.clone();
        let mut polyline_changed = false;
        for i in 0..line_arr.len() - 4 {
            let Some(new_line) = self.reposition_line_any(&line_arr, i, changed_area) else {
                continue;
            };
            polyline_changed = true;
            line_arr[i + 2] = new_line;
            if line_arr[i + 2].is_parallel(&line_arr[i + 1])
                || line_arr[i + 2].is_parallel(&line_arr[i + 3])
            {
                break;
            }
        }
        if !polyline_changed {
            return None;
        }
        let result = Polyline::new_from_lines(&line_arr);
        if result.arr.is_empty() {
            return None;
        }
        Some(result)
    }

    /// `reduce_lines` (`:266-365`) — move a line parallel past the intersection
    /// of its neighbours, dropping the corner(s) it crossed.
    ///
    /// This is the direction-preserving line remover the pipeline runs before
    /// `reduce_corners`: it never invents a direction, it only deletes lines.
    fn reduce_lines_any(
        &mut self,
        polyline: &Polyline,
        changed_area: &mut ChangedArea,
    ) -> Option<Polyline> {
        if polyline.arr.len() < 6 {
            return None;
        }
        let clip = self.clip_shape();
        let mut line_arr: Vec<Line> = polyline.arr.clone();
        let mut polyline_changed = false;

        let mut i = 2usize;
        while i + 2 < line_arr.len() {
            let prev_corner = line_arr[i - 2].intersection_approx(&line_arr[i - 1]);
            let next_corner = line_arr[i + 1].intersection_approx(&line_arr[i + 2]);
            let in_clip_shape = match clip.as_ref() {
                None => true,
                Some(octagon) => {
                    octagon_contains_float(octagon, &prev_corner)
                        && octagon_contains_float(octagon, &next_corner)
                }
            };
            if !in_clip_shape {
                i += 1;
                continue;
            }
            let translate_line = line_arr[i];
            let prev_dist = translate_line.signed_distance(&prev_corner);
            let next_dist = translate_line.signed_distance(&next_corner);
            // the two corners are on different sides of the translate line
            if Signum::of(prev_dist) != Signum::of(next_dist) {
                i += 1;
                continue;
            }
            let mut translate_dist = if prev_dist.abs() < next_dist.abs() {
                prev_dist
            } else {
                next_dist
            };
            if translate_dist == 0.0 {
                // the line segment may have length 0
                i += 1;
                continue;
            }
            let line_side = translate_line.side_of_float(&prev_corner);
            let sign = f64::from(signum_as_int(translate_dist));
            let mut new_line = translate_line.translate(-translate_dist);
            let mut side_prev = new_line.side_of_float(&prev_corner);
            let mut side_next = new_line.side_of_float(&next_corner);
            // `:302-308`: make sure we really crossed the nearest corner.
            let mut guard = MAX_CROSSING_STEPS;
            while side_prev == line_side && side_next == line_side && guard > 0 {
                guard -= 1;
                translate_dist += sign * 0.5;
                new_line = translate_line.translate(-translate_dist);
                side_prev = new_line.side_of_float(&prev_corner);
                side_next = new_line.side_of_float(&next_corner);
            }
            if guard == 0 {
                i += 1;
                continue;
            }
            let crossed_before = usize::from(side_prev != line_side);
            let crossed_after = usize::from(side_next != line_side);
            // `:318-322`: never cross both.
            if crossed_before > 1 || crossed_after > 1 {
                i += 1;
                continue;
            }
            if crossed_before > 0 {
                if i < 3 {
                    i += 1;
                    continue;
                }
                let prev_prev = line_arr[i - 3].intersection_approx(&line_arr[i - 2]);
                if new_line.side_of_float(&prev_prev) != line_side {
                    i += 1;
                    continue;
                }
            }
            if crossed_after > 0 {
                if i + 3 >= line_arr.len() {
                    i += 1;
                    continue;
                }
                let next_next = line_arr[i + 2].intersection_approx(&line_arr[i + 3]);
                if new_line.side_of_float(&next_next) != line_side {
                    i += 1;
                    continue;
                }
            }

            let keep_before_ind = i - crossed_before;
            let mut curr_lines: Vec<Line> = Vec::with_capacity(line_arr.len());
            curr_lines.extend_from_slice(&line_arr[..keep_before_ind]);
            curr_lines.push(new_line);
            curr_lines.extend_from_slice(&line_arr[i + 1 + crossed_after..]);

            let tmp = Polyline::new_from_lines(&curr_lines);
            let mut check_ok = false;
            if tmp.arr.len() == curr_lines.len() && keep_before_ind >= 1 {
                if let Some(shape) = tmp.offset_shape(self.curr_half_width(), keep_before_ind - 1)
                {
                    check_ok = self.check_trace_shape(&shape);
                }
            }
            if check_ok {
                let layer = self.curr_layer();
                changed_area.join_point(prev_corner, layer);
                changed_area.join_point(next_corner, layer);
                polyline_changed = true;
                line_arr = curr_lines;
                // `:357`: `--i` then the loop's `++i` — retry the same index,
                // which now holds the translated line.
                continue;
            }
            i += 1;
        }

        if !polyline_changed {
            return None;
        }
        let result = Polyline::new_from_lines(&line_arr);
        if result.arr.is_empty() {
            return None;
        }
        Some(result)
    }

    /// `smoothen_corner` (`:369-462`) — cut one corner off with a line whose
    /// direction is the *middle* of its two neighbours', binary-searching how
    /// far that line may be translated.
    ///
    /// `middle_approx` is what makes this the any-angle rule: the cut line can
    /// point anywhere, where the 45° flavor may only bisect into another
    /// multiple of 45°.
    fn smoothen_corner_any(
        &mut self,
        line_arr: &[Line],
        start_no: usize,
        changed_area: &mut ChangedArea,
    ) -> Option<Line> {
        if line_arr.len() < start_no + 4 {
            return None;
        }
        let curr_corner = line_arr[start_no + 1].intersection_approx(&line_arr[start_no + 2]);
        if let Some(octagon) = self.clip_shape() {
            if !octagon_contains_float(&octagon, &curr_corner) {
                return None;
            }
        }
        // `:377-383`: nearly parallel lines are not divided further.
        if line_cos_angle(&line_arr[start_no + 1], &line_arr[start_no + 2]) > MAX_COS_ANGLE {
            return None;
        }
        let prev_corner = line_arr[start_no].intersection_approx(&line_arr[start_no + 1]);
        let next_corner = line_arr[start_no + 2].intersection_approx(&line_arr[start_no + 3]);

        let prev_dir: Direction = line_arr[start_no + 1].direction();
        let next_dir: Direction = line_arr[start_no + 2].direction();
        let middle_dir = prev_dir.middle_approx(&next_dir);
        let translate_line = Line::new_from_point_and_dir(curr_corner.round(), &middle_dir);
        let prev_dist = translate_line.signed_distance(&prev_corner);
        let next_dist = translate_line.signed_distance(&next_corner);
        let (nearest_point, mut max_translate_dist) = if prev_dist.abs() < next_dist.abs() {
            (prev_corner, prev_dist)
        } else {
            (next_corner, next_dist)
        };
        if max_translate_dist.abs() < 1.0 {
            return None;
        }

        // The cut inserts one line: `curr_lines` is `line_arr` with a hole at
        // `start_no + 2` (`:411-413`).
        let mut curr_lines: Vec<Line> = Vec::with_capacity(line_arr.len() + 1);
        curr_lines.extend_from_slice(&line_arr[..start_no + 2]);
        curr_lines.push(translate_line);
        curr_lines.extend_from_slice(&line_arr[start_no + 2..]);

        let mut translate_dist = max_translate_dist;
        let mut delta_dist = max_translate_dist;
        let side_of_nearest_point = translate_line.side_of_float(&nearest_point);
        let sign = f64::from(signum_as_int(max_translate_dist));
        let mut result: Option<Line> = None;
        let mut guard = MAX_BINARY_SEARCH_STEPS;
        while delta_dist.abs() > self.min_translate_dist() && guard > 0 {
            guard -= 1;
            let mut check_ok = false;
            let new_line = translate_line.translate(-translate_dist);
            let new_side = new_line.side_of_float(&nearest_point);
            if new_side == side_of_nearest_point || new_side == Side::Collinear {
                curr_lines[start_no + 2] = new_line;
                let tmp = Polyline::new_from_lines(&curr_lines);
                if tmp.arr.len() == curr_lines.len() {
                    if let Some(shape) = tmp.offset_shape(self.curr_half_width(), start_no + 1) {
                        check_ok = self.check_trace_shape(&shape);
                    }
                }
                delta_dist /= 2.0;
                if check_ok {
                    result = Some(curr_lines[start_no + 2]);
                    if translate_dist == max_translate_dist {
                        // the biggest possible change
                        break;
                    }
                    translate_dist += delta_dist;
                } else {
                    translate_dist -= delta_dist;
                }
            } else {
                // moved a little too far the first time, from numerical
                // inaccuracy (`:445-451`).
                let shorten_value = sign * 0.5;
                max_translate_dist -= shorten_value;
                translate_dist -= shorten_value;
                delta_dist -= shorten_value;
            }
        }
        let result = result?;
        curr_lines[start_no + 2] = result;
        if let (Some(a0), Some(a1), Some(b0), Some(b1)) = (
            curr_lines.get(start_no),
            curr_lines.get(start_no + 1),
            curr_lines.get(start_no + 3),
            curr_lines.get(start_no + 4),
        ) {
            let layer = self.curr_layer();
            changed_area.join_point(a0.intersection_approx(a1), layer);
            changed_area.join_point(b0.intersection_approx(b1), layer);
        }
        Some(result)
    }

    /// `reposition_line` (`:464-618`) — translate one interior line toward the
    /// nearer of its two outer corners, binary-searching the distance.
    ///
    /// The two `corners_skipped_*` walks (`:485-511`, applied at `:557-580`)
    /// have no counterpart in the base rule and upstream's own comment says
    /// they are rare but load-bearing: when three or more consecutive corners
    /// coincide, every other tightening rule leaves the polyline alone, and
    /// dragging the collapsed neighbours along with the translated line is the
    /// only move that unsticks it.
    fn reposition_line_any(
        &mut self,
        line_arr: &[Line],
        start_no: usize,
        changed_area: &mut ChangedArea,
    ) -> Option<Line> {
        if line_arr.len() < start_no + 5 {
            return None;
        }
        // `:466-477`: both corners of the line to translate must be in the clip.
        if let Some(octagon) = self.clip_shape() {
            for i in 1..3 {
                let corner =
                    line_arr[start_no + i].intersection_approx(&line_arr[start_no + i + 1]);
                if !octagon_contains_float(&octagon, &corner) {
                    return None;
                }
            }
        }
        let translate_line = line_arr[start_no + 2];
        let mut prev_corner = line_arr[start_no].intersection_approx(&line_arr[start_no + 1]);
        let mut next_corner =
            line_arr[start_no + 3].intersection_approx(&line_arr[start_no + 4]);
        let mut prev_dist = translate_line.signed_distance(&prev_corner);
        let mut corners_skipped_before = 0usize;
        let mut corners_skipped_after = 0usize;

        while prev_dist.abs() < EPSILON {
            corners_skipped_before += 1;
            if corners_skipped_before > start_no {
                // the first corner is on the line to translate
                return None;
            }
            let curr_no = start_no - corners_skipped_before;
            prev_corner = line_arr[curr_no].intersection_approx(&line_arr[curr_no + 1]);
            prev_dist = translate_line.signed_distance(&prev_corner);
        }
        let mut next_dist = translate_line.signed_distance(&next_corner);
        while next_dist.abs() < EPSILON {
            corners_skipped_after += 1;
            let curr_no = start_no + 3 + corners_skipped_after;
            if curr_no + 2 >= line_arr.len() {
                // the last corner is on the line to translate
                return None;
            }
            next_corner = line_arr[curr_no].intersection_approx(&line_arr[curr_no + 1]);
            next_dist = translate_line.signed_distance(&next_corner);
        }
        // the two corners are on different sides of the translate line
        if Signum::of(prev_dist) != Signum::of(next_dist) {
            return None;
        }
        let (nearest_point, mut max_translate_dist) = if prev_dist.abs() < next_dist.abs() {
            (prev_corner, prev_dist)
        } else {
            (next_corner, next_dist)
        };

        let mut curr_lines: Vec<Line> = line_arr.to_vec();
        let mut translate_dist = max_translate_dist;
        let mut delta_dist = max_translate_dist;
        let side_of_nearest_point = translate_line.side_of_float(&nearest_point);
        let sign = f64::from(signum_as_int(max_translate_dist));
        let mut result: Option<Line> = None;
        let mut first_time = true;
        let mut guard = MAX_BINARY_SEARCH_STEPS;
        while (first_time || delta_dist.abs() > self.min_translate_dist()) && guard > 0 {
            guard -= 1;
            let mut check_ok = false;
            let mut new_line = translate_line.translate(-translate_dist);
            if first_time && translate_dist.abs() < 1.0 {
                if new_line == translate_line {
                    // `:534-542`: try the parallel line through the rounded
                    // nearest point instead.
                    let rounded = nearest_point.round();
                    if nearest_point.distance(&rounded.to_float()) < translate_dist.abs() {
                        new_line =
                            Line::new_from_point_and_dir(rounded, &translate_line.direction());
                    }
                    first_time = false;
                }
                if new_line == translate_line {
                    return None;
                }
            }
            let new_side = new_line.side_of_float(&nearest_point);
            if new_side == side_of_nearest_point || new_side == Side::Collinear {
                first_time = false;
                curr_lines[start_no + 2] = new_line;
                // `:557-568` / `:569-580`: drag the collapsed neighbours along.
                let mut prev_translated_line = new_line;
                for i in 0..corners_skipped_before {
                    let prev_line_no = start_no + 1 - corners_skipped_before;
                    let curr_prev_corner =
                        prev_translated_line.intersection_approx(&curr_lines[prev_line_no]);
                    let curr_translate_line = line_arr[start_no + 1 - i];
                    let curr_translate_dist =
                        curr_translate_line.signed_distance(&curr_prev_corner);
                    prev_translated_line = curr_translate_line.translate(-curr_translate_dist);
                    curr_lines[start_no + 1 - i] = prev_translated_line;
                }
                prev_translated_line = new_line;
                for i in 0..corners_skipped_after {
                    let next_line_no = start_no + 3 + corners_skipped_after;
                    let curr_next_corner =
                        prev_translated_line.intersection_approx(&curr_lines[next_line_no]);
                    let curr_translate_line = line_arr[start_no + 3 + i];
                    let curr_translate_dist =
                        curr_translate_line.signed_distance(&curr_next_corner);
                    prev_translated_line = curr_translate_line.translate(-curr_translate_dist);
                    curr_lines[start_no + 3 + i] = prev_translated_line;
                }
                let tmp = Polyline::new_from_lines(&curr_lines);
                if tmp.arr.len() == curr_lines.len() {
                    if let Some(shape) = tmp.offset_shape(self.curr_half_width(), start_no + 1) {
                        check_ok = self.check_trace_shape(&shape);
                    }
                }
                delta_dist /= 2.0;
                if check_ok {
                    result = Some(curr_lines[start_no + 2]);
                    if translate_dist == max_translate_dist {
                        break;
                    }
                    translate_dist += delta_dist;
                } else {
                    translate_dist -= delta_dist;
                }
            } else {
                let shorten_value = sign * 0.5;
                max_translate_dist -= shorten_value;
                translate_dist -= shorten_value;
                delta_dist -= shorten_value;
            }
        }
        let result = result?;
        curr_lines[start_no + 2] = result;
        if let (Some(a0), Some(a1), Some(b0), Some(b1)) = (
            curr_lines.get(start_no),
            curr_lines.get(start_no + 1),
            curr_lines.get(start_no + 3),
            curr_lines.get(start_no + 4),
        ) {
            let layer = self.curr_layer();
            changed_area.join_point(a0.intersection_approx(a1), layer);
            changed_area.join_point(b0.intersection_approx(b1), layer);
        }
        Some(result)
    }

    /// `skip_lines` (`:625-715`) — drop one or two lines whose neighbours have
    /// already crossed to the other side of them, first success wins.
    ///
    /// Like `reduce_lines` this only deletes lines, never invents a direction.
    fn skip_lines_any(
        &mut self,
        polyline: &Polyline,
        changed_area: &mut ChangedArea,
    ) -> Option<Polyline> {
        if polyline.arr.len() < 4 {
            return None;
        }
        let clip = self.clip_shape();
        for i in 1..polyline.arr.len() - 3 {
            for j in 0..2 {
                let (mut curr_line, corner1, corner2) = if j == 0 {
                    // try to skip the line before the (i+2)-th line
                    (
                        polyline.arr[i + 2],
                        polyline.corner_approx(i),
                        polyline.corner_approx(i - 1),
                    )
                } else {
                    // try to skip the line after the i-th line
                    (
                        polyline.arr[i],
                        polyline.corner_approx(i + 1),
                        polyline.corner_approx(i + 2),
                    )
                };
                let in_clip_shape = match clip.as_ref() {
                    None => true,
                    Some(octagon) => {
                        octagon_contains_float(octagon, &corner1)
                            && octagon_contains_float(octagon, &corner2)
                    }
                };
                if !in_clip_shape {
                    continue;
                }

                let mut side1 = curr_line.side_of_float(&corner1);
                let mut side2 = curr_line.side_of_float(&corner2);
                if side1 != side2 {
                    let reduced = polyline.skip_lines(i + 1, i + 1);
                    if reduced.arr.len() == polyline.arr.len() - 1 {
                        let shape_no = if j == 0 { i } else { i - 1 };
                        if let Some(shape) =
                            reduced.offset_shape(self.curr_half_width(), shape_no)
                        {
                            if self.check_trace_shape(&shape) {
                                let layer = self.curr_layer();
                                changed_area.join_point(corner1, layer);
                                changed_area.join_point(corner2, layer);
                                return Some(reduced);
                            }
                        }
                    }
                }

                // now try skipping 2 lines (`:684-686`)
                if i + 4 >= polyline.arr.len() {
                    break;
                }
                let corner3 = if j == 1 {
                    polyline.corner_approx(i + 3)
                } else {
                    polyline.corner_approx(i + 1)
                };
                if let Some(octagon) = clip.as_ref() {
                    if !octagon_contains_float(octagon, &corner3) {
                        continue;
                    }
                }
                if j == 0 {
                    // curr_line is one line later than in the one-line case
                    curr_line = polyline.arr[i + 3];
                    side1 = curr_line.side_of_float(&corner1);
                    side2 = curr_line.side_of_float(&corner2);
                } else {
                    side1 = curr_line.side_of_float(&corner3);
                }
                if side1 != side2 {
                    let reduced = polyline.skip_lines(i + 1, i + 2);
                    if reduced.arr.len() == polyline.arr.len() - 2 {
                        let shape_no = if j == 0 { i } else { i - 1 };
                        if let Some(shape) =
                            reduced.offset_shape(self.curr_half_width(), shape_no)
                        {
                            if self.check_trace_shape(&shape) {
                                let layer = self.curr_layer();
                                changed_area.join_point(corner1, layer);
                                changed_area.join_point(corner2, layer);
                                changed_area.join_point(corner3, layer);
                                return Some(reduced);
                            }
                        }
                    }
                }
            }
        }
        None
    }

    // ---- §6.4 fork/end smoothing, any-angle flavor -----------------------

    /// `PullTightAlgoAnyAngle.smoothen_start_corner_at_trace` (`:717-833`).
    ///
    /// Structurally the 45° rule with two differences, both upstream's:
    ///
    /// * the `acute_angle` branch does **not** require the contact's line to be
    ///   orthogonal (`:764-767` against `PullTightAlgo45.java:454-457`) — an
    ///   any-angle joint can double back at any angle;
    /// * [`SKIP_LENGTH`]: when the end corner is *not* an integer point and sits
    ///   within `√SKIP_LENGTH` of its neighbour, the rule operates one line
    ///   further in (`:731-738`). A sub-grid stub at the end of the trace is an
    ///   artifact of rounding, not geometry the joint should be measured
    ///   against.
    pub(crate) fn smoothen_start_corner_at_trace_any(
        &mut self,
        id: ItemId,
        changed_area: &mut ChangedArea,
    ) -> Option<Polyline> {
        let trace_polyline = self.trace_polyline_of(id)?;
        let arr = trace_polyline.arr.clone();
        if arr.len() < 3 {
            return None;
        }
        let curr_end_corner = trace_polyline.corner(0);
        if let Some(clip) = self.clip_shape() {
            if octagon_is_outside(&clip, &curr_end_corner) {
                return None;
            }
        }
        let mut curr_prev_end_corner = trace_polyline.corner(1);
        let skip_short_segment = !matches!(curr_end_corner, Point::Int(_))
            && curr_end_corner
                .to_float()
                .distance_square(&curr_prev_end_corner.to_float())
                < SKIP_LENGTH;
        let mut start_line_no = 1usize;
        if skip_short_segment {
            if trace_polyline.corner_count() < 3 {
                return None;
            }
            curr_prev_end_corner = trace_polyline.corner(2);
            start_line_no += 1;
        }
        if arr.len() <= start_line_no + 1 {
            return None;
        }
        let line_direction = arr[start_line_no].direction();
        let prev_line_direction = arr[start_line_no + 1].direction();

        let fork = self.scan_fork_contacts(
            id,
            &curr_end_corner,
            &curr_prev_end_corner,
            &line_direction,
            &prev_line_direction,
            trace_polyline.corner_count(),
            false,
            false,
        )?;
        let contact = fork.contact?;

        // `:785-790` — one line shorter, and copied from one line later, when
        // the short end segment is being skipped.
        let new_line_count = if skip_short_segment {
            arr.len()
        } else {
            arr.len() + 1
        };
        let copy_from = if skip_short_segment { 2 } else { 1 };

        if fork.acute_angle {
            let add_line = self.fork_chamfer_line(
                &curr_end_corner,
                &curr_prev_end_corner,
                &contact,
                fork.prev_corner_side,
                (2, 6),
            )?;
            let mut new_lines = Vec::with_capacity(new_line_count);
            new_lines.push(contact.line);
            new_lines.push(add_line);
            new_lines.extend_from_slice(&arr[copy_from..copy_from + new_line_count - 2]);
            return polyline_from_lines(&new_lines);
        }
        if fork.bend {
            let mut check_lines = Vec::with_capacity(new_line_count);
            check_lines.push(contact.prev_line);
            check_lines.push(contact.line);
            check_lines.extend_from_slice(&arr[copy_from..copy_from + new_line_count - 2]);
            // `:824` — the any-angle `reposition_line` indexes from the window
            // start, so the junction window begins at 0 here, not at 2.
            let new_line = self.reposition_line_any(&check_lines, 0, changed_area)?;
            let mut new_lines = Vec::with_capacity(arr.len());
            new_lines.push(contact.line);
            new_lines.push(new_line);
            new_lines.extend_from_slice(&arr[2..]);
            return polyline_from_lines(&new_lines);
        }
        None
    }

    /// `PullTightAlgoAnyAngle.smoothen_end_corner_at_trace` (`:835-951`).
    ///
    /// Two things in this one are upstream oddities, transcribed rather than
    /// corrected because the differential harness cannot attribute a
    /// "correction":
    ///
    /// * `prev_line_direction` reads `arr[end_line_no]`, the **same** index as
    ///   `line_direction` (`:860` against `:859`), where the 45° mirror reads
    ///   `arr[len - 3]`. The `bend` branch's second projection test is therefore
    ///   the first one repeated, which makes `bend` here strictly narrower than
    ///   its start-corner twin;
    /// * the contact loop skips two-corner contacts entirely (`:867`), a guard
    ///   none of the other three rules has.
    pub(crate) fn smoothen_end_corner_at_trace_any(
        &mut self,
        id: ItemId,
        changed_area: &mut ChangedArea,
    ) -> Option<Polyline> {
        let trace_polyline = self.trace_polyline_of(id)?;
        let arr = trace_polyline.arr.clone();
        if arr.len() < 3 {
            return None;
        }
        let curr_end_corner = trace_polyline.last_corner();
        if let Some(clip) = self.clip_shape() {
            if octagon_is_outside(&clip, &curr_end_corner) {
                return None;
            }
        }
        let corner_count = trace_polyline.corner_count();
        let mut curr_prev_end_corner = trace_polyline.corner(corner_count - 2);
        let skip_short_segment = !matches!(curr_end_corner, Point::Int(_))
            && curr_end_corner
                .to_float()
                .distance_square(&curr_prev_end_corner.to_float())
                < SKIP_LENGTH;
        let mut end_line_no = arr.len() - 2;
        if skip_short_segment {
            if corner_count < 3 {
                return None;
            }
            curr_prev_end_corner = trace_polyline.corner(corner_count - 3);
            end_line_no -= 1;
        }
        let line_direction = arr[end_line_no].direction().opposite();
        // Upstream reads the same index again; see the doc comment.
        let prev_line_direction = arr[end_line_no].direction().opposite();

        let fork = self.scan_fork_contacts(
            id,
            &curr_end_corner,
            &curr_prev_end_corner,
            &line_direction,
            &prev_line_direction,
            corner_count,
            false,
            true,
        )?;
        let contact = fork.contact?;

        let new_line_count = if skip_short_segment {
            arr.len()
        } else {
            arr.len() + 1
        };
        let copy_from = usize::from(skip_short_segment);

        if fork.acute_angle {
            let add_line = self.fork_chamfer_line(
                &curr_end_corner,
                &curr_prev_end_corner,
                &contact,
                fork.prev_corner_side,
                (6, 2),
            )?;
            let mut new_lines = Vec::with_capacity(new_line_count);
            new_lines.extend_from_slice(&arr[..new_line_count - 2]);
            new_lines.push(add_line);
            new_lines.push(contact.line);
            return polyline_from_lines(&new_lines);
        }
        if fork.bend {
            let mut check_lines = Vec::with_capacity(new_line_count);
            check_lines.extend_from_slice(&arr[copy_from..copy_from + new_line_count - 2]);
            check_lines.push(contact.line);
            check_lines.push(contact.prev_line);
            let start_no = check_lines.len().checked_sub(5)?;
            let new_line = self.reposition_line_any(&check_lines, start_no, changed_area)?;
            let mut new_lines = Vec::with_capacity(arr.len());
            new_lines.extend_from_slice(&arr[..arr.len() - 2]);
            new_lines.push(new_line);
            new_lines.push(contact.line);
            return polyline_from_lines(&new_lines);
        }
        None
    }
}

/// Ceiling on the "make sure we crossed the corner" walk of `reduce_lines`
/// (`:302-308`) and on every binary search here.
///
/// Not upstream's — upstream relies on the arithmetic terminating. Both loops
/// step by a fixed amount toward a bound, so they do terminate; this is the
/// belt-and-braces bound that turns a numerical pathology into a refused move
/// instead of a hang, the same discipline `trace_ops`' `MAX_SPLIT_STEPS` uses.
const MAX_CROSSING_STEPS: u32 = 4096;
const MAX_BINARY_SEARCH_STEPS: u32 = 4096;

/// `Line.cos_angle(Line)` (`geometry/planar/Line.java:449-453`): the cosine of
/// the angle between the two lines' `b - a` vectors.
fn line_cos_angle(a: &Line, b: &Line) -> f64 {
    let va: Vector = Point::Int(a.b).difference_by(&Point::Int(a.a));
    let vb: Vector = Point::Int(b.b).difference_by(&Point::Int(b.a));
    va.cos_angle(&vb)
}

/// `IntOctagon.contains(FloatPoint)` — inside all eight half planes.
///
/// [`crate::pull_tight::octagon_is_outside`] answers the same question for an
/// exact [`Point`]; the any-angle rules ask it about float corners throughout.
fn octagon_contains_float(octagon: &IntOctagon, point: &FloatPoint) -> bool {
    point.x >= f64::from(octagon.left_x)
        && point.y >= f64::from(octagon.bottom_y)
        && point.x <= f64::from(octagon.right_x)
        && point.y <= f64::from(octagon.top_y)
        && (point.x - point.y) >= f64::from(octagon.upper_left_diagonal_x)
        && (point.x - point.y) <= f64::from(octagon.lower_right_diagonal_x)
        && (point.x + point.y) >= f64::from(octagon.lower_left_diagonal_x)
        && (point.x + point.y) <= f64::from(octagon.upper_right_diagonal_x)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fr_geometry::planar::IntPoint;

    #[test]
    fn cos_angle_matches_the_java_helper() {
        // Two perpendicular lines: cos 90° = 0.
        let a = Line::new(IntPoint::new(0, 0), IntPoint::new(1000, 0));
        let b = Line::new(IntPoint::new(0, 0), IntPoint::new(0, 1000));
        assert!(line_cos_angle(&a, &b).abs() < 1e-12);
        // Two parallel lines pointing the same way: cos 0° = 1.
        let c = Line::new(IntPoint::new(5, 7), IntPoint::new(105, 7));
        assert!((line_cos_angle(&a, &c) - 1.0).abs() < 1e-12);
        // Opposite: cos 180° = -1, and that is *below* MAX_COS_ANGLE, so a
        // doubled-back corner is still offered to `smoothen_corner`.
        let d = Line::new(IntPoint::new(0, 0), IntPoint::new(-1000, 0));
        assert!((line_cos_angle(&a, &d) + 1.0).abs() < 1e-12);
    }

    #[test]
    fn a_nearly_parallel_pair_is_above_the_max_cos_angle_gate() {
        // ~1.1°, well inside the 2.6° band `smoothen_corner` refuses.
        let a = Line::new(IntPoint::new(0, 0), IntPoint::new(100_000, 0));
        let b = Line::new(IntPoint::new(0, 0), IntPoint::new(100_000, 2_000));
        assert!(line_cos_angle(&a, &b) > MAX_COS_ANGLE);
    }

    #[test]
    fn octagon_contains_float_is_the_complement_of_is_outside() {
        let octagon = IntOctagon::new(0, 0, 100, 100, -100, 100, 0, 200);
        assert!(octagon_contains_float(&octagon, &FloatPoint::new(50.0, 50.0)));
        assert!(!octagon_contains_float(
            &octagon,
            &FloatPoint::new(-0.5, 50.0)
        ));
        assert!(!octagon_contains_float(
            &octagon,
            &FloatPoint::new(50.0, 100.5)
        ));
        // On the border counts as contained, as `is_outside` uses strict
        // comparisons.
        assert!(octagon_contains_float(&octagon, &FloatPoint::new(0.0, 0.0)));
    }
}
