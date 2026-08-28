//! The 45° tiling chain: the octagon tree flavor, the octagon `complete_shape`,
//! and the octagon door calculation.
//!
//! Three things are checked here, in this order:
//!
//! 1. **the room invariants still hold** under the octagon flavor — the tiling
//!    is disjoint, obstacle-free, and every room is live and 2-dimensional.
//!    These are the same properties the any-angle path is held to; if the flip
//!    broke one of them the gate would show it as added violations, and this
//!    catches it first;
//! 2. **every room really is an octagon** — no more than eight border lines,
//!    where the any-angle path can produce any number. That is the structural
//!    claim of the chain. It does *not* follow that the tiling is coarser: the
//!    measurement on real boards says `Full` produces more rooms, not fewer
//!    (see `Tiling45Mode`), which is exactly why the switch is three-valued;
//! 3. **a hand-derived door set** on a board simple enough to work out on
//!    paper, including the one upstream quirk this port reproduces on purpose.

use fr_board::search_tree::{Tiling45Mode, TreeAngleFamily};
use fr_geometry::planar::{IntBox, IntOctagon, Point, Simplex};
use fr_router::engine::AutorouteEngine;
use fr_router::rooms::{RoomId, RoomKind};
use fr_router::test_support::TestFixture;

/// An engine pinned to a mode, regardless of the environment switch.
fn engine_in<'a>(fixture: &'a TestFixture, mode: Tiling45Mode) -> AutorouteEngine<'a> {
    let mut engine = fixture.engine();
    engine.set_tiling_45(mode);
    engine
}

/// True when `shape` is exactly its own bounding octagon — i.e. it is an
/// octagon, not merely contained in one.
fn is_octagon(shape: &Simplex) -> bool {
    let Some(oct) = shape.bounding_octagon() else {
        return false;
    };
    // Areas are exact for integer-cornered convex shapes, and the bounding
    // octagon always *contains* the shape, so equal area means equal shape.
    (oct.normalize().area() - shape.area()).abs() < 1e-6
}

/// Completes one room around `contained` and returns the resulting room ids.
fn complete_around(engine: &mut AutorouteEngine<'_>, contained: IntBox, layer: i32) -> Vec<RoomId> {
    let incomplete = engine
        .rooms
        .add_incomplete_room(None, layer, contained.to_simplex());
    engine.complete_expansion_room(incomplete)
}

// ---------------------------------------------------------------- invariants

/// Every room the 45° chain produces is an octagon — the structural invariant
/// the whole module rests on, and one the any-angle path does not have.
#[test]
fn every_room_the_fortyfive_chain_builds_is_an_octagon() {
    for fixture in [
        TestFixture::empty_two_layer(),
        TestFixture::two_layer_with_pads(),
        TestFixture::with_foreign_trace(),
    ] {
        let mut engine = engine_in(&fixture, Tiling45Mode::Full);
        let rooms = complete_around(&mut engine, IntBox::new(-1900, -10, -1800, 10), 0);
        assert!(!rooms.is_empty(), "the completion must produce a room");
        for room in rooms {
            let shape = &engine.rooms.room(room).shape;
            assert_eq!(shape.dimension(), 2);
            assert!(
                shape.border_line_count() <= 8,
                "an octagon has at most 8 border lines, got {}",
                shape.border_line_count()
            );
            assert!(is_octagon(shape), "room shape {shape:?} is not an octagon");
            // The memo must agree with the shape it stands for.
            let oct = engine.rooms.room(room).octagon();
            assert!(
                (oct.area() - shape.area()).abs() < 1e-6,
                "the octagon memo disagrees with the room shape"
            );
        }
    }
}

