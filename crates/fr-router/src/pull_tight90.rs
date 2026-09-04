//! The 90° local optimization rules — `board/PullTightAlgo90.java` (144 L).
//!
//! Spec `docs/upstream-study/pull-tight-insert.md` §6.3 and the conformance
//! appendix rows for the 90° pipeline. Upstream's `PullTightAlgo.get_instance`
//! (`PullTightAlgo.java:73-75`) maps `NINETY_DEGREE` to this class — **not** to
//! `PullTightAlgo45` — and both fork-smoothing hooks return `null`
//! (`PullTightAlgo90.java:135-143`), so on a 90° board no corner is ever
//! chamfered into a diagonal.
//!
//! ```text
//! pull_tight: avoid_acid_traps once (dead), then
//!             loop { try_skip_second_corner; try_skip_corners; reposition_lines }   (:19-30)
//! ```

use fr_geometry::planar::{Area, Line, Polyline};

use crate::insert_forced::ChangedArea;
use crate::pull_tight::{polyline_from_lines, PullTightAlgo};

impl PullTightAlgo<'_> {
    /// `PullTightAlgo90.pull_tight` (`:19-30`) — the fixed-point loop.
    ///
    /// `avoid_acid_traps` is `if (true) return p_polyline;` in the release
    /// (`PullTightAlgo.java:454-457`, conformance row "avoid_acid_traps dead"),
    /// so it is not reproduced. Returns `None` when nothing changed — upstream's
    /// "returned the input object".
    pub(crate) fn pull_tight_polyline_90(
        &mut self,
        polyline: &Polyline,
        changed_area: &mut ChangedArea,
    ) -> Option<Polyline> {
        let mut curr = polyline.clone();
        let mut any_changed = false;
        while !self.is_stop_requested() && self.take_round() {
            fr_metrics::counters::bump::pull_tight_iterations(1);
            let mut round_changed = false;
            if let Some(next) = self.try_skip_second_corner(&curr) {
                curr = next;
                round_changed = true;
            }
            if let Some(next) = self.try_skip_corners(&curr, changed_area) {
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
        if !any_changed || curr.arr.len() < 3 {
            return None;
        }
        Some(curr)
    }

    /// `try_skip_second_corner` (`:35-60`): swap the first two lines so the
    /// second corner vanishes, when the swapped geometry still clears.
    ///
    /// Returns `None` when nothing was changed — upstream's "return p_polyline".
    fn try_skip_second_corner(&mut self, polyline: &Polyline) -> Option<Polyline> {
        if polyline.arr.len() < 5 {
            return None;
        }
        let check_lines = [
            polyline.arr[1],
            polyline.arr[0],
            polyline.arr[3],
            polyline.arr[4],
        ];
        let check_polyline = Polyline::new_from_lines(&check_lines);
        if check_polyline.arr.len() != 4 {
            return None;
        }
        if let Some(clip) = self.clip_shape() {
            if !clip.contains_float_point(&check_polyline.corner_approx(1)) {
                return None;
            }
        }
        for no in 0..2 {
            // A `None` shape is a degenerate offset; upstream would have thrown
            // on the null. Refusing the move is the safe reading.
            let shape = check_polyline.offset_shape(self.curr_half_width(), no)?;
            if !self.check_trace_shape(&shape) {
                return None;
            }
        }
        // The second corner can be skipped: the first two lines trade places.
        let mut new_lines = Vec::with_capacity(polyline.arr.len() - 1);
        new_lines.push(polyline.arr[1]);
        new_lines.push(polyline.arr[0]);
        new_lines.extend_from_slice(&polyline.arr[3..]);
        polyline_from_lines(&new_lines)
    }

    /// `try_skip_corners` (`:65-133`): sliding-window corner removal.
    ///
    /// Returns `None` when nothing was changed — upstream's "return p_polyline".
    fn try_skip_corners(
        &mut self,
        polyline: &Polyline,
        changed_area: &mut ChangedArea,
    ) -> Option<Polyline> {
        let n = polyline.arr.len();
        if n < 5 {
            return None;
        }
        let mut new_lines: Vec<Line> = Vec::with_capacity(n);
        new_lines.push(polyline.arr[0]);
        new_lines.push(polyline.arr[1]);
        let mut polyline_changed = false;
        let mut second_last_corner_skipped = false;
        let layer = self.curr_layer();
        let mut i = 5;
        while i <= n {
            let mut skip_lines = false;
            // `:75` — the corner this window would drop must be in the clip.
            let in_clip_shape = self.clip_shape().is_none_or(|clip| {
                clip.contains_float_point(&polyline.corner_approx(i - 3))
            });
            if in_clip_shape {
                let check_lines = [
                    new_lines[new_lines.len() - 2],
                    new_lines[new_lines.len() - 1],
                    polyline.arr[i - 1],
                    if i < n {
                        polyline.arr[i]
                    } else {
                        // The concluding line is the second last one (`:83-84`).
                        polyline.arr[i - 2]
                    },
                ];
                let check_polyline = Polyline::new_from_lines(&check_lines);
                skip_lines = check_polyline.arr.len() == 4
                    && self.clip_shape().is_none_or(|clip| {
                        clip.contains_float_point(&check_polyline.corner_approx(1))
                    });
                for no in 0..2 {
                    if !skip_lines {
                        break;
                    }
                    match check_polyline.offset_shape(self.curr_half_width(), no) {
                        Some(shape) => skip_lines = self.check_trace_shape(&shape),
                        None => skip_lines = false,
                    }
                }
                if skip_lines {
                    // `:101-106` — both the merged corner and the skipped one
                    // join the changed area.
                    changed_area.join_point(
                        check_lines[1].intersection_approx(&check_lines[2]),
                        layer,
                    );
                    changed_area.join_point(
                        polyline.arr[i - 2].intersection_approx(&polyline.arr[i - 3]),
                        layer,
                    );
                }
            }
            if skip_lines {
                if i == n {
                    second_last_corner_skipped = true;
                }
                polyline_changed = true;
                i += 2;
            } else {
                new_lines.push(polyline.arr[i - 3]);
                i += 1;
            }
        }
        if !polyline_changed {
            return None;
        }
        if second_last_corner_skipped {
            // The second last corner was skipped: the last two lines trade
            // places (`:119-122`).
            new_lines.push(polyline.arr[n - 1]);
            new_lines.push(polyline.arr[n - 2]);
        } else {
            new_lines.extend_from_slice(&polyline.arr[n - 3..]);
        }
        polyline_from_lines(&new_lines)
    }
}
