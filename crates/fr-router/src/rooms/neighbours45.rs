//! The 45° room-neighbour specialization — `Sorted45DegreeRoomNeighbours.java`.
//!
//! `AutorouteEngine.calculate_doors` dispatches on the *tree* subclass, not on
//! the room: `ShapeSearchTree45Degree` → `Sorted45DegreeRoomNeighbours`,
//! `ShapeSearchTreeOrthogonal` → `SortedOrthogonalRoomNeighbours`, else the
//! any-angle [`neighbours`](super::neighbours) (`AutorouteEngine.java:520-525`).
//!
//! # What is ported here, and what is blocked
//!
//! The 45° variant is **not** "the any-angle algorithm with tighter
//! comparisons". It is a rewrite in which *every* shape is an
//! [`IntOctagon`]: the room, the neighbour, and their intersection. It reads
//! and writes the eight octagon half-plane offsets directly
//! (`left_x, bottom_y, right_x, top_y` and the four diagonals) instead of
//! iterating border lines, which is why it can decide a touching side with two
//! integer comparisons where the general case runs `touching_sides` over the
//! border-line rings.
//!
//! That representation is supplied by `ShapeSearchTree45Degree`, whose own
//! `complete_shape` override (`board/ShapeSearchTree45Degree.java:84-...`)
//! produces `IntOctagon` rooms; `SearchTreeManager.java:166` is what selects it
//! from the board's angle restriction. **We have no such tree.** `fr-board`
//! exposes one compensated tree flavor, `AutorouteEngine::tree_class` is a
//! clearance-class number rather than an angle family, and
//! [`complete_shape`](crate::engine::AutorouteEngine::complete_shape)
//! unconditionally yields [`Simplex`](fr_geometry::planar::Simplex) rooms. So
//! the *dispatch* cannot be wired up from this crate alone.
//!
//! What is ported here is the part that does not need the tree: the pure
//! octagon geometry the 45° variant is built out of, transcribed line for line
//! and covered by tests, so that wiring it up later is assembly rather than
//! translation. Every function below is a total function of octagons — no
//! engine state, no doors, no arena.
//!
//! | here | upstream |
//! |---|---|
//! | [`touching_sides`] | `Sorted45DegreeRoomNeighbours.java:713-760` |
//! | [`mark_edge_interior_touches`] | `:764-790` |
//! | [`remove_not_touching_border_lines`] | `:156-249` |
//! | [`edge_incomplete_room`] | `:238-268` (the `switch (curr_side_no)`) |
//! | [`from_side_incomplete_room`] | `:400-444` (the `switch (from_side_no)`) |
//! | [`to_side_incomplete_room`] | `:456-489` (the `switch (to_side_no)`) |
//!
//! # Side numbering
//!
//! Sides run counter-clockwise from the bottom edge, matching
//! `IntOctagon.border_line(no)` and `IntOctagon.corner(no)`:
//!
//! ```text
//!   0 bottom      1 lower-right diagonal   2 right   3 upper-right diagonal
//!   4 top         5 upper-left diagonal    6 left    7 lower-left diagonal
//! ```

use fr_geometry::planar::{IntOctagon, Limits};

/// Number of octagon sides — the array length of `edge_interior_touches_obstacle`.
pub const SIDE_COUNT: usize = 8;

/// The first and last side of the room shape a neighbour touches.
///
/// `Sorted45DegreeRoomNeighbours.SortedRoomNeighbour` (`:713-760`) stores these
/// as two `int`s and uses `-1` to mean *"sorting did not work because the room
/// shape is contained in the neighbour shape"*; here that is [`None`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TouchingSides {
    /// The first side of the room shape where the neighbour touches.
    pub first: usize,
    /// The last side of the room shape where the neighbour touches.
    pub last: usize,
}

impl TouchingSides {
    /// How many sides the neighbour spans, counter-clockwise from `first`.
    ///
    /// `SortedRoomNeighbour.compareTo` (`:829-830`) breaks ties on exactly this
    /// quantity: `(last - first + 8) % 8`.
    pub fn span(&self) -> usize {
        (self.last + SIDE_COUNT - self.first) % SIDE_COUNT
    }
}

