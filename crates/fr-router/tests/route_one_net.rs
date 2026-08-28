//! End-to-end: one connection of one net on a tiny synthetic board.
//!
//! Two pads of the same net, 4000 units apart on layer 0, on an otherwise empty
//! two-layer board. The maze must find the straight connection, and the
//! backtrack chain must have the shape `LocateFoundConnectionAlgo` requires
//! (`pull-tight-insert.md` §2.1): destination first, start target door last,
//! every hop carrying a live room.

use fr_router::maze::search::MazeSearch;
use fr_router::result::BacktrackKind;
use fr_router::test_support::TestFixture;

#[test]
fn routes_a_straight_connection_between_two_pads() {
    let fixture = TestFixture::two_layer_with_pads();
    let mut engine = fixture.engine();
    let pads = fixture.pad_ids();
    let start = &pads[..1];
    let destination = &pads[1..2];

    let mut search =
        MazeSearch::get_instance(&mut engine, start, destination).expect("start and destination");
    let connection = search
        .find_connection()
        .expect("an empty board between two pads is always routable");

    // ---- the locator's invariants ------------------------------------
    assert!(
        connection.is_well_formed(),
        "the last chain element must be a target door"
    );
    assert!(
        connection.backtrack.len() >= 2,
        "a connection is at least destination door → start door, got {}",
        connection.backtrack.len()
    );

    // Destination first.
    let first = &connection.backtrack[0];
    match first.kind {
        BacktrackKind::TargetDoor { item, .. } => {
            assert_eq!(item, destination[0], "the chain starts at the destination")
        }
        other => panic!("expected the destination target door, got {other:?}"),
    }
    assert_eq!(connection.destination_door, first.door);
    assert_eq!(
        connection.section_no_of_destination_door,
        first.section_no_of_door
    );

    // Start last.
    let last = connection.backtrack.last().expect("non-empty chain");
    match last.kind {
        BacktrackKind::TargetDoor { item, .. } => {
            assert_eq!(item, start[0], "the chain ends at the start item")
        }
        other => panic!("expected the start target door, got {other:?}"),
    }

    // Both ends are on the pads' layer, and nothing in between changed layer:
    // an empty board offers no reason to pay a via.
    assert_eq!(connection.start_layer(), Some(0));
    assert_eq!(connection.target_layer(), Some(0));
    assert!(
        !connection
            .backtrack
            .iter()
            .any(|element| matches!(element.kind, BacktrackKind::Drill { .. })),
        "a straight connection on an empty board must not use a via"
    );

    // Every hop carries the room it shares with the next hop, and that room is
    // live and 2-dimensional — the locator shrinks it by the compensated half
    // width to place corners (`pull-tight-insert.md` §3).
    for (index, element) in connection.backtrack.iter().enumerate() {
        let shape = element
            .next_room_shape
            .as_ref()
            .unwrap_or_else(|| panic!("chain element {index} has no room"));
        assert_eq!(
            shape.dimension(),
            2,
            "chain element {index} room is degenerate"
        );
        assert_eq!(element.next_room_layer, Some(0));
    }

    // Consecutive elements must actually share their room, or the locator
    // cannot walk from one door to the next.
    for pair in connection.backtrack.windows(2) {
        assert!(
            pair[0].next_room.is_some() && pair[1].next_room.is_some(),
            "both ends of a hop need a room"
        );
    }

    // Nothing was ripped: the board holds nothing to rip.
    assert!(connection.ripped_items.is_empty());
}

#[test]
fn a_found_connection_reports_the_expansions_it_took() {
    let fixture = TestFixture::two_layer_with_pads();
    let mut engine = fixture.engine();
    let pads = fixture.pad_ids();
    let mut search =
        MazeSearch::get_instance(&mut engine, &pads[..1], &pads[1..2]).expect("instance");
    search.find_connection().expect("routable");
    assert!(search.expansions() > 0);
    let stats = search.frontier_stats();
    assert!(stats.pushes >= stats.pops);
}

#[test]
fn an_empty_destination_set_is_refused_at_init() {
    // `MazeSearchAlgo.init` fails outright when nothing was joined into the
    // heuristic (`:1240-1244`); the caller reports NOT_ROUTED.
    let fixture = TestFixture::two_layer_with_pads();
    let mut engine = fixture.engine();
    let pads = fixture.pad_ids();
    let failure = MazeSearch::get_instance(&mut engine, &pads[..1], &[]).err();
    assert_eq!(failure, Some(fr_router::SearchFailure::NoDestination));
}

#[test]
fn an_expansion_limit_stops_the_search_instead_of_running_away() {
    let fixture = TestFixture::two_layer_with_pads();
    let mut engine = fixture.engine();
    let pads = fixture.pad_ids();
    let mut search =
        MazeSearch::get_instance(&mut engine, &pads[..1], &pads[1..2]).expect("instance");
    search.set_expansion_limit(Some(0));
    assert_eq!(
        search.find_connection().err(),
        Some(fr_router::SearchFailure::Stopped)
    );
}
