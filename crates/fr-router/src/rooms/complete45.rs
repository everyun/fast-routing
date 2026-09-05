//! The 45° room completion — `ShapeSearchTree45Degree.complete_shape` and
//! `restrain_shape` (`board/ShapeSearchTree45Degree.java:84-408`).
//!
//! # Why this exists at all
//!
//! [`complete`](super::complete) is the any-angle algorithm: it walks the
//! obstacle's border **lines**, tests each segment against the room with
//! `is_intersected_interior_by`, measures `distance_to_the_left` of the
//! contained simplex, and cuts with a half-plane simplex intersection. Every
//! one of those steps is `O(edges)` with rational arithmetic behind it.
//!
//! The 45° algorithm is the same skeleton with all of that replaced by integer
//! comparisons on eight numbers, and its tiles are octagons:
//!
//! | any-angle | 45° |
//! |---|---|
//! | `is_intersected_interior_by(room, segment)` | [`obstacle_segment_touches_inside`] — 8 corner-side tests |
//! | `distance_to_the_left(contained, line)` | [`signed_line_distance`] — one subtraction, diagonals scaled by 0.5 |
//! | `room ∩ half_plane(line)` | [`calc_outside_restrained_shape`] — replace **one** of the eight offsets |
//!
//! # What it actually measured
//!
//! The M2b prediction was that this would *coarsen* the tiling and cut
//! expansions. It does not. Measured at `-mp 1` (full table on
//! [`Tiling45Mode`]), turning this module on raises expansions — Issue034
//! 65,530 → 95,325, Issue026 6,650 → 20,634 — because the octagon completion
//! produces **more and smaller** rooms than the any-angle one on these boards,
//! not fewer and larger: an obstacle whose exact simplex cut would leave the
//! room alone still gets one of eight fixed half planes taken out of it.
//!
//! Re-measured in M3 against the lab jar's own decomposition, with the
//! `FR_ROOM_TRACE` / `bench/tools/room_stats.py` harness and after the
//! `divide_large_room` contained-shape fix in [`complete`](super::complete):
//! on Issue026 at `-mp 1` this mode builds **3.73×** the jar's rooms against
//! [`Tiling45Mode::Off`]'s 5.36×, and its located chains carry **1.52×** the
//! jar's corners against `Off`'s 1.59× — and the *opening* of a connection is
//! bit-identical to the jar's, room bounding box for room bounding box (net 19
//! agrees on its first sixteen completed rooms before the door visit order
//! parts company). That is the parity evidence the A/B never had: the octagon
//! completion is upstream's algorithm on a 45° board and reproduces it exactly
//! until something *else* diverges. It still does not ship by default, because
//! the gate score, not the parity, decides that.
//!
//! The win in the chain is the *other* half, the octagon door calculation in
//! [`neighbours45`](super::neighbours45), which is why
//! [`Tiling45Mode::Doors`] exists and why this module is behind
//! [`Tiling45Mode::Full`] alone. It is kept, complete and tested, because it is
//! the upstream algorithm and the gate is entitled to A/B it on the full set —
//! and because on Issue034 it still improves unrouted (37 → 36) at zero added
//! violations.
//!
//! # The two rules that are easy to get wrong
//!
//! * **The 0.5 diagonal preference** (`:65-70`). The true distance from a
//!   diagonal line is the offset difference over `√2 ≈ 0.707`; upstream uses
//!   `0.5` *"to prefer orthogonal lines slightly to diagonal restraining
//!   lines"*. It is not an approximation of `1/√2` — it is a deliberate bias,
//!   and copying `1/√2` instead changes which line wins on nearly-tied cuts and
//!   therefore changes the tiling.
//! * **The contains-shape endless-loop guard** (`:216-218`). A result room whose
//!   `contained_shape` already contains the room shape is dropped. Without it
//!   the room lifecycle can re-complete the same region forever, because the
//!   room is not growing and the contained shape is not shrinking.