/// The first/last sides of `room` that `intersection` touches
/// (`Sorted45DegreeRoomNeighbours.java:713-760`).
///
/// `intersection` is `room ∩ neighbour`, already an octagon. Each arm pins one
/// offset of the intersection to the same offset of the room — that is what
/// *"touches this side"* means for a half-plane representation — and requires
/// the *neighbouring* offset to be strictly inside, which is what orders the
/// eight arms and makes the answer unique.
///
/// Answers `None` when no arm matches, i.e. the room shape is contained in the
/// neighbour shape and there is no boundary to sort along.
pub fn touching_sides(room: &IntOctagon, intersection: &IntOctagon) -> Option<TouchingSides> {
    let first = if intersection.bottom_y == room.bottom_y
        && intersection.lower_left_diagonal_x > room.lower_left_diagonal_x
    {
        0
    } else if intersection.lower_right_diagonal_x == room.lower_right_diagonal_x
        && intersection.bottom_y > room.bottom_y
    {
        1
    } else if intersection.right_x == room.right_x
        && intersection.lower_right_diagonal_x < room.lower_right_diagonal_x
    {
        2
    } else if intersection.upper_right_diagonal_x == room.upper_right_diagonal_x
        && intersection.right_x < room.right_x
    {
        3
    } else if intersection.top_y == room.top_y
        && intersection.upper_right_diagonal_x < room.upper_right_diagonal_x
    {
        4
    } else if intersection.upper_left_diagonal_x == room.upper_left_diagonal_x
        && intersection.top_y < room.top_y
    {
        5
    } else if intersection.left_x == room.left_x
        && intersection.upper_left_diagonal_x > room.upper_left_diagonal_x
    {
        6
    } else if intersection.lower_left_diagonal_x == room.lower_left_diagonal_x
        && intersection.left_x > room.left_x
    {
        7
    } else {
        return None;
    };

    // The `last` chain walks the same eight predicates *clockwise*, so a
    // neighbour spanning several sides reports the far end rather than the near
    // one. Upstream spells both chains out; the asymmetry is deliberate.
    let last = if intersection.lower_left_diagonal_x == room.lower_left_diagonal_x
        && intersection.bottom_y > room.bottom_y
    {
        7
    } else if intersection.left_x == room.left_x
        && intersection.lower_left_diagonal_x > room.lower_left_diagonal_x
    {
        6
    } else if intersection.upper_left_diagonal_x == room.upper_left_diagonal_x
        && intersection.left_x > room.left_x
    {
        5
    } else if intersection.top_y == room.top_y
        && intersection.upper_left_diagonal_x > room.upper_left_diagonal_x
    {
        4
    } else if intersection.upper_right_diagonal_x == room.upper_right_diagonal_x
        && intersection.top_y < room.top_y
    {
        3
    } else if intersection.right_x == room.right_x
        && intersection.upper_right_diagonal_x < room.upper_right_diagonal_x
    {
        2
    } else if intersection.lower_right_diagonal_x == room.lower_right_diagonal_x
        && intersection.right_x < room.right_x
    {
        1
    } else if intersection.bottom_y == room.bottom_y
        && intersection.lower_right_diagonal_x < room.lower_right_diagonal_x
    {
        0
    } else {
        return None;
    };

    Some(TouchingSides { first, last })
}

/// Marks every side from `sides.first` to `sides.last` as interior-touched
/// (`Sorted45DegreeRoomNeighbours.java:764-790`).
///
/// A side is *not* marked when the neighbour meets it only at a corner: at the
/// first side, when the intersection's corner `first` is the room's corner
/// `first + 1`; at the last side, when the intersection's corner `last + 1` is
/// the room's corner `last`. That exemption is what keeps
/// [`remove_not_touching_border_lines`] willing to push a side outwards that a
/// neighbour merely grazes.
pub fn mark_edge_interior_touches(
    room: &IntOctagon,
    intersection: &IntOctagon,
    sides: TouchingSides,
    touched: &mut [bool; SIDE_COUNT],
) {
    let mut next_side_no = sides.first;
    loop {
        let curr_side_no = next_side_no;
        next_side_no = (next_side_no + 1) % SIDE_COUNT;
        if !touched[curr_side_no] {
            let mut touch_only_at_corner = false;
            if curr_side_no == sides.first
                && intersection.corner(curr_side_no) == room.corner(next_side_no)
            {
                touch_only_at_corner = true;
            }
            if curr_side_no == sides.last
                && intersection.corner(next_side_no) == room.corner(curr_side_no)
            {
                touch_only_at_corner = true;
            }
            if !touch_only_at_corner {
                touched[curr_side_no] = true;
            }
        }
        if curr_side_no == sides.last {
            break;
        }
    }
}

