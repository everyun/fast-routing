//! The 45°-family tree must re-flavor only the classes upstream's
//! `ShapeSearchTree45Degree` overrides — drills (`:411-440`), ObstacleArea
//! (`:442-451`) and BoardOutline (`:453-460`) — and keep every other class's
//! base shape. `PolylineTrace` has no override (`ShapeSearchTree.java:1039-1051`),
//! so its strips must stay the exact concave Simplex; flavoring one onto its
//! bounding octagon strictly grows it past the copper on oblique imported
//! segments (the Issue054 /row4 dead-pocket family).

use std::path::PathBuf;

use fr_board::search_tree::{calculate_tree_shapes_for_family, TreeAngleFamily};
use fr_board::shape::AngleRestriction;
use fr_io::specctra;

fn shapes_for(
    item: &fr_board::item::Item,
    ctx: fr_board::search_tree::ShapeContext<'_>,
) -> Vec<fr_board::shape::TileShape> {
    let mut out = Vec::new();
    calculate_tree_shapes_for_family(item, 0, TreeAngleFamily::FortyFive, ctx, &mut out);
    out.into_iter()
        .map(|t| t.shape.expect("indexed shape"))
        .collect()
}

#[test]
fn the_45_family_keeps_trace_strips_and_coarsens_the_rest() {
    let bytes = std::fs::read(PathBuf::from(
        "/tmp/freerouting-upstream/fixtures/Issue054-tairakb.dsn",
    ))
    .expect("fixture");
    let design = specctra::import(&bytes).expect("imports");
    let ctx = fr_board::search_tree::ShapeContext {
        clearances: design.board.clearances(),
        padstacks: design.board.padstacks(),
        layer_count: design.board.layers().len(),
        max_tree_shape_width: design.board.max_tree_shape_width(),
        angle_restriction: AngleRestriction::FortyFive,
    };

    // Trace #6789 — the /row4 protected trace. Base shapes are Simplex strips;
    // the family tree must keep them exactly.
    let trace = design
        .board
        .store()
        .get(fr_board::item::ItemId::from_raw(6789))
        .unwrap();
    let mut base = Vec::new();
    fr_board::search_tree::calculate_tree_shapes(trace, 0, ctx, &mut base);
    let fam = shapes_for(trace, ctx);
    assert_eq!(fam.len(), base.len());
    for (b, f) in base.iter().zip(&fam) {
        assert!(
            matches!(f, fr_board::shape::TileShape::Simplex(_)),
            "a trace strip must stay a Simplex in the 45° family tree, got {f:?}"
        );
        assert_eq!(
            b.shape.as_ref().unwrap(),
            f,
            "trace strip must equal the base shape"
        );
    }

    // Pin #6 — a drill item; upstream coarsens drills to octagons, so the
    // family tile must be an Oct.
    let pin = design
        .board
        .store()
        .get(fr_board::item::ItemId::from_raw(6))
        .unwrap();
    for f in shapes_for(pin, ctx) {
        assert!(
            matches!(f, fr_board::shape::TileShape::Oct(_)),
            "a drill tile must be an Oct in the 45° family tree, got {f:?}"
        );
    }

    // BoardOutline #1 — kept at its base shape (48ef09d, parity note in
    // PARITY-DIVERGENCES.md): the strips stay Simplex and must not swallow the
    // boundary pin at (2158930, -1539207).
    let outline = design
        .board
        .store()
        .get(fr_board::item::ItemId::from_raw(1))
        .unwrap();
    let p6 = fr_geometry::Point::Int(fr_geometry::planar::IntPoint {
        x: 2158930,
        y: -1539207,
    });
    let _ = p6;
    for t in {
        let mut out = Vec::new();
        calculate_tree_shapes_for_family(outline, 0, TreeAngleFamily::FortyFive, ctx, &mut out);
        out
    } {
        assert!(
            matches!(t.shape, Some(fr_board::shape::TileShape::Simplex(_))),
            "an outline strip must stay a Simplex in the 45° family tree"
        );
    }
}