use fr_board::search_tree::Tiling45Mode;
use fr_board::shape::TileShape;
use fr_geometry::planar::{IntOctagon, Side, Simplex};

use crate::engine::{AutorouteEngine, Obstacle};
use crate::rooms::complete::CompletedShape;
use crate::rooms::RoomId;

/// Number of octagon border lines.
const SIDE_COUNT: usize = 8;

impl AutorouteEngine<'_> {
    /// True when the 45° **door calculation** runs
    /// ([`Tiling45Mode::Doors`] or [`Tiling45Mode::Full`]).
    #[inline]
    pub(crate) fn is_fortyfive(&self) -> bool {
        self.tiling_45().is_on()
    }

    /// True when the 45° **room completion** runs ([`Tiling45Mode::Full`]).
    ///
    /// Separate from [`is_fortyfive`](Self::is_fortyfive) because the two halves
    /// of the chain measure very differently — see [`Tiling45Mode`].
    #[inline]
    pub(crate) fn is_fortyfive_completion(&self) -> bool {
        self.tiling_45() == Tiling45Mode::Full
    }

    /// `ShapeSearchTree45Degree.complete_shape` (`:84-220`).
    ///
    /// Signature-compatible with the any-angle
    /// [`complete_shape`](AutorouteEngine::complete_shape) so the room lifecycle
    /// is shared: the simplex arguments are narrowed to their bounding octagons
    /// on the way in — which is upstream's own `containedRaw.bounding_octagon()`
    /// (`:96`) and `p_room.get_shape().bounding_octagon()` (`:115`) — and the
    /// results carry their exact octagon back out in
    /// [`CompletedShape::oct`], so nothing downstream has to re-derive it.
    pub(crate) fn complete_shape_45(
        &mut self,
        shape: Option<&Simplex>,
        contained: &Simplex,
        layer: i32,
        ignore_room: Option<RoomId>,
        ignore_shape: Option<&Simplex>,
    ) -> Vec<CompletedShape> {
        let Some(contained_oct) = self.narrow_to_octagon(contained) else {
            return Vec::new();
        };
        let shape_oct = match shape {
            Some(shape) => match self.narrow_to_octagon(shape) {
                Some(oct) => Some(oct),
                None => return Vec::new(),
            },
            None => None,
        };
        let ignore_oct = ignore_shape.and_then(simplex_octagon);
        self.complete_shape_45_oct(shape_oct, contained_oct, layer, ignore_room, ignore_oct)
            .into_iter()
            .map(|room| CompletedShape {
                shape: room.shape.to_simplex(),
                contained: room.contained.to_simplex(),
                oct: Some(room.shape),
                bbox: std::cell::Cell::new(None),
                sat: std::cell::RefCell::new(None),
            })
            .collect()
    }

    /// [`complete_shape_45`](Self::complete_shape_45) with both ends already in
    /// the octagon flavor, so a caller that is octagon-native — the 45° door
    /// calculation's `try_remove_edge_line` — pays no round trip.
    pub(crate) fn complete_shape_45_oct(
        &mut self,
        shape: Option<IntOctagon>,
        contained_oct: IntOctagon,
        layer: i32,
        ignore_room: Option<RoomId>,
        ignore_oct: Option<IntOctagon>,
    ) -> Vec<Room45> {
        if contained_oct.is_empty() {
            return Vec::new();
        }

        // `:109-116` — `board.get_bounding_box().bounding_octagon()`, then the
        // room's own shape if it has one.
        let mut start = self.bounds().to_int_octagon().normalize();
        if let Some(shape_oct) = shape {
            start = shape_oct.intersection(&start).normalize();
        }
        if start.dimension() != 2 {
            return Vec::new();
        }

        let mut result: Vec<Room45> = vec![Room45 {
            shape: start,
            contained: contained_oct,
        }];

        // Upstream re-walks the tree with a shrinking `bounding_shape`; this
        // gathers once against the *start* bound, for the reason spelled out in
        // `complete::complete_shape`: the walk's own filter is a bounding-shape
        // test, the query returns a superset, and every extra entry is a no-op
        // because it fails the `overlaps` gate below. The query goes in as a
        // box so the tree's per-leaf filter stays four integer comparisons.
        //
        // Unlike `complete::complete_shape` this does **not** re-sort into the
        // `ShapeTree.Leaf` order: `ShapeSearchTree45Degree.complete_shape`
        // (`:127-200`) never got the `Collections.sort` its orthogonal sibling
        // did and still carves in raw `node_stack` pop order, which is a tree
        // shape we do not reproduce. The query's ascending-id order stands in
        // for it — deterministic, but not numerically upstream's, which is one
        // more reason the 45° chain ships behind `TILING_45_ENV`.
        let mut obstacles = Vec::new();
        let mut own_net = Vec::new();
        self.overlapping_obstacles_tile_for_completion(
            &TileShape::Box(start.bounding_box()),
            Some(layer),
            self.ctrl.net_no,
            &mut obstacles,
            &mut own_net,
        );

        for obstacle in obstacles {
            if Some(obstacle) == ignore_room.map(Obstacle::Room) {
                continue;
            }
            if self.obstacle_layer(obstacle) != Some(layer) {
                continue;
            }
            let Some(obstacle_oct) = self.obstacle_octagon(obstacle) else {
                continue;
            };
            // Cheap integer reject before the exact loop: the item octagon
            // is memoized per connection (see `tile_octagon_cache`), so this
            // box costs four integer reads. `octagon_overlaps` implies the
            // bounding boxes intersect, so an obstacle disjoint from every
            // candidate's box keeps all of them unchanged — exactly the
            // loop's answer. (`scratch_hit_bounds` would avoid the memo
            // read, but the plain `overlapping_obstacles_tile` re-sorts for
            // the neighbour order after collecting, so it is not paired
            // with the collect's bounds here.)
            let obstacle_bounds = obstacle_oct.bounding_box();
            if !result
                .iter()
                .any(|candidate| candidate.shape.bounding_box().intersects(&obstacle_bounds))
            {
                continue;
            }

            let mut new_result: Vec<Room45> = Vec::new();
            for candidate in &result {
                if !octagon_overlaps(&candidate.shape, &obstacle_oct) {
                    // `:186-192`
                    new_result.push(*candidate);
                    continue;
                }
                // `:160-174`: a completed room whose overlap with us is inside
                // the door we came through is the room we came from. Keep the
                // candidate unchanged — unless the candidate is *itself* wholly
                // inside the ignore shape, in which case it is dropped.
                if let (Obstacle::Room(_), Some(ignore)) = (obstacle, ignore_oct) {
                    let intersection = candidate.shape.intersection(&obstacle_oct).normalize();
                    if octagon_contains(&ignore, &intersection) {
                        if !octagon_contains(&ignore, &candidate.shape) {
                            new_result.push(*candidate);
                        }
                        continue;
                    }
                }
                self.restrain_shape_45(candidate, &obstacle_oct, &mut new_result);
            }
            result = new_result;
            if result.is_empty() {
                return Vec::new();
            }
        }

        // `:214` then `:216-218`: divide, then drop any room the contained
        // shape already swallows, which is what stops the completion loop.
        self.divide_large_rooms_45(result)
            .into_iter()
            .filter(|room| !octagon_contains(&room.contained, &room.shape))
            .collect()
    }

    /// The octagon flavor of a room shape that may be **unbounded**.
    ///
    /// The 45° chain builds only board-clipped octagon rooms, but it does not
    /// own every room it is handed: `remove_complete_expansion_room`'s repair
    /// frontier (`AutorouteEngine.java:359-394`) makes an incomplete room out of
    /// a bare half plane, and `complete_expansion_room` passes that straight in.
    /// Upstream's `bounding_octagon()` of such a shape reports the coordinate
    /// envelope in the unconstrained directions and the completion then clips it
    /// against the board (`ShapeSearchTree45Degree.java:115`); clipping first is
    /// the same region and keeps every later offset comparison in range.
    ///
    /// Getting this wrong is not a rounding difference — it drops the room
    /// entirely, and with it every connection that needed the repaired tile.
    fn narrow_to_octagon(&self, shape: &Simplex) -> Option<IntOctagon> {
        if shape.is_bounded() {
            return simplex_octagon(shape);
        }
        if shape.is_empty() {
            return Some(IntOctagon::EMPTY);
        }
        let clipped = self.bounds().to_simplex().intersection(shape);
        if clipped.is_empty() {
            return Some(IntOctagon::EMPTY);
        }
        simplex_octagon(&clipped)
    }

    /// `ShapeSearchTree45Degree.restrain_shape` (`:243-350`), appending to
    /// `out` instead of returning a list so the recursion allocates nothing.
    fn restrain_shape_45(&mut self, room: &Room45, obstacle: &IntOctagon, out: &mut Vec<Room45>) {
        if room.contained.is_empty() {
            return;
        }
        if !self.enter_restrain() {
            return;
        }
        let room_shape = room.shape;
        let contained = room.contained;

        // Pass 1 (`:294-307`): among the obstacle border lines whose segment
        // crosses the room's interior, take the one the contained octagon is
        // furthest to the right of, and cut with its outside half plane.
        let mut cut_line_distance = -1.0f64;
        let mut restraining_line_no: usize = usize::MAX;
        for line_no in 0..SIDE_COUNT {
            let distance = signed_line_distance(obstacle, line_no, &contained);
            if distance > cut_line_distance
                && obstacle_segment_touches_inside(obstacle, line_no, &room_shape)
            {
                cut_line_distance = distance;
                restraining_line_no = line_no;
            }
        }
        if cut_line_distance >= 0.0 {
            let restrained =
                calc_outside_restrained_shape(obstacle, restraining_line_no, &room_shape);
            out.push(Room45 {
                shape: restrained,
                contained,
            });
            self.leave_restrain();
            return;
        }

        // `:312-315`: a contained shape of dimension 0 already has a completed
        // room around it.
        if contained.dimension() < 1 {
            self.leave_restrain();
            return;
        }

        // Pass 2 (`:317-332`): a border line that splits the contained shape.
        let mut split_line_no: Option<usize> = None;
        for line_no in 0..SIDE_COUNT {
            if !obstacle_segment_touches_inside(obstacle, line_no, &room_shape) {
                continue;
            }
            let line = obstacle.border_line(line_no);
            if crate::geom::side_of_line(&contained.to_simplex(), &line) == Side::Collinear {
                split_line_no = Some(line_no);
                break;
            }
        }
        let Some(split_line_no) = split_line_no else {
            // Parts of the room may already be occupied from somewhere else.
            self.leave_restrain();
            return;
        };

        // `:333-339` — the far piece keeps whatever of the contained shape is
        // on its side …
        let restrained = calc_outside_restrained_shape(obstacle, split_line_no, &room_shape);
        if restrained.dimension() == 2 {
            let new_contained = contained.intersection(&restrained).normalize();
            if new_contained.dimension() > 0 {
                out.push(Room45 {
                    shape: restrained,
                    contained: new_contained,
                });
            }
        }
        // … and the near piece is restrained again against the same obstacle
        // (`:341-348`).
        let rest = calc_inside_restrained_shape(obstacle, split_line_no, &room_shape);
        if rest.dimension() >= 2 {
            let rest_contained = contained.intersection(&rest).normalize();
            if rest_contained.dimension() >= 0 {
                self.restrain_shape_45(
                    &Room45 {
                        shape: rest,
                        contained: rest_contained,
                    },
                    obstacle,
                    out,
                );
            }
        }
        self.leave_restrain();
    }

    /// `ShapeSearchTree45Degree.divide_large_room` (`:226-237`): the base-class
    /// division, then every result re-octagonized.
    fn divide_large_rooms_45(&self, rooms: Vec<Room45>) -> Vec<Room45> {
        if rooms.len() != 1 {
            return rooms;
        }
        let simplex_form = rooms
            .iter()
            .map(|room| CompletedShape {
                shape: room.shape.to_simplex(),
                contained: room.contained.to_simplex(),
                oct: Some(room.shape),
                bbox: std::cell::Cell::new(None),
                sat: std::cell::RefCell::new(None),
            })
            .collect();
        // `:226-237`: every piece is re-octagonized, contained shape included.
        // An empty contained shape stays empty — `simplex_octagon` answers
        // `IntOctagon::EMPTY` for it — which is what makes a section that
        // misses the contained shape die at its next completion instead of
        // being kept alive as a room equal to its own contained shape.
        self.divide_large_rooms(simplex_form)
            .into_iter()
            .filter_map(|piece| {
                let shape = simplex_octagon(&piece.shape)?;
                let contained = simplex_octagon(&piece.contained)?;
                Some(Room45 { shape, contained })
            })
            .collect()
    }
}

