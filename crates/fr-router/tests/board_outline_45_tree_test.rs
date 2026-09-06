use fr_board::item::ItemId;
use fr_io::specctra;
use std::path::PathBuf;

#[test]
fn test_outline_shapes_45_family_retains_simplex() {
    let bytes = std::fs::read(PathBuf::from(
        "/tmp/freerouting-upstream/fixtures/Issue054-tairakb.dsn",
    ))
    .expect("fixture");
    let mut design = specctra::import(&bytes).expect("imports");
    design.board.ensure_compensated_tree(1);

    let tree_45 = design
        .board
        .trees()
        .compensated_tree_with_family(1, fr_board::search_tree::TreeAngleFamily::FortyFive)
        .expect("45 tree must exist");

    let slot1 = design.board.store().slot_of(ItemId::from_raw(1)).unwrap();
    let shapes = tree_45.item_shapes(slot1, ItemId::from_raw(1));

    // Shape 72 on layer 1 must remain a Simplex, matching upstream Java ShapeSearchTree.calculate_tree_shapes(BoardOutline)
    let shape_72 = &shapes[72];
    assert_eq!(shape_72.layer, 1);
    match &shape_72.shape {
        Some(fr_board::shape::TileShape::Simplex(s)) => {
            // Check that it is not inflated into an octagon that swallows pin 6
            let p6 = fr_geometry::planar::IntPoint {
                x: 2158930,
                y: -1539207,
            };
            assert!(
                !s.contains(&fr_geometry::Point::Int(p6)),
                "Board outline strip must not contain pin 6!"
            );
        }
        other => panic!("Expected Simplex, got {:?}", other),
    }
}