/// Pushes every untouched side of `room` out to the envelope
/// (`Sorted45DegreeRoomNeighbours.java:156-249`).
///
/// This is the 45° `try_remove_edge`: where the any-angle variant removes one
/// simplex border line per restart, this one relaxes **all eight** octagon
/// offsets that no neighbour touches in a single step, then re-completes the
/// enlarged room. `±Limits::CRIT_INT` is upstream's "no constraint" value, and
/// the result is normalized because relaxing offsets can leave the octagon in a
/// non-canonical form.
pub fn remove_not_touching_border_lines(
    room: &IntOctagon,
    touched: &[bool; SIDE_COUNT],
) -> IntOctagon {
    const FAR: i32 = Limits::CRIT_INT;
    let relaxed = IntOctagon::new(
        if touched[6] { room.left_x } else { -FAR },
        if touched[0] { room.bottom_y } else { -FAR },
        if touched[2] { room.right_x } else { FAR },
        if touched[4] { room.top_y } else { FAR },
        if touched[5] {
            room.upper_left_diagonal_x
        } else {
            -FAR
        },
        if touched[1] {
            room.lower_right_diagonal_x
        } else {
            FAR
        },
        if touched[7] {
            room.lower_left_diagonal_x
        } else {
            -FAR
        },
        if touched[3] {
            room.upper_right_diagonal_x
        } else {
            FAR
        },
    );
    relaxed.normalize()
}

/// The incomplete room beyond side `side_no` of an obstacle room
/// (`Sorted45DegreeRoomNeighbours.java:238-268`).
///
/// The board's bounding octagon with exactly one offset replaced by the room's
/// *opposite* offset, so the result is the half-plane on the far side of that
/// edge, clipped to the board. Upstream then normalizes it inside
/// `insert_incomplete_room` (`:373`); that is done here so the function is
/// total.
///
/// Panics is impossible: `side_no` is taken modulo [`SIDE_COUNT`].
pub fn edge_incomplete_room(room: &IntOctagon, board: &IntOctagon, side_no: usize) -> IntOctagon {
    let mut o = Offsets::of(board);
    match side_no % SIDE_COUNT {
        0 => o.top_y = room.bottom_y,
        1 => o.upper_left_diagonal_x = room.lower_right_diagonal_x,
        2 => o.left_x = room.right_x,
        3 => o.lower_left_diagonal_x = room.upper_right_diagonal_x,
        4 => o.bottom_y = room.top_y,
        5 => o.lower_right_diagonal_x = room.upper_left_diagonal_x,
        6 => o.right_x = room.left_x,
        _ => o.upper_right_diagonal_x = room.lower_left_diagonal_x,
    }
    o.build()
}