/// One candidate room while the octagon completion is running.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Room45 {
    pub(crate) shape: IntOctagon,
    pub(crate) contained: IntOctagon,
}

/// The bounding octagon of a simplex, `None` for a shape that has none.
///
/// An unbounded simplex answers `None` rather than a garbage envelope: every
/// shape the 45° chain narrows is a room or door shape that has already been
/// clipped to the board.
pub(crate) fn simplex_octagon(shape: &Simplex) -> Option<IntOctagon> {
    if shape.is_empty() {
        return Some(IntOctagon::EMPTY);
    }
    if !shape.is_bounded() {
        return None;
    }
    shape.bounding_octagon().map(|oct| oct.normalize())
}

/// `IntOctagon.overlaps` (`IntOctagon.java:675-706`): the intersection exists
/// and is 2-dimensional, decided by four strict comparisons without building it.
pub(crate) fn octagon_overlaps(a: &IntOctagon, b: &IntOctagon) -> bool {
    a.left_x.max(b.left_x) < a.right_x.min(b.right_x)
        && a.bottom_y.max(b.bottom_y) < a.top_y.min(b.top_y)
        && a.lower_left_diagonal_x.max(b.lower_left_diagonal_x)
            < a.upper_right_diagonal_x.min(b.upper_right_diagonal_x)
        && a.upper_left_diagonal_x.max(b.upper_left_diagonal_x)
            < a.lower_right_diagonal_x.min(b.lower_right_diagonal_x)
}