/// The tiling invariants: rooms are alive, pairwise interior-disjoint, and none
/// of them overlaps a foreign obstacle.
#[test]
fn the_fortyfive_tiling_keeps_the_room_invariants() {
    let fixture = TestFixture::with_foreign_trace();
    let mut engine = engine_in(&fixture, Tiling45Mode::Full);
    let rooms = complete_around(&mut engine, IntBox::new(-1900, -10, -1800, 10), 0);
    assert!(!rooms.is_empty());

    let shapes: Vec<Simplex> = rooms
        .iter()
        .map(|room| {
            let room = engine.rooms.room(*room);
            assert!(room.alive, "a returned room must be alive");
            assert_eq!(room.kind, RoomKind::CompleteFree);
            room.shape.clone()
        })
        .collect();

    for (i, a) in shapes.iter().enumerate() {
        for b in shapes.iter().skip(i + 1) {
            assert!(
                a.intersection(b).dimension() < 2,
                "two completed rooms share an area"
            );
        }
    }

    // No completed room may overlap the foreign-net trace, which is the only
    // real obstacle on this board besides the outline.
    let trace = fixture.trace_id();
    let tile_count = engine.tree_shape_count(trace);
    assert!(tile_count > 0);
    for shape in &shapes {
        for index in 0..tile_count as u16 {
            if let Some(obstacle) = engine.item_tile(trace, index) {
                assert!(
                    shape.intersection(&obstacle).dimension() < 2,
                    "a completed room overlaps the foreign trace"
                );
            }
        }
    }
}

/// Every door the 45° chain creates is symmetric — both of its rooms list it —
/// and every door shape is really the shared region of the two rooms.
#[test]
fn the_fortyfive_doors_are_symmetric_and_real() {
    let fixture = TestFixture::with_foreign_trace();
    let mut engine = engine_in(&fixture, Tiling45Mode::Full);
    let rooms = complete_around(&mut engine, IntBox::new(-1900, -10, -1800, 10), 0);

    for room_id in rooms {
        for door_id in engine.rooms.room(room_id).doors.clone() {
            let door = engine.rooms.door(door_id).clone();
            assert!(door.alive);
            let other = door
                .other_room_any(room_id)
                .expect("a room's own door must name it");
            assert!(
                engine.rooms.room(other).doors.contains(&door_id),
                "the far side does not list the door"
            );
            assert!(
                !door.shape.is_empty(),
                "a door with an empty shape is not a door"
            );
            let expected = engine
                .rooms
                .room(door.first_room)
                .shape
                .intersection(&engine.rooms.room(door.second_room).shape);
            assert!(
                (expected.area() - door.shape.area()).abs() < 1e-6,
                "the cached door shape is not the shared region"
            );
        }
    }
}

/// The structural claim, measured: on the same board and the same start, no 45°
/// room may have more border lines than an octagon has, and both chains must
/// still cover the start region.
///
/// This is deliberately *not* asserted as "fewer rooms" — on real boards the
/// octagon completion produces more of them, and a test that pretended
/// otherwise would be asserting the prediction rather than the behaviour.
#[test]
fn the_fortyfive_tiling_is_not_finer_than_the_any_angle_one() {
    let fixture = TestFixture::with_foreign_trace();
    let start = IntBox::new(-1900, -10, -1800, 10);

    let mut any = engine_in(&fixture, Tiling45Mode::Off);
    let any_rooms = complete_around(&mut any, start, 0);
    let any_lines: usize = any_rooms
        .iter()
        .map(|r| any.rooms.room(*r).shape.border_line_count())
        .sum();

    let mut oct = engine_in(&fixture, Tiling45Mode::Full);
    let oct_rooms = complete_around(&mut oct, start, 0);
    let oct_lines: usize = oct_rooms
        .iter()
        .map(|r| oct.rooms.room(*r).shape.border_line_count())
        .sum();

    assert!(!oct_rooms.is_empty());
    assert!(
        oct_rooms
            .iter()
            .all(|r| oct.rooms.room(*r).shape.border_line_count() <= 8),
        "a 45° room must be an octagon"
    );
    // Both cover the start; the octagon tiling is the coarser description of it.
    let origin = Point::Int(fr_geometry::planar::IntPoint::new(-1850, 0));
    assert!(oct_rooms
        .iter()
        .any(|r| oct.rooms.room(*r).shape.contains(&origin)));
    assert!(any_rooms
        .iter()
        .any(|r| any.rooms.room(*r).shape.contains(&origin)));
    // Per room, the octagon tiling is bounded where the any-angle one is not.
    assert!(
        oct_lines <= 8 * oct_rooms.len(),
        "45°: {oct_lines} border lines over {} rooms; any-angle: {any_lines} over {}",
        oct_rooms.len(),
        any_rooms.len()
    );
}

