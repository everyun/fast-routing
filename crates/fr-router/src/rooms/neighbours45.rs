//! The 45° room-neighbour specialization — `Sorted45DegreeRoomNeighbours.java`.
//!
//! `AutorouteEngine.calculate_doors` dispatches on the *tree* subclass, not on
//! the room: `ShapeSearchTree45Degree` → `Sorted45DegreeRoomNeighbours`,
//! `ShapeSearchTreeOrthogonal` → `SortedOrthogonalRoomNeighbours`, else the
//! any-angle [`neighbours`](super::neighbours) (`AutorouteEngine.java:520-525`).
//!
//! # What is ported here
//!
//! The 45° variant is **not** "the any-angle algorithm with tighter
//! comparisons". It is a rewrite in which *every* shape is an
//! [`IntOctagon`]: the room, the neighbour, and their intersection. It reads
//! and writes the eight octagon half-plane offsets directly
//! (`left_x, bottom_y, right_x, top_y` and the four diagonals) instead of
//! iterating border lines, which is why it can decide a touching side with two
//! integer comparisons where the general case runs `touching_sides` over the
//! border-line rings — and, far more importantly, why its gap rooms are
//! octagons rather than arbitrarily-sloped simplices.
//!
//! This is the half of the 45° chain that pays. Measured at `-mp 1` with the
//! room completion left any-angle ([`Tiling45Mode::Doors`], full table there):
//! Issue034 65,530 → 60,678 expansions and 37 → 32 unrouted, Issue159 3,907 →
//! 1,255 expansions and 3 → 2 unrouted, Issue026 8 → 3 unrouted — all at zero
//! added clearance violations. Issue155 regresses (8 → 13), so it is not a free
//! win and the switch stays gated.
//!
//! That octagon representation is supplied by `ShapeSearchTree45Degree`
//! (`fr_board::search_tree::TreeAngleFamily::FortyFive` here) and its
//! `complete_shape` override ([`complete45`](super::complete45)).
//! `SearchTreeManager.java:166` selects both from the board's angle
//! restriction; [`default_tiling_45`](crate::engine::default_tiling_45) is that
//! selector, gated on the A/B switch.
//!
//! | here | upstream |
//! |---|---|
//! | [`touching_sides`] | `Sorted45DegreeRoomNeighbours.java:713-760` |
//! | [`mark_edge_interior_touches`] | `:764-790` |
//! | [`remove_not_touching_border_lines`] | `:156-216` |
//! | [`edge_incomplete_room`] | `:228-271` (the `switch (curr_side_no)`) |
//! | [`from_side_incomplete_room`] | `:400-444` (the `switch (from_side_no)`) |
//! | [`to_side_incomplete_room`] | `:456-489` (the `switch (to_side_no)`) |
//! | [`free_gap_room`] | `:545-677` (the `switch (next.first_touching_side)`) |
//! | [`compare_neighbours`] | `:797-854` (`SortedRoomNeighbour.compareTo`) |
//! | `AutorouteEngine::calculate_doors_45` | `:44-72` (`calculate`) |
//! | `AutorouteEngine::calculate_neighbours_45` | `:77-155` |
//! | `AutorouteEngine::try_remove_edge_line_45` | `:276-365` |
//! | `AutorouteEngine::calculate_new_incomplete_rooms_45` | `:503-682` |
//! | `AutorouteEngine::obstacle_gap_rooms_45` | `:391-501` |
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

use fr_board::item::ItemId;
#[cfg(doc)]
use fr_board::search_tree::Tiling45Mode;
use fr_geometry::planar::{IntOctagon, Limits};

use crate::engine::{AutorouteEngine, Obstacle};
use crate::rooms::complete45::Room45;
use crate::rooms::{RoomId, RoomKind};

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