/// `TileShape.contains(TileShape)` for two octagons: `inner ⊆ container`.
///
/// For convex shapes given as half-plane offsets this is offset domination in
/// all eight directions, which is what `intersection == inner` says.
pub(crate) fn octagon_contains(container: &IntOctagon, inner: &IntOctagon) -> bool {
    if inner.is_empty() {
        return true;
    }
    container.intersection(inner).normalize() == inner.normalize()
}

/// `IntOctagon.side_of_border_line` (`IntOctagon.java:565-590`).
///
/// Computed in `i64`: the offsets can be `±Limits::CRIT_INT` on a relaxed room
/// shape, and `x + y` of two such would wrap in `i32`. Upstream wraps; the
/// wrapped answer is meaningless either way, so widening is a strict
/// improvement rather than a divergence.
fn side_of_border_line(oct: &IntOctagon, x: i32, y: i32, line_no: usize) -> Side {
    let (x, y) = (x as i64, y as i64);
    let tmp = match line_no {
        0 => oct.bottom_y as i64 - y,
        2 => x - oct.right_x as i64,
        4 => y - oct.top_y as i64,
        6 => oct.left_x as i64 - x,
        1 => x - y - oct.lower_right_diagonal_x as i64,
        3 => x + y - oct.upper_right_diagonal_x as i64,
        5 => oct.upper_left_diagonal_x as i64 + y - x,
        _ => oct.lower_left_diagonal_x as i64 - x - y,
    };
    match tmp.cmp(&0) {
        std::cmp::Ordering::Less => Side::OnTheLeft,
        std::cmp::Ordering::Greater => Side::OnTheRight,
        std::cmp::Ordering::Equal => Side::Collinear,
    }
}