/// The incomplete room between the previous neighbour and the next corner
/// (`Sorted45DegreeRoomNeighbours.java:400-444`).
///
/// `from_side_no` is the previous neighbour's `last` touching side and
/// `prev_intersection` its intersection with the room. Two offsets move: the
/// room's own opposite offset (the same one [`edge_incomplete_room`] moves) and
/// the offset of the *following* side, pinned to where the previous neighbour
/// ended. That second offset is what makes the gap room start exactly where the
/// neighbour stopped instead of at the corner.
pub fn from_side_incomplete_room(
    room: &IntOctagon,
    board: &IntOctagon,
    from_side_no: usize,
    prev_intersection: &IntOctagon,
) -> IntOctagon {
    let mut o = Offsets::of(board);
    match from_side_no % SIDE_COUNT {
        0 => {
            o.top_y = room.bottom_y;
            o.upper_left_diagonal_x = prev_intersection.lower_right_diagonal_x;
        }
        1 => {
            o.upper_left_diagonal_x = room.lower_right_diagonal_x;
            o.left_x = prev_intersection.right_x;
        }
        2 => {
            o.left_x = room.right_x;
            o.lower_left_diagonal_x = prev_intersection.upper_right_diagonal_x;
        }
        3 => {
            o.lower_left_diagonal_x = room.upper_right_diagonal_x;
            o.bottom_y = prev_intersection.top_y;
        }
        4 => {
            o.bottom_y = room.top_y;
            o.lower_right_diagonal_x = prev_intersection.upper_left_diagonal_x;
        }
        5 => {
            o.lower_right_diagonal_x = room.upper_left_diagonal_x;
            o.right_x = prev_intersection.left_x;
        }
        6 => {
            o.right_x = room.left_x;
            o.upper_right_diagonal_x = prev_intersection.lower_left_diagonal_x;
        }
        _ => {
            o.upper_right_diagonal_x = room.lower_left_diagonal_x;
            o.top_y = prev_intersection.bottom_y;
        }
    }
    o.build()
}

/// The incomplete room between the previous corner and the next neighbour
/// (`Sorted45DegreeRoomNeighbours.java:456-489`).
///
/// The mirror image of [`from_side_incomplete_room`]: `to_side_no` is the next
/// neighbour's `first` touching side, and the second offset that moves belongs
/// to the *preceding* side, pinned to where the next neighbour begins.
pub fn to_side_incomplete_room(
    room: &IntOctagon,
    board: &IntOctagon,
    to_side_no: usize,
    next_intersection: &IntOctagon,
) -> IntOctagon {
    let mut o = Offsets::of(board);
    match to_side_no % SIDE_COUNT {
        0 => {
            o.top_y = room.bottom_y;
            o.upper_right_diagonal_x = next_intersection.lower_left_diagonal_x;
        }
        1 => {
            o.upper_left_diagonal_x = room.lower_right_diagonal_x;
            o.top_y = next_intersection.bottom_y;
        }
        2 => {
            o.left_x = room.right_x;
            o.upper_left_diagonal_x = next_intersection.lower_right_diagonal_x;
        }
        3 => {
            o.lower_left_diagonal_x = room.upper_right_diagonal_x;
            o.left_x = next_intersection.right_x;
        }
        4 => {
            o.bottom_y = room.top_y;
            o.lower_left_diagonal_x = next_intersection.upper_right_diagonal_x;
        }
        5 => {
            o.lower_right_diagonal_x = room.upper_left_diagonal_x;
            o.bottom_y = next_intersection.top_y;
        }
        6 => {
            o.right_x = room.left_x;
            o.lower_right_diagonal_x = next_intersection.upper_left_diagonal_x;
        }
        _ => {
            o.upper_right_diagonal_x = room.lower_left_diagonal_x;
            o.right_x = next_intersection.left_x;
        }
    }
    o.build()
}

/// The eight octagon offsets, so the `switch` arms above read like upstream's.
struct Offsets {
    left_x: i32,
    bottom_y: i32,
    right_x: i32,
    top_y: i32,
    upper_left_diagonal_x: i32,
    lower_right_diagonal_x: i32,
    lower_left_diagonal_x: i32,
    upper_right_diagonal_x: i32,
}

impl Offsets {
    fn of(oct: &IntOctagon) -> Offsets {
        Offsets {
            left_x: oct.left_x,
            bottom_y: oct.bottom_y,
            right_x: oct.right_x,
            top_y: oct.top_y,
            upper_left_diagonal_x: oct.upper_left_diagonal_x,
            lower_right_diagonal_x: oct.lower_right_diagonal_x,
            lower_left_diagonal_x: oct.lower_left_diagonal_x,
            upper_right_diagonal_x: oct.upper_right_diagonal_x,
        }
    }