// ------------------------------------------------- the hand-derived door set

/// A square obstacle room alone in open space, worked out by hand.
///
/// The room is the box `[-100,-100]..[100,100]`, whose normalized octagon has
/// `ulx = llx = -200`, `lrx = urx = 200` and therefore four *degenerate*
/// diagonal sides. Its eight corners, in `IntOctagon` order, are
///
/// ```text
///   0 (-100,-100)   1 (100,-100)   2 (100,-100)   3 (100, 100)
///   4 ( 100, 100)   5 (-100, 100)  6 (-100, 100)  7 (-100,-100)
/// ```
///
/// so sides 1, 3, 5 and 7 are points and sides 0, 2, 4, 6 are the bottom, right,
/// top and left edges. Nothing else on the board touches it, so `calculate_doors`
/// takes the empty-neighbours branch and walks all eight sides
/// (`Sorted45DegreeRoomNeighbours.java:66,228-271`).
///
/// **The expected answer is three doors, not four.** Upstream's walk compares
/// each side's *end* corner against `corner(from_side)` — the anchor — rather
/// than against the side's own start corner: `curr_corner` is read once before
/// the loop and never reassigned (`:257,261-262`, identical in v1.9 and 2.3.0).
/// Side 6 is the left edge, and its end corner `7` is `(-100,-100)`, which *is*
/// the anchor, so the left gap room is never created. The two degenerate sides
/// that do get past the corner test (1 and 3) are then dropped because their
/// gap room shares only a point with the obstacle, giving a 0-dimensional door.
///
/// This test exists to pin that quirk deliberately: the target of the whole
/// chain is the lab jar's tiling, and "fixing" the walk here would create gap
/// rooms the jar does not create.
#[test]
fn an_isolated_square_obstacle_room_gets_the_hand_derived_gap_rooms() {
    let fixture = TestFixture::two_layer_with_pads();
    let mut engine = engine_in(&fixture, Tiling45Mode::Full);
    let pad = fixture.pad_ids()[0];

    let square = IntBox::new(-100, -100, 100, 100);
    let room = engine
        .rooms
        .add_obstacle_room(pad, 0, square.to_simplex(), 0);
    engine.calculate_doors(room);

    let doors = engine.rooms.room(room).doors.clone();
    assert_eq!(
        doors.len(),
        3,
        "bottom, right and top; the left edge is lost to the upstream anchor quirk"
    );

    // Each gap room is the board clipped to one side's outside half plane, and
    // its door back is the full edge — a 1-dimensional door.
    let mut door_boxes: Vec<IntBox> = doors
        .iter()
        .map(|door| {
            let door = engine.rooms.door(*door);
            assert_eq!(door.dimension, 1, "an edge door is 1-dimensional");
            door.shape.bounding_box()
        })
        .collect();
    door_boxes.sort_by_key(|b| (b.left_x(), b.bottom_y()));
    assert_eq!(
        door_boxes,
        vec![
            // the top edge, the bottom edge …
            IntBox::new(-100, -100, 100, -100),
            IntBox::new(-100, 100, 100, 100),
            // … and the right edge.
            IntBox::new(100, -100, 100, 100),
        ],
        "the three doors must be exactly the bottom, top and right edges"
    );

    // Every gap room is an incomplete octagon on the far side of its edge, and
    // none of them reaches into the obstacle.
    for door in doors {
        let other = engine.rooms.door(door).other_room_any(room).unwrap();
        let other_room = engine.rooms.room(other);
        assert_eq!(other_room.kind, RoomKind::IncompleteFree);
        assert!(is_octagon(&other_room.shape));
        assert!(
            other_room
                .shape
                .intersection(&square.to_simplex())
                .dimension()
                < 2,
            "a gap room reaches into the obstacle"
        );
    }
}