/// `ShapeSearchTree45Degree.obstacle_segment_touches_inside` (`:33-56`):
/// does the obstacle's border segment `line_no` cross the room's interior?
///
/// The segment runs from the obstacle's corner `line_no` to its corner
/// `line_no + 1`. The first corner must be strictly left of the room's border
/// lines `line_no … line_no + 4`, the second strictly left of `line_no + 5 …
/// line_no + 7` — five and three tests, together the eight half planes that
/// make up the room, split between the segment's two ends by which of them can
/// possibly violate which.
pub(crate) fn obstacle_segment_touches_inside(
    obstacle: &IntOctagon,
    line_no: usize,
    room: &IntOctagon,
) -> bool {
    let corner_x = obstacle.corner_x(line_no);
    let corner_y = obstacle.corner_y(line_no);
    let mut border_line_no = line_no;
    for _ in 0..5 {
        if side_of_border_line(room, corner_x, corner_y, border_line_no) != Side::OnTheLeft {
            return false;
        }
        border_line_no = (border_line_no + 1) % SIDE_COUNT;
    }

    let next = (line_no + 1) % SIDE_COUNT;
    let next_x = obstacle.corner_x(next);
    let next_y = obstacle.corner_y(next);
    let mut border_line_no = (line_no + 5) % SIDE_COUNT;
    for _ in 0..3 {
        if side_of_border_line(room, next_x, next_y, border_line_no) != Side::OnTheLeft {
            return false;
        }
        border_line_no = (border_line_no + 1) % SIDE_COUNT;
    }
    true
}