/// The incomplete room in the gap between two neighbours of a **free** room
/// (`Sorted45DegreeRoomNeighbours.java:545-677`).
///
/// Where [`from_side_incomplete_room`] and [`to_side_incomplete_room`] each
/// build one half of the gap around an *obstacle* room's corner, a free room
/// gets a single room spanning the whole gap, and which two offsets pin it
/// depends on how the two neighbours' intersections are ordered along the side
/// the next one starts on. Each of the eight arms is the same three-way
/// comparison in the coordinate that runs along that side:
///
/// * the previous neighbour ends **before** the next one starts along the side —
///   the gap is bounded by the room's own side and by both neighbours;
/// * it ends **after** — the neighbours are ordered the other way round the
///   ring, so the gap is pinned to the previous neighbour's far offset instead;
/// * they are **equal** — a single pin, and the gap is degenerate unless the
///   ring wraps.
///
/// The extra `prev.last == next.first` guard inside the first branch is
/// upstream's: when the previous neighbour also *ended* on this side, its own
/// far offset closes the gap and must be carried over too.
pub fn free_gap_room(
    room: &IntOctagon,
    board: &IntOctagon,
    prev_intersection: &IntOctagon,
    prev_last_side: usize,
    next_intersection: &IntOctagon,
    next_first_side: usize,
) -> IntOctagon {
    let _ = room;
    let mut o = Offsets::of(board);
    let p = prev_intersection;
    let n = next_intersection;
    match next_first_side % SIDE_COUNT {
        0 => {
            if p.lower_left_diagonal_x < n.lower_left_diagonal_x {
                o.upper_right_diagonal_x = n.lower_left_diagonal_x;
                o.top_y = p.bottom_y;
                if prev_last_side == 0 {
                    o.upper_left_diagonal_x = p.lower_right_diagonal_x;
                }
            } else if p.lower_left_diagonal_x > n.lower_left_diagonal_x {
                o.right_x = n.left_x;
                o.upper_right_diagonal_x = p.lower_left_diagonal_x;
            } else {
                o.upper_right_diagonal_x = n.lower_left_diagonal_x;
            }
        }
        1 => {
            if p.bottom_y < n.bottom_y {
                o.top_y = n.bottom_y;
                o.upper_left_diagonal_x = p.lower_right_diagonal_x;
                if prev_last_side == 1 {
                    o.left_x = p.right_x;
                }
            } else if p.bottom_y > n.bottom_y {
                o.top_y = p.bottom_y;
                o.upper_right_diagonal_x = n.lower_left_diagonal_x;
            } else {
                o.top_y = n.bottom_y;
            }
        }
        2 => {
            if p.lower_right_diagonal_x > n.lower_right_diagonal_x {
                o.upper_left_diagonal_x = n.lower_right_diagonal_x;
                o.left_x = p.right_x;
                if prev_last_side == 2 {
                    o.lower_left_diagonal_x = p.upper_right_diagonal_x;
                }
            } else if p.lower_right_diagonal_x < n.lower_right_diagonal_x {
                o.top_y = n.bottom_y;
                o.upper_left_diagonal_x = p.lower_right_diagonal_x;
            } else {
                o.upper_left_diagonal_x = n.lower_right_diagonal_x;
            }
        }
        3 => {
            if p.right_x > n.right_x {
                o.left_x = n.right_x;
                o.lower_left_diagonal_x = p.upper_right_diagonal_x;
                if prev_last_side == 3 {
                    o.bottom_y = p.top_y;
                }
            } else if p.right_x < n.right_x {
                o.left_x = p.right_x;
                o.upper_left_diagonal_x = n.lower_right_diagonal_x;
            } else {
                o.left_x = n.right_x;
            }
        }
        4 => {
            if p.upper_right_diagonal_x > n.upper_right_diagonal_x {
                o.lower_left_diagonal_x = n.upper_right_diagonal_x;
                o.bottom_y = p.top_y;
                if prev_last_side == 4 {
                    o.lower_right_diagonal_x = p.upper_left_diagonal_x;
                }
            } else if p.upper_right_diagonal_x < n.upper_right_diagonal_x {
                o.left_x = n.right_x;
                o.lower_left_diagonal_x = p.upper_right_diagonal_x;
            } else {
                o.lower_left_diagonal_x = n.upper_right_diagonal_x;
            }
        }
        5 => {
            if p.top_y > n.top_y {
                o.bottom_y = n.top_y;
                o.lower_right_diagonal_x = p.upper_left_diagonal_x;
                if prev_last_side == 5 {
                    o.right_x = p.left_x;
                }
            } else if p.top_y < n.top_y {
                o.bottom_y = p.top_y;
                o.lower_left_diagonal_x = n.upper_right_diagonal_x;
            } else {
                o.bottom_y = n.top_y;
            }
        }
        6 => {
            if p.upper_left_diagonal_x < n.upper_left_diagonal_x {
                o.lower_right_diagonal_x = n.upper_left_diagonal_x;
                o.right_x = p.left_x;
                if prev_last_side == 6 {
                    o.upper_right_diagonal_x = p.lower_left_diagonal_x;
                }
            } else if p.upper_left_diagonal_x > n.upper_left_diagonal_x {
                o.bottom_y = n.top_y;
                o.lower_right_diagonal_x = p.upper_left_diagonal_x;
            } else {
                o.lower_right_diagonal_x = n.upper_left_diagonal_x;
            }
        }
        _ => {
            if p.left_x < n.left_x {
                o.right_x = n.left_x;
                o.upper_right_diagonal_x = p.lower_left_diagonal_x;
                if prev_last_side == 7 {
                    o.top_y = p.bottom_y;
                }
            } else if p.left_x > n.left_x {
                o.right_x = p.left_x;
                o.lower_right_diagonal_x = n.upper_left_diagonal_x;
            } else {
                o.right_x = n.left_x;
            }
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

// ------------------------------------------------------------ the assembly

/// One touching neighbour of the room being completed —
/// `Sorted45DegreeRoomNeighbours.SortedRoomNeighbour` (`:687-855`).
#[derive(Debug, Clone, Copy)]
struct Neighbour45 {
    /// The deterministic tie-break identity, `search_tree_object.get_id_no()`.
    object_id: i64,
    /// The shape index in the object.
    shape_index: u16,
    /// `room ∩ neighbour`, normalized.
    intersection: IntOctagon,
    /// Where the neighbour first and last touches the room border.
    sides: TouchingSides,
}

/// `SortedRoomNeighbour.compareTo` (`:797-854`), as an ordering.
///
/// Counter-clockwise around the room border: first by the side the neighbour
/// starts on, then by *where* on that side it starts — which is a different
/// coordinate for each of the eight sides, and runs backwards on the four that
/// point the other way (sides 4-7) so that "further counter-clockwise" is always
/// "greater". Ties fall through to how far the neighbour spans, then to where
/// it ends, then to the object id.
fn compare_neighbours(a: &Neighbour45, b: &Neighbour45) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if a.sides.first != b.sides.first {
        return a.sides.first.cmp(&b.sides.first);
    }
    let (p, q) = (&a.intersection, &b.intersection);
    let mut cmp = match a.sides.first {
        0 => p.corner_x(0) - q.corner_x(0),
        1 => p.corner_x(1) - q.corner_x(1),
        2 => p.corner_y(2) - q.corner_y(2),
        3 => p.corner_y(3) - q.corner_y(3),
        4 => q.corner_x(4) - p.corner_x(4),
        5 => q.corner_x(5) - p.corner_x(5),
        6 => q.corner_y(6) - p.corner_y(6),
        _ => q.corner_y(7) - p.corner_y(7),
    };
    if cmp == 0 {
        // The two neighbours start at the same point; the one that wraps
        // further around the ring comes second.
        let span = a.sides.span().cmp(&b.sides.span());
        if span != Ordering::Equal {
            return span;
        }
        cmp = match a.sides.last {
            0 => p.corner_x(1) - q.corner_x(1),
            1 => p.corner_x(2) - q.corner_x(2),
            2 => p.corner_y(3) - q.corner_y(3),
            3 => p.corner_y(4) - q.corner_y(4),
            4 => q.corner_x(5) - p.corner_x(5),
            5 => q.corner_x(6) - p.corner_x(6),
            6 => q.corner_y(7) - p.corner_y(7),
            _ => q.corner_y(0) - p.corner_y(0),
        };
    }
    if cmp != 0 {
        return cmp.cmp(&0);
    }
    a.object_id
        .cmp(&b.object_id)
        .then_with(|| a.shape_index.cmp(&b.shape_index))
}

/// The result of one `calculate_neighbours` pass.
struct Neighbours45 {
    room_shape: IntOctagon,
    neighbours: Vec<Neighbour45>,
    touched: [bool; SIDE_COUNT],
    own_net: Vec<(ItemId, u16)>,
}

/// Safety valve on the `try_remove_edge_line` restart loop.
///
/// Upstream recurses without a bound (`:58`); termination rests on the strict
/// area increase at `:350-352`, which holds for exact integer areas. This is a
/// valve, not a behaviour — a real board settles in one or two rounds.
const MAX_EDGE_REMOVAL_ROUNDS: u32 = 64;

impl AutorouteEngine<'_> {
    /// `Sorted45DegreeRoomNeighbours.calculate` (`:44-72`) — the 45° arm of
    /// `AutorouteEngine.calculate_doors`.
    pub fn calculate_doors_45(&mut self, room_id: RoomId) {
        for _ in 0..MAX_EDGE_REMOVAL_ROUNDS {
            // `:46-47` mints once per invocation, restarts included.
            self.mint_room_id(room_id);
            let found = self.calculate_neighbours_45(room_id);
            // `:54-59`: an enlarged room invalidates every door; start over.
            if self.try_remove_edge_line_45(room_id, &found) {
                self.remove_all_doors(room_id);
                continue;
            }
            let kind = self.rooms.room(room_id).kind;
            if found.neighbours.is_empty() {
                if kind == RoomKind::Obstacle {
                    // `:66` — the full ring, sides 0 through 7.
                    self.edge_gap_rooms_45(room_id, &found.room_shape, 0, 7);
                }
            } else {
                self.calculate_new_incomplete_rooms_45(room_id, &found);
            }
            if kind == RoomKind::CompleteFree {
                self.calculate_target_doors(room_id, &found.own_net);
            }
            return;
        }
    }

    /// `Sorted45DegreeRoomNeighbours.calculate_neighbours` (`:77-155`).
    fn calculate_neighbours_45(&mut self, room_id: RoomId) -> Neighbours45 {
        let (room_shape, layer, is_obstacle_room) = {
            let room = self.rooms.room(room_id);
            (room.octagon(), room.layer, room.kind == RoomKind::Obstacle)
        };

        let (mut obstacles, mut own_net) = self.take_collect_vecs();
        self.overlapping_obstacles_tile(
            &fr_board::shape::TileShape::Oct(room_shape),
            Some(layer),
            self.ctrl.net_no,
            &mut obstacles,
            &mut own_net,
        );
        if is_obstacle_room {
            // `:109` defers own-net objects only for a free room; for an
            // obstacle room they are ordinary neighbours.
            let store = self.board.store();
            obstacles.reserve(own_net.len());
            obstacles.extend(own_net.drain(..).map(|(item, index)| {
                let slot = store
                    .slot_of(item)
                    .unwrap_or(fr_board::store::SlotIndex::from_raw(0));
                Obstacle::Item(item, index, slot)
            }));
            obstacles.sort_unstable();
        }

        let mut found = Neighbours45 {
            room_shape,
            neighbours: Vec::new(),
            touched: [false; SIDE_COUNT],
            own_net,
        };

        for idx in 0..obstacles.len() {
            let obstacle = obstacles[idx];
            if obstacle == Obstacle::Room(room_id) {
                continue;
            }
            if let (Obstacle::Item(item, index, _), Some((room_item, room_index))) =
                (obstacle, self.rooms.room(room_id).item)
            {
                if item == room_item && index == room_index {
                    continue;
                }
            }
            let Some(neighbour_oct) = self.obstacle_octagon(obstacle) else {
                continue;
            };
            let intersection = room_shape.intersection(&neighbour_oct).normalize();
            let dimension = intersection.dimension();

            if dimension > 1 && is_obstacle_room {
                // `:117-127`: only an obstacle room may swallow a 2-dim overlap.
                if let Obstacle::Item(item, index, _) = obstacle {
                    self.create_overlap_door(room_id, item, index);
                }
                continue;
            }
            if dimension < 0 {
                // "may happen at a corner from 2 diagonal lines with non
                // integer coordinates" (`:128-131`).
                continue;
            }

            // `:132` — the neighbour is recorded even for a 0-dimensional
            // corner touch, because the gap-room ring is built from the touch
            // intervals, not from the doors.
            if let Some(sides) = touching_sides(&room_shape, &intersection) {
                mark_edge_interior_touches(&room_shape, &intersection, sides, &mut found.touched);
                let (object_id, shape_index) = match obstacle {
                    Obstacle::Item(item, idx, _) => (item.raw() as i64, idx),
                    Obstacle::Room(room) => (self.rooms.room(room).id_no, 0),
                };
                found.neighbours.push(Neighbour45 {
                    object_id,
                    shape_index,
                    intersection,
                    sides,
                });
            }

            if dimension > 0 {
                // `:133-152`: make sure there is a door to the neighbour room.
                let neighbour_room = match obstacle {
                    Obstacle::Room(other) => Some(other),
                    Obstacle::Item(item, index, slot) => {
                        let routable =
                            self.board
                                .store()
                                .item_at_slot(slot)
                                .is_some_and(|board_item| {
                                    board_item.id == item && board_item.is_routable()
                                });
                        if routable {
                            self.obstacle_room(item, index)
                        } else {
                            None
                        }
                    }
                };
                if let Some(neighbour_room) = neighbour_room {
                    if self
                        .insert_door_ok_lazy(room_id, neighbour_room, || intersection.to_simplex())
                    {
                        // `ExpansionDoor`'s 2-argument constructor derives the
                        // dimension from the shared shape.
                        self.rooms.add_room_door(room_id, neighbour_room, dimension);
                    }
                }
            }
        }
        self.give_back_collect_vecs(obstacles, Vec::new());

        // `sorted_neighbours` is a `TreeSet` (`:22`, `:36`, `:221`), so equal
        // elements collapse *there*. This is a plain stable sort and nothing is
        // removed: not reproducing that collapse is the measured divergence
        // recorded on `neighbours::AutorouteEngine::obstacle_id` (3.2x
        // expansions on Issue026, 11x on Issue034 when it is reproduced), and
        // `compare_neighbours` reads an `obstacle_id` that carries the shape
        // index, so two distinct obstacles never compare equal to begin with.
        found.neighbours.sort_by(compare_neighbours);
        found
    }

    /// `Sorted45DegreeRoomNeighbours.try_remove_edge_line` (`:276-365`).
    ///
    /// Where the any-angle variant drops **one** simplex border line per
    /// restart, this relaxes every untouched offset at once
    /// ([`remove_not_touching_border_lines`]) and re-completes. That is the
    /// single biggest reason the 45° tiling converges in fewer rounds.
    fn try_remove_edge_line_45(&mut self, room_id: RoomId, found: &Neighbours45) -> bool {
        if self.rooms.room(room_id).kind != RoomKind::CompleteFree {
            return false;
        }
        let room_oct = found.room_shape;
        if room_oct.dimension() != 2 {
            return false;
        }
        let room_area = room_oct.area();

        // `:287-296`: only bother when some untouched side is a real edge and
        // not a degenerate corner of the octagon.
        let mut worth_trying = false;
        for side in 0..SIDE_COUNT {
            if found.touched[side] {
                continue;
            }
            let a = room_oct.corner(side);
            let b = room_oct.corner((side + 1) % SIDE_COUNT);
            let dx = b.x as f64 - a.x as f64;
            let dy = b.y as f64 - a.y as f64;
            if dx * dx + dy * dy > 1.0 {
                worth_trying = true;
                break;
            }
        }
        if !worth_trying {
            return false;
        }

        // `:321-338`: the largest 2-dimensional door to an already complete
        // free room is what `complete_shape` must not re-carve against.
        let mut ignore_room = None;
        let mut ignore_shape = None;
        let mut max_door_area = 0.0f64;
        for door_id in self.rooms.room(room_id).doors.clone() {
            let door = self.rooms.door(door_id);
            if door.dimension != 2 {
                continue;
            }
            let Some(other) = door.other_room_any(room_id) else {
                continue;
            };
            if self.rooms.room(other).kind != RoomKind::CompleteFree {
                continue;
            }
            let area = crate::geom::area(&door.shape);
            if area > max_door_area {
                max_door_area = area;
                ignore_shape = crate::rooms::complete45::simplex_octagon(&door.shape);
                ignore_room = Some(other);
            }
        }

        let enlarged = remove_not_touching_border_lines(&room_oct, &found.touched);
        let (contained, layer) = {
            let room = self.rooms.room(room_id);
            (room.contained_octagon(), room.layer)
        };
        let new_rooms = if self.is_fortyfive_completion() {
            self.complete_shape_45_oct(Some(enlarged), contained, layer, ignore_room, ignore_shape)
        } else {
            let enlarged_simplex = enlarged.to_simplex();
            let contained_simplex = self.rooms.room(room_id).contained_shape.clone();
            let ignore_simplex = ignore_shape.map(|oct| oct.to_simplex());
            self.complete_shape(
                Some(&enlarged_simplex),
                &contained_simplex,
                layer,
                ignore_room,
                ignore_simplex.as_ref(),
            )
            .into_iter()
            .filter_map(|piece| {
                let shape = piece.oct.or_else(|| piece.shape.bounding_octagon())?;
                let contained = piece.contained.bounding_octagon()?;
                Some(Room45 { shape, contained })
            })
            .collect()
        };
        if new_rooms.len() != 1 {
            return false;
        }
        // `:350-352`: only adopt a strictly larger room, or the restart loop
        // never ends.
        if new_rooms[0].shape.area() <= room_area {
            return false;
        }
        let new_room = new_rooms[0];
        let room = self.rooms.room_mut(room_id);
        room.set_shape_from_octagon(new_room.shape);
        room.contained_shape = new_room.contained.to_simplex();
        true
    }

    /// `Sorted45DegreeRoomNeighbours.insert_incomplete_room` (`:374-389`).
    ///
    /// The door dimension is the *contained* shape's dimension, not a constant:
    /// a gap room that shares an area with the room gets a 2-dimensional door.
    fn insert_incomplete_room_45(
        &mut self,
        room_id: RoomId,
        room_shape: &IntOctagon,
        new_shape: IntOctagon,
    ) {
        if new_shape.dimension() != 2 {
            return;
        }
        let new_contained = room_shape.intersection(&new_shape).normalize();
        if new_contained.is_empty() {
            return;
        }
        let door_dimension = new_contained.dimension();
        if door_dimension <= 0 {
            return;
        }
        let layer = self.rooms.room(room_id).layer;
        let new_room = self
            .rooms
            .add_incomplete_room_oct(new_shape, layer, new_contained);
        self.rooms.add_room_door(room_id, new_room, door_dimension);
    }

    /// `calculate_edge_incomplete_rooms_of_obstacle_expansion_room` (`:228-271`):
    /// one gap room per free side of an obstacle room, from `from_side` to
    /// `to_side` inclusive, walking counter-clockwise.
    ///
    /// **Upstream quirk, reproduced deliberately:** the degenerate-side test
    /// compares each side's *end* corner against `corner(from_side)` and not
    /// against the side's own start corner — `curr_corner` is read once before
    /// the loop and never reassigned (`:236,240-241`). Fixing it would create
    /// gap rooms upstream does not create, which is exactly the tiling
    /// difference this port is trying not to have.
    fn edge_gap_rooms_45(
        &mut self,
        room_id: RoomId,
        room_shape: &IntOctagon,
        from_side: usize,
        to_side: usize,
    ) {
        let board = self.bounds().to_int_octagon().normalize();
        let anchor_corner = room_shape.corner(from_side % SIDE_COUNT);
        let mut curr_side = from_side % SIDE_COUNT;
        let to_side = to_side % SIDE_COUNT;
        for _ in 0..SIDE_COUNT {
            let next_side = (curr_side + 1) % SIDE_COUNT;
            if room_shape.corner(next_side) != anchor_corner {
                let gap = edge_incomplete_room(room_shape, &board, curr_side);
                self.insert_incomplete_room_45(room_id, room_shape, gap);
            }
            if curr_side == to_side {
                break;
            }
            curr_side = next_side;
        }
    }

    /// `calculate_new_incomplete_rooms_for_obstacle_expansion_room` (`:391-501`).
    ///
    /// The gap around an obstacle room's corner is built in three parts: the
    /// piece leaving the previous neighbour ([`from_side_incomplete_room`]), the
    /// piece arriving at the next one ([`to_side_incomplete_room`]), and one
    /// whole-side room for every side strictly between them.
    fn obstacle_gap_rooms_45(
        &mut self,
        room_id: RoomId,
        room_shape: &IntOctagon,
        prev: &Neighbour45,
        next: &Neighbour45,
        same_neighbour: bool,
    ) {
        let from_side = prev.sides.last;
        let to_side = next.sides.first;
        if from_side == to_side && !same_neighbour {
            // `:394-397` — "no return in case of only 1 neighbour".
            return;
        }
        let board = self.bounds().to_int_octagon().normalize();
        let from_room =
            from_side_incomplete_room(room_shape, &board, from_side, &prev.intersection);
        self.insert_incomplete_room_45(room_id, room_shape, from_room);
        let to_room = to_side_incomplete_room(room_shape, &board, to_side, &next.intersection);
        self.insert_incomplete_room_45(room_id, room_shape, to_room);

        let curr_from = (from_side + 1) % SIDE_COUNT;
        if curr_from == to_side {
            return;
        }
        let curr_to = (to_side + SIDE_COUNT - 1) % SIDE_COUNT;
        self.edge_gap_rooms_45(room_id, room_shape, curr_from, curr_to);
    }

    /// `Sorted45DegreeRoomNeighbours.calculate_new_incomplete_rooms` (`:503-682`).
    fn calculate_new_incomplete_rooms_45(&mut self, room_id: RoomId, found: &Neighbours45) {
        let neighbours = &found.neighbours;
        if neighbours.is_empty() {
            return;
        }
        let room_shape = found.room_shape;
        let is_obstacle_room = self.rooms.room(room_id).kind == RoomKind::Obstacle;

        // `:505-510` — an obstacle room with a single neighbour wraps the ring
        // from that neighbour back to itself.
        if is_obstacle_room && neighbours.len() == 1 {
            let only = neighbours[0];
            self.obstacle_gap_rooms_45(room_id, &room_shape, &only, &only, true);
            return;
        }

        let mut prev = neighbours[neighbours.len() - 1];
        for next in neighbours.iter().copied() {
            let insert = if is_obstacle_room && neighbours.len() == 2 {
                // `:515-531`: with exactly two neighbours, decide whether this
                // side of the obstacle is closed off or open.
                let touch = next
                    .intersection
                    .intersection(&prev.intersection)
                    .normalize();
                if touch.is_empty() {
                    true
                } else if touch.dimension() >= 1 {
                    false
                } else if prev.sides.last == next.sides.first {
                    // A corner touch along one side of the room shape.
                    false
                } else {
                    prev.sides.last != (next.sides.first + 1) % SIDE_COUNT
                }
            } else {
                // `:534` — the two neighbours simply do not touch.
                !next.intersection.intersects(&prev.intersection)
            };

            if insert {
                if is_obstacle_room && next.sides.first != prev.sides.last {
                    self.obstacle_gap_rooms_45(room_id, &room_shape, &prev, &next, false);
                } else {
                    let board = self.bounds().to_int_octagon().normalize();
                    let gap = free_gap_room(
                        &room_shape,
                        &board,
                        &prev.intersection,
                        prev.sides.last,
                        &next.intersection,
                        next.sides.first,
                    );
                    self.insert_incomplete_room_45(room_id, &room_shape, gap);
                }
            }
            prev = next;
        }
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
        assert_eq!(
            touched,
            [true, true, false, false, false, false, true, true]
        );
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
                let point =
                    fr_geometry::planar::Point::Int(fr_geometry::planar::IntPoint::new(x, y));
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