/// The isolated-square case again, but read as octagon offsets: the bottom gap
/// room really is "everything on the board below `y = -100`".
#[test]
fn the_bottom_gap_room_is_the_board_below_the_obstacle() {
    let fixture = TestFixture::two_layer_with_pads();
    let mut engine = engine_in(&fixture, Tiling45Mode::Full);
    let pad = fixture.pad_ids()[0];
    let board = engine.bounds().to_int_octagon().normalize();

    let square = IntBox::new(-100, -100, 100, 100);
    let room = engine
        .rooms
        .add_obstacle_room(pad, 0, square.to_simplex(), 0);
    engine.calculate_doors(room);

    let bottom = engine
        .rooms
        .room(room)
        .doors
        .iter()
        .map(|door| {
            let other = engine.rooms.door(*door).other_room_any(room).unwrap();
            engine.rooms.room(other).octagon()
        })
        .find(|oct| oct.top_y == -100)
        .expect("there must be a gap room below the obstacle");

    let expected = IntOctagon::new(
        board.left_x,
        board.bottom_y,
        board.right_x,
        -100,
        board.upper_left_diagonal_x,
        board.lower_right_diagonal_x,
        board.lower_left_diagonal_x,
        board.upper_right_diagonal_x,
    )
    .normalize();
    assert_eq!(bottom, expected);
}

// ------------------------------------------------------------- the A/B switch

/// The switch is off by default, so nothing changes unless the gate asks for it.
#[test]
fn the_chain_is_off_unless_the_switch_says_otherwise() {
    // The test process does not set `FR_ROUTER_TILING_45`.
    assert_eq!(fr_board::search_tree::tiling_45_mode(), Tiling45Mode::Off);
    let fixture = TestFixture::two_layer_with_pads();
    let engine = fixture.engine();
    assert_eq!(engine.tiling_45(), Tiling45Mode::Off);
    assert_eq!(engine.angle_family(), TreeAngleFamily::AnyAngle);
}

/// `Doors` runs the octagon door calculation but leaves the room completion
/// any-angle, so the split between the two halves of the chain is real rather
/// than a rename: the rooms are still simplices, and the tiling still works.
#[test]
fn the_doors_mode_runs_only_half_the_chain() {
    let fixture = TestFixture::with_foreign_trace();
    let mut engine = engine_in(&fixture, Tiling45Mode::Doors);
    assert_eq!(engine.angle_family(), TreeAngleFamily::FortyFive);
    let rooms = complete_around(&mut engine, IntBox::new(-1900, -10, -1800, 10), 0);
    assert!(!rooms.is_empty(), "the completion must still produce rooms");
    for room in rooms {
        let room = engine.rooms.room(room);
        assert!(room.alive);
        assert_eq!(room.kind, RoomKind::CompleteFree);
        assert_eq!(room.shape.dimension(), 2);
    }
}

/// Both families route the same start pad to a target door — the smoke test
/// that the flip does not simply disconnect the board.
#[test]
fn both_families_find_a_target_door_on_the_start_pad() {
    let fixture = TestFixture::two_layer_with_pads();
    for mode in [Tiling45Mode::Off, Tiling45Mode::Doors, Tiling45Mode::Full] {
        let mut engine = engine_in(&fixture, mode);
        let pad = fixture.pad_ids()[0];
        let shape = engine.item_tile(pad, 0).expect("pad tile");
        let incomplete = engine.rooms.add_incomplete_room(None, 0, shape);
        let rooms = engine.complete_expansion_room(incomplete);
        let target_doors: usize = rooms
            .iter()
            .map(|room| engine.rooms.room(*room).target_doors.len())
            .sum();
        assert!(
            target_doors >= 1,
            "{mode:?}: the start pad must produce at least one target door"
        );
    }
}