/// `ShapeSearchTree45Degree.signed_line_distance` (`:58-76`).
///
/// The separation of `contained` from the obstacle's border line `line_no`,
/// straight out of the eight offsets. **The diagonal arms are scaled by 0.5,
/// not by `1/√2`** — upstream's comment is explicit that this is a bias, "to
/// prefer orthogonal lines slightly to diagonal restraining lines", and the
/// choice of restraining line is what the whole tiling hangs on.
///
/// The value only ever feeds a `>` comparison against another value from this
/// same function, so `f64` here decides *which* line, never a coordinate: every
/// input is an integer difference below 2^32 and therefore exact in `f64`, and
/// the halving is exact too, so the comparison is an exact rational comparison
/// in disguise.
pub(crate) fn signed_line_distance(
    obstacle: &IntOctagon,
    line_no: usize,
    contained: &IntOctagon,
) -> f64 {
    let diff = |a: i32, b: i32| a as f64 - b as f64;
    match line_no {
        0 => diff(obstacle.bottom_y, contained.top_y),
        2 => diff(contained.left_x, obstacle.right_x),
        4 => diff(contained.bottom_y, obstacle.top_y),
        6 => diff(obstacle.left_x, contained.right_x),
        1 => {
            0.5 * diff(
                contained.upper_left_diagonal_x,
                obstacle.lower_right_diagonal_x,
            )
        }
        3 => {
            0.5 * diff(
                contained.lower_left_diagonal_x,
                obstacle.upper_right_diagonal_x,
            )
        }
        5 => {
            0.5 * diff(
                obstacle.upper_left_diagonal_x,
                contained.lower_right_diagonal_x,
            )
        }
        _ => {
            0.5 * diff(
                obstacle.lower_left_diagonal_x,
                contained.upper_right_diagonal_x,
            )
        }
    }
}

