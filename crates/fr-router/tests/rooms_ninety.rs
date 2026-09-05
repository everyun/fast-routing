//! Tests for 90° room completion and door calculation (`rooms/complete90.rs`, `rooms/neighbours90.rs`).

use fr_geometry::planar::IntBox;
use fr_router::ctrl::AutorouteControl;
use fr_router::engine::AutorouteEngine;
use fr_router::rooms::RoomKind;
use fr_router::test_support::TestFixture;

#[test]
fn ninety_degree_room_completion_splits_boxes_cleanly() {
    let mut fixture = TestFixture::empty_two_layer();
    fixture
        .board
        .set_angle_restriction(fr_board::AngleRestriction::Ninety);
    fixture
        .rules
        .set_trace_angle_restriction(fr_rules::AngleRestriction::NinetyDegree);

    let ctrl = AutorouteControl::for_net(&fixture.board, &fixture.rules, &fixture.settings, 1);
    let mut engine = AutorouteEngine::new(&fixture.board, &fixture.rules, ctrl);

    let contained = IntBox::new(400, 400, 600, 600).to_simplex();
    let completed = engine.complete_shape(None, &contained, 0, None, None);

    assert_eq!(completed.len(), 1);
    assert!(!completed[0].shape.is_empty());
}

#[test]
fn ninety_degree_calculate_doors_builds_orthogonal_doors() {
    let mut fixture = TestFixture::two_layer_with_pads();
    fixture
        .board
        .set_angle_restriction(fr_board::AngleRestriction::Ninety);
    fixture
        .rules
        .set_trace_angle_restriction(fr_rules::AngleRestriction::NinetyDegree);

    let ctrl = AutorouteControl::for_net(&fixture.board, &fixture.rules, &fixture.settings, 1);
    let mut engine = AutorouteEngine::new(&fixture.board, &fixture.rules, ctrl);

    let shape = IntBox::new(-2500, -500, -1500, 500).to_simplex();
    let contained = IntBox::new(-2100, -100, -1900, 100).to_simplex();
    let room_id = engine.rooms.add_incomplete_room(Some(shape), 0, contained);

    engine.complete_expansion_room(room_id);

    assert_eq!(engine.rooms.complete.len(), 1);
    let complete_room_id = engine.rooms.complete[0];
    assert_eq!(
        engine.rooms.room(complete_room_id).kind,
        RoomKind::CompleteFree
    );
    assert!(!engine.rooms.room(complete_room_id).target_doors.is_empty());
}