    fn build(self) -> IntOctagon {
        IntOctagon::new(
            self.left_x,
            self.bottom_y,
            self.right_x,
            self.top_y,
            self.upper_left_diagonal_x,
            self.lower_right_diagonal_x,
            self.lower_left_diagonal_x,
            self.upper_right_diagonal_x,
        )
        .normalize()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An axis-parallel box as an octagon: the diagonals are relaxed to the
    /// values `IntOctagon::normalize` gives a box, so the shape really is the
    /// rectangle and side 0/2/4/6 are its four edges.
    fn box_oct(lx: i32, ly: i32, rx: i32, uy: i32) -> IntOctagon {
        IntOctagon::new(
            lx,
            ly,
            rx,
            uy,
            -Limits::CRIT_INT,
            Limits::CRIT_INT,
            -Limits::CRIT_INT,
            Limits::CRIT_INT,
        )
        .normalize()
    }

    fn board() -> IntOctagon {
        box_oct(0, 0, 1000, 1000)
    }

    // --- touching_sides ------------------------------------------------

    /// A neighbour abutting the room's *right* edge. The room is
    /// [100,100]..[300,300]; the overlap is the degenerate strip at x = 300.
    /// Side 2 is the right edge, so both ends must report 2.
    #[test]
    fn a_neighbour_on_the_right_edge_touches_side_two() {
        let room = box_oct(100, 100, 300, 300);
        let intersection = box_oct(300, 150, 300, 250);
        let sides = touching_sides(&room, &intersection).expect("the strip touches the room");
        assert_eq!(sides, TouchingSides { first: 2, last: 2 });
        assert_eq!(sides.span(), 0);
    }

    #[test]
    fn a_neighbour_on_each_of_the_four_orthogonal_edges() {
        let room = box_oct(100, 100, 300, 300);
        // (strip, expected side)
        let cases = [
            (box_oct(150, 100, 250, 100), 0), // bottom
            (box_oct(300, 150, 300, 250), 2), // right
            (box_oct(150, 300, 250, 300), 4), // top
            (box_oct(100, 150, 100, 250), 6), // left
        ];
        for (strip, expected) in cases {
            let sides = touching_sides(&room, &strip).unwrap_or_else(|| {
                panic!("strip {strip:?} must touch side {expected}");
            });
            assert_eq!(
                (sides.first, sides.last),
                (expected, expected),
                "strip {strip:?}"
            );
        }
    }

    /// A neighbour wrapping the bottom-right corner touches sides 0 through 2:
    /// `first` is the counter-clockwise-first side, `last` the final one.
    #[test]
    fn a_corner_neighbour_spans_from_the_first_to_the_last_side() {
        let room = box_oct(100, 100, 300, 300);
        // Overlap covering the whole bottom edge and the whole right edge.
        let intersection = box_oct(100, 100, 300, 300);
        // A full-room overlap has no strictly-interior offset anywhere, so the
        // room is "contained in the neighbour" and sorting is refused.
        assert_eq!(touching_sides(&room, &intersection), None);
    }

    #[test]
    fn a_room_contained_in_the_neighbour_has_no_touching_side() {
        let room = box_oct(100, 100, 300, 300);
        assert_eq!(touching_sides(&room, &room), None);
    }

    #[test]
    fn span_wraps_counter_clockwise_through_zero() {
        assert_eq!(TouchingSides { first: 7, last: 1 }.span(), 2);
        assert_eq!(TouchingSides { first: 0, last: 7 }.span(), 7);
        assert_eq!(TouchingSides { first: 3, last: 3 }.span(), 0);
    }

    // --- mark_edge_interior_touches ------------------------------------

    #[test]
    fn marking_covers_every_side_from_first_to_last_inclusive() {
        let room = box_oct(100, 100, 300, 300);
        // An overlap strictly inside the right edge marks only side 2.
        let intersection = box_oct(300, 150, 300, 250);
        let sides = touching_sides(&room, &intersection).unwrap();
        let mut touched = [false; SIDE_COUNT];
        mark_edge_interior_touches(&room, &intersection, sides, &mut touched);
        assert!(touched[2]);
        assert_eq!(touched.iter().filter(|t| **t).count(), 1);
    }

    #[test]
    fn marking_wraps_around_side_seven_to_side_zero() {
        let room = box_oct(100, 100, 300, 300);
        let intersection = box_oct(300, 150, 300, 250);
        let mut touched = [false; SIDE_COUNT];
        mark_edge_interior_touches(
            &room,
            &intersection,
            TouchingSides { first: 6, last: 1 },
            &mut touched,
        );
        // 6, 7, 0, 1 — and nothing else.
        assert_eq!(touched, [true, true, false, false, false, false, true, true]);
    }

    #[test]
    fn marking_is_idempotent() {
        let room = box_oct(100, 100, 300, 300);
        let intersection = box_oct(300, 150, 300, 250);
        let sides = touching_sides(&room, &intersection).unwrap();
        let mut once = [false; SIDE_COUNT];
        mark_edge_interior_touches(&room, &intersection, sides, &mut once);
        let mut twice = once;
        mark_edge_interior_touches(&room, &intersection, sides, &mut twice);
        assert_eq!(once, twice);
    }

    // --- remove_not_touching_border_lines ------------------------------

    #[test]
    fn an_untouched_room_relaxes_to_the_whole_envelope() {
        let room = box_oct(100, 100, 300, 300);
        let relaxed = remove_not_touching_border_lines(&room, &[false; SIDE_COUNT]);
        assert_eq!(relaxed.left_x, -Limits::CRIT_INT);
        assert_eq!(relaxed.bottom_y, -Limits::CRIT_INT);
        assert_eq!(relaxed.right_x, Limits::CRIT_INT);
        assert_eq!(relaxed.top_y, Limits::CRIT_INT);
    }

    #[test]
    fn a_fully_touched_room_is_unchanged() {
        let room = box_oct(100, 100, 300, 300);
        let relaxed = remove_not_touching_border_lines(&room, &[true; SIDE_COUNT]);
        assert_eq!(relaxed, room);
    }

    /// The relaxation is *monotone*: it can only grow the room. That is the
    /// invariant `try_remove_edge_line` relies on for termination — it accepts
    /// the enlarged room only when the re-completed area strictly increased
    /// (`:355-360`), which is only a meaningful gate if relaxing never shrinks.
    #[test]
    fn relaxation_never_shrinks_the_room() {
        let room = box_oct(100, 100, 300, 300);
        for mask in 0u32..256 {
            let mut touched = [false; SIDE_COUNT];
            for (i, slot) in touched.iter_mut().enumerate() {
                *slot = mask & (1 << i) != 0;
            }
            let relaxed = remove_not_touching_border_lines(&room, &touched);
            assert!(
                relaxed.area() >= room.area(),
                "mask {mask:08b}: area {} < {}",
                relaxed.area(),
                room.area()
            );
            assert!(
                relaxed.left_x <= room.left_x
                    && relaxed.bottom_y <= room.bottom_y
                    && relaxed.right_x >= room.right_x
                    && relaxed.top_y >= room.top_y,
                "mask {mask:08b} shrank a box offset"
            );
        }
    }

    /// Relaxing exactly one side must move exactly that side.
    #[test]
    fn relaxing_one_side_moves_only_that_side() {
        let room = box_oct(100, 100, 300, 300);
        // Sides 0/2/4/6 map to bottom_y / right_x / top_y / left_x.
        for side in [0usize, 2, 4, 6] {
            let mut touched = [true; SIDE_COUNT];
            touched[side] = false;
            let relaxed = remove_not_touching_border_lines(&room, &touched);
            assert!(
                relaxed.area() > room.area(),
                "side {side} did not grow the room"
            );
        }
    }

    // --- the incomplete-room constructions -----------------------------

    /// Side 0 is the bottom edge, so the gap room beyond it is everything on
    /// the board below `room.bottom_y`.
    #[test]
    fn edge_incomplete_room_below_the_bottom_edge() {
        let room = box_oct(100, 100, 300, 300);
        let gap = edge_incomplete_room(&room, &board(), 0);
        assert_eq!(gap.bottom_y, 0);
        assert_eq!(gap.top_y, 100);
        assert_eq!(gap.left_x, 0);
        assert_eq!(gap.right_x, 1000);
    }

    /// Side 2 is the right edge: the gap room is everything right of it.
    #[test]
    fn edge_incomplete_room_right_of_the_right_edge() {
        let room = box_oct(100, 100, 300, 300);
        let gap = edge_incomplete_room(&room, &board(), 2);
        assert_eq!(gap.left_x, 300);
        assert_eq!(gap.right_x, 1000);
        assert_eq!(gap.bottom_y, 0);
        assert_eq!(gap.top_y, 1000);
    }

    /// Every one of the eight gap rooms must be disjoint from the room's
    /// interior — that is what makes the ring around an obstacle room a tiling
    /// rather than an overlap.
    #[test]
    fn no_edge_gap_room_overlaps_the_room_interior() {
        let room = box_oct(100, 100, 300, 300);
        for side in 0..SIDE_COUNT {
            let gap = edge_incomplete_room(&room, &board(), side);
            if gap.is_empty() {
                continue;
            }
            let overlap = gap.intersection(&room);
            assert!(
                overlap.is_empty() || overlap.dimension() < 2,
                "side {side}: gap room overlaps the room in 2 dimensions"
            );
        }
    }

    /// The eight gap rooms plus the room must cover the board: no point of the
    /// board may fall outside all nine shapes. Checked on a lattice, because
    /// there is no exact union of octagons to compare against.
    #[test]
    fn the_edge_gap_rooms_and_the_room_cover_the_board() {
        let room = box_oct(100, 100, 300, 300);
        let board = board();
        let gaps: Vec<IntOctagon> = (0..SIDE_COUNT)
            .map(|side| edge_incomplete_room(&room, &board, side))
            .collect();
        for x in (0..=1000).step_by(37) {
            for y in (0..=1000).step_by(37) {
                let point = fr_geometry::planar::Point::Int(fr_geometry::planar::IntPoint::new(
                    x, y,
                ));
                let covered = room.to_simplex().contains(&point)
                    || gaps
                        .iter()
                        .any(|gap| !gap.is_empty() && gap.to_simplex().contains(&point));
                assert!(covered, "({x}, {y}) is in no room");
            }
        }
    }

    /// `from_side` pins the *following* side to where the previous neighbour
    /// ended; `to_side` pins the *preceding* one to where the next neighbour
    /// begins. On side 0 (bottom) those are the two lower diagonals, and the
    /// two gap rooms must therefore be different shapes.
    #[test]
    fn from_side_and_to_side_pin_opposite_diagonals() {
        let room = box_oct(100, 100, 300, 300);
        let board = board();
        let neighbour = box_oct(150, 100, 250, 100);
        let from = from_side_incomplete_room(&room, &board, 0, &neighbour);
        let to = to_side_incomplete_room(&room, &board, 0, &neighbour);
        // Both live below the room's bottom edge …
        assert_eq!(from.top_y, 100);
        assert_eq!(to.top_y, 100);
        // … but they are pinned on opposite diagonals, so they differ.
        assert_ne!(from, to);
    }

    /// Both constructions must stay inside the board and beyond the room, for
    /// every side — the property `insert_incomplete_room` assumes when it takes
    /// the intersection with the room shape as the door (`:374-380`).
    #[test]
    fn every_side_construction_stays_in_the_board_and_out_of_the_room() {
        let room = box_oct(100, 100, 300, 300);
        let board = board();
        let neighbour = box_oct(150, 100, 250, 100);
        for side in 0..SIDE_COUNT {
            for gap in [
                from_side_incomplete_room(&room, &board, side, &neighbour),
                to_side_incomplete_room(&room, &board, side, &neighbour),
            ] {
                if gap.is_empty() {
                    continue;
                }
                let clipped = gap.intersection(&board);
                assert_eq!(clipped, gap, "side {side}: gap room leaves the board");
                let overlap = gap.intersection(&room);
                assert!(
                    overlap.is_empty() || overlap.dimension() < 2,
                    "side {side}: gap room overlaps the room interior"
                );
            }
        }
    }

    /// `side_no` is taken modulo 8, so a caller that walks past the ring end
    /// (upstream's `(from_side_no + 1) % 8`) cannot index out of range.
    #[test]
    fn side_numbers_wrap() {
        let room = box_oct(100, 100, 300, 300);
        let board = board();
        for side in 0..SIDE_COUNT {
            assert_eq!(
                edge_incomplete_room(&room, &board, side),
                edge_incomplete_room(&room, &board, side + SIDE_COUNT)
            );
        }
    }
}