/// `ShapeSearchTree45Degree.calc_outside_restrained_shape` (`:355-379`): the
/// room cut with the half plane **outside** the obstacle's border line — one of
/// the eight offsets replaced, then normalized.
pub(crate) fn calc_outside_restrained_shape(
    obstacle: &IntOctagon,
    line_no: usize,
    room: &IntOctagon,
) -> IntOctagon {
    let mut o = *room;
    match line_no % SIDE_COUNT {
        0 => o.top_y = obstacle.bottom_y,
        2 => o.left_x = obstacle.right_x,
        4 => o.bottom_y = obstacle.top_y,
        6 => o.right_x = obstacle.left_x,
        1 => o.upper_left_diagonal_x = obstacle.lower_right_diagonal_x,
        3 => o.lower_left_diagonal_x = obstacle.upper_right_diagonal_x,
        5 => o.lower_right_diagonal_x = obstacle.upper_left_diagonal_x,
        _ => o.upper_right_diagonal_x = obstacle.lower_left_diagonal_x,
    }
    o.normalize()
}

/// `ShapeSearchTree45Degree.calc_inside_restrained_shape` (`:384-408`): the
/// complementary half plane, for the recursion's near piece.
pub(crate) fn calc_inside_restrained_shape(
    obstacle: &IntOctagon,
    line_no: usize,
    room: &IntOctagon,
) -> IntOctagon {
    let mut o = *room;
    match line_no % SIDE_COUNT {
        0 => o.bottom_y = obstacle.bottom_y,
        2 => o.right_x = obstacle.right_x,
        4 => o.top_y = obstacle.top_y,
        6 => o.left_x = obstacle.left_x,
        1 => o.lower_right_diagonal_x = obstacle.lower_right_diagonal_x,
        3 => o.upper_right_diagonal_x = obstacle.upper_right_diagonal_x,
        5 => o.upper_left_diagonal_x = obstacle.upper_left_diagonal_x,
        _ => o.lower_left_diagonal_x = obstacle.lower_left_diagonal_x,
    }
    o.normalize()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fr_geometry::planar::{IntBox, Limits};

    fn box_oct(lx: i32, ly: i32, rx: i32, uy: i32) -> IntOctagon {
        IntBox::new(lx, ly, rx, uy).to_int_octagon().normalize()
    }

    #[test]
    fn overlaps_needs_a_two_dimensional_intersection() {
        let a = box_oct(0, 0, 100, 100);
        assert!(octagon_overlaps(&a, &box_oct(50, 50, 150, 150)));
        // Edge contact is 1-dimensional, not an overlap.
        assert!(!octagon_overlaps(&a, &box_oct(100, 0, 200, 100)));
        assert!(!octagon_overlaps(&a, &box_oct(200, 200, 300, 300)));
    }

    #[test]
    fn contains_is_offset_domination() {
        let outer = box_oct(0, 0, 100, 100);
        assert!(octagon_contains(&outer, &box_oct(10, 10, 90, 90)));
        assert!(octagon_contains(&outer, &outer));
        assert!(!octagon_contains(&outer, &box_oct(10, 10, 110, 90)));
        assert!(octagon_contains(&outer, &IntOctagon::EMPTY));
    }

    /// The 0.5 diagonal bias, on the case it exists for: an orthogonal and a
    /// diagonal line at the same *true* separation must not tie — the
    /// orthogonal one must win.
    #[test]
    fn the_diagonal_distance_is_halved_not_divided_by_root_two() {
        let contained = box_oct(0, 0, 10, 10);
        // Line 0 is `obstacle.bottom_y - contained.top_y`; line 5 is
        // `0.5 * (obstacle.upper_left_diagonal_x - contained.lower_right_diagonal_x)`.
        // Both offset gaps are set to exactly 100. Built raw, without
        // `normalize`, because the function reads the eight offsets as given.
        let obstacle = IntOctagon::new(
            -1000,
            contained.top_y + 100,
            1000,
            1000,
            contained.lower_right_diagonal_x + 100,
            2000,
            -2000,
            2000,
        );
        let orthogonal = signed_line_distance(&obstacle, 0, &contained);
        let diagonal = signed_line_distance(&obstacle, 5, &contained);
        assert_eq!(orthogonal, 100.0);
        assert_eq!(diagonal, 50.0, "the diagonal offset gap of 100 halves");
        assert!(
            diagonal < orthogonal,
            "an equal offset gap must prefer the orthogonal line"
        );
        // 1/sqrt(2) would have given ~70.7 — still less, but the bias is
        // stronger than the geometric factor, which is the point.
        assert!(diagonal < 100.0 / std::f64::consts::SQRT_2);
    }

    #[test]
    fn the_outside_cut_moves_exactly_one_offset() {
        let room = box_oct(0, 0, 1000, 1000);
        let obstacle = box_oct(400, 400, 600, 600);
        // Line 0 is the obstacle's bottom edge: the outside half plane is
        // below it, so the room's top drops to the obstacle's bottom.
        let cut = calc_outside_restrained_shape(&obstacle, 0, &room);
        assert_eq!(cut.top_y, 400);
        assert_eq!(cut.bottom_y, 0);
        assert_eq!((cut.left_x, cut.right_x), (0, 1000));
        // Line 2 is the right edge: the room starts at the obstacle's right.
        let cut = calc_outside_restrained_shape(&obstacle, 2, &room);
        assert_eq!(cut.left_x, 600);
        assert_eq!(cut.right_x, 1000);
    }

    /// Outside and inside are complementary: their union covers the room and
    /// their interiors are disjoint. That is what makes the pass-2 recursion a
    /// partition rather than a duplication.
    #[test]
    fn outside_and_inside_partition_the_room() {
        let room = box_oct(0, 0, 1000, 1000);
        let obstacle = box_oct(400, 400, 600, 600);
        for line_no in 0..SIDE_COUNT {
            let outside = calc_outside_restrained_shape(&obstacle, line_no, &room);
            let inside = calc_inside_restrained_shape(&obstacle, line_no, &room);
            assert!(
                !octagon_overlaps(&outside, &inside),
                "line {line_no}: the two pieces overlap"
            );
            assert!(octagon_contains(&room, &outside), "line {line_no}");
            assert!(octagon_contains(&room, &inside), "line {line_no}");
            let total = outside.area() + inside.area();
            assert!(
                total >= room.area() - 1e-6,
                "line {line_no}: {total} < {}",
                room.area()
            );
        }
    }

    /// An obstacle strictly inside the room has all four of its box edges
    /// crossing the room's interior; one outside the room has none.
    #[test]
    fn segment_touches_inside_only_for_a_crossing_edge() {
        let room = box_oct(0, 0, 1000, 1000);
        let inside = box_oct(400, 400, 600, 600);
        for line_no in [0usize, 2, 4, 6] {
            assert!(
                obstacle_segment_touches_inside(&inside, line_no, &room),
                "line {line_no} of an enclosed obstacle must cross the room"
            );
        }
        let outside = box_oct(2000, 2000, 3000, 3000);
        for line_no in 0..SIDE_COUNT {
            assert!(!obstacle_segment_touches_inside(&outside, line_no, &room));
        }
    }

    /// The side test must not wrap when a relaxed room carries `±CRIT_INT`
    /// offsets — the case `remove_not_touching_border_lines` produces.
    #[test]
    fn the_side_test_survives_the_coordinate_envelope() {
        let huge = IntOctagon::new(
            -Limits::CRIT_INT,
            -Limits::CRIT_INT,
            Limits::CRIT_INT,
            Limits::CRIT_INT,
            -Limits::CRIT_INT,
            Limits::CRIT_INT,
            -Limits::CRIT_INT,
            Limits::CRIT_INT,
        );
        // Every corner of a small obstacle is strictly inside the envelope.
        for line_no in 0..SIDE_COUNT {
            assert_eq!(
                side_of_border_line(&huge, 1000, -1000, line_no),
                Side::OnTheLeft,
                "line {line_no}"
            );
        }
    }
}
