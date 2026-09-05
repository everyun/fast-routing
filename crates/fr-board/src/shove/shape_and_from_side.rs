//! `board/CalcShapeAndFromSide.java` (108 L) — one tile shape of a substitute
//! trace, prepared for the recursive shove: dog ears cut off and a from-side
//! derived from the cut.

use fr_geometry::planar::{Line, Polyline, Side, Simplex};

use super::entries::SubstitutePiece;
use super::from_side::FromSide;
use super::geom;
use crate::board::Board;

/// The prepared shape plus its from-side.
#[derive(Debug, Clone)]
pub struct ShapeAndFromSide {
    /// The (possibly cut) tile shape of segment `index`.
    pub shape: Simplex,
    /// `None` is upstream's `null`, which makes
    /// [`ShapeTraceEntries`](super::ShapeTraceEntries) derive the side lazily
    /// from own-net entries.
    pub from_side: Option<FromSide>,
}

impl ShapeAndFromSide {
    /// `CalcShapeAndFromSide(trace, index, orthogonal, in_shove_check)`
    /// (`:21-71`).
    ///
    /// `in_shove_check` is the load-bearing flag: when a cut produced no
    /// from-side, the check paths leave it `null` because deriving it from the
    /// polyline here *"may produce an undesired stack_level > 1 in
    /// ShapeTraceEntries"* (`:65-70`); the insert paths fall back to
    /// [`FromSide::from_polyline`].
    pub fn new(
        board: &Board,
        piece: &SubstitutePiece,
        index: usize,
        orthogonal: bool,
        in_shove_check: bool,
    ) -> Option<ShapeAndFromSide> {
        let tree_shape = piece
            .polyline
            .offset_shape(piece.compensated_half_width(board), index)?;
        let mut curr_shape = tree_shape;
        let mut curr_from_side = None;

        if orthogonal {
            curr_shape = curr_shape.bounding_box().to_simplex();
        } else {
            let compensated = piece.compensated_half_width(board);
            let mut cut_off_at_start = false;
            let mut cut_off_at_end = false;

            let end_cutline = calc_cutline_at_end(index, &piece.polyline, compensated);
            if let Some(cut_line) = end_cutline {
                let tmp_shape = curr_shape.intersection(&geom::half_plane(&cut_line));
                if tmp_shape != curr_shape && !tmp_shape.is_empty() {
                    curr_shape = tmp_shape;
                    cut_off_at_end = true;
                }
            }
            let start_cutline = calc_cutline_at_start(index, &piece.polyline, compensated);
            if let Some(cut_line) = start_cutline {
                let tmp_shape = curr_shape.intersection(&geom::half_plane(&cut_line));
                if tmp_shape != curr_shape && !tmp_shape.is_empty() {
                    curr_shape = tmp_shape;
                    cut_off_at_start = true;
                }
            }

            let mut from_side_no = None;
            let mut curr_cut_line = None;
            if cut_off_at_start {
                curr_cut_line = start_cutline;
                from_side_no = start_cutline.and_then(|line| curr_shape.border_line_index(&line));
            }
            if from_side_no.is_none() && cut_off_at_end {
                curr_cut_line = end_cutline;
                from_side_no = end_cutline.and_then(|line| curr_shape.border_line_index(&line));
            }
            if let (Some(no), Some(cut_line)) = (from_side_no, curr_cut_line) {
                let border_intersection = cut_line.intersection_approx(&curr_shape.border_line(no));
                curr_from_side = Some(FromSide::raw(no as i32, Some(border_intersection)));
            }
        }

        if curr_from_side.is_none() && !in_shove_check {
            curr_from_side = Some(FromSide::from_polyline(&piece.polyline, index, &curr_shape));
        }

        Some(ShapeAndFromSide {
            shape: curr_shape,
            from_side: curr_from_side,
        })
    }
}

/// `calc_cutline_at_end` (`:74-91`).
fn calc_cutline_at_end(
    index: usize,
    trace_lines: &Polyline,
    compensated_half_width: i32,
) -> Option<Line> {
    let line_count = trace_lines.arr.len();
    if line_count < 3 {
        return None;
    }
    let at_last_segment = index + 3 == line_count;
    let near_last_corner = trace_lines
        .corner_approx(line_count - 2)
        .distance(&trace_lines.corner_approx(index + 1))
        < compensated_half_width as f64;
    if !at_last_segment && !near_last_corner {
        return None;
    }
    let curr_line = trace_lines.arr[line_count - 1];
    let is = trace_lines.corner_approx(line_count - 3);
    Some(if curr_line.side_of_float(&is) == Side::OnTheLeft {
        curr_line.opposite()
    } else {
        curr_line
    })
}

/// `calc_cutline_at_start` (`:93-107`).
fn calc_cutline_at_start(
    index: usize,
    trace_lines: &Polyline,
    compensated_half_width: i32,
) -> Option<Line> {
    if trace_lines.arr.len() < 3 {
        return None;
    }
    let near_first_corner = trace_lines
        .corner_approx(0)
        .distance(&trace_lines.corner_approx(index))
        < compensated_half_width as f64;
    if index != 0 && !near_first_corner {
        return None;
    }
    let curr_line = trace_lines.arr[0];
    let is = trace_lines.corner_approx(1);
    Some(if curr_line.side_of_float(&is) == Side::OnTheLeft {
        curr_line.opposite()
    } else {
        curr_line
    })
}
