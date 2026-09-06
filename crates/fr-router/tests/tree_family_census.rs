//! Audit census: for every item of every 45°-restricted fixture, classify the
//! 45°-family post-pass effect per item type. The family shape is always the
//! enclosing bounding octagon of the base shape, so the effect is
//! region-preserving iff `fam.area() == base.area()` (both convex, fam ⊇ base).

use std::path::PathBuf;

use fr_board::search_tree::{calculate_tree_shapes_for_family, TreeAngleFamily};
use fr_board::shape::AngleRestriction;
use fr_io::specctra;

fn load(name: &str) -> fr_io::specctra::ImportedDesign {
    let bytes = std::fs::read(PathBuf::from(format!(
        "/tmp/freerouting-upstream/fixtures/{name}"
    )))
    .unwrap_or_else(|e| panic!("fixture {name}: {e}"));
    specctra::import(&bytes).expect("imports")
}

#[test]
fn census_45_family_region_changes() {
    let fixtures = [
        "Issue054-tairakb.dsn",
        "Issue110-RelayModule.dsn",
        "Issue110-Pajalnaja_stancija.dsn",
        "Issue163-pic_programmer.dsn",
        "Issue155-CH376_MCP795_Module.dsn",
        "Issue034-Green14SegLED.dsn",
        "Issue026-J2_reference.dsn",
        "Issue107-freq_teiler_200kHz_kicad.dsn",
        "Issue217-8088sbc.dsn",
        "Issue230-CNH_Functional_Tester_1.dsn",
    ];
    let mut total = [0u64; 6]; // trace, pin, via, obs, cond, outline
    let mut changed = [0u64; 6];
    let clearances = fr_board::clearance::ClearanceTable::new(2, 2);

    for name in fixtures {
        let design = load(name);
        assert_eq!(
            design.board.angle_restriction(),
            AngleRestriction::FortyFive
        );
        let ctx = fr_board::search_tree::ShapeContext {
            clearances: &clearances,
            padstacks: design.board.padstacks(),
            layer_count: design.board.layers().len(),
            max_tree_shape_width: design.board.max_tree_shape_width(),
            angle_restriction: AngleRestriction::None,
        };
        for (_, item) in design.board.store().iter() {
            let bucket = match &item.kind {
                fr_board::item::ItemKind::Trace(_) => 0,
                fr_board::item::ItemKind::Pin(_) => 1,
                fr_board::item::ItemKind::Via(_) => 2,
                fr_board::item::ItemKind::ObstacleArea(_) => 3,
                fr_board::item::ItemKind::ConductionArea(_) => 4,
                fr_board::item::ItemKind::BoardOutline(_) => 5,
                _ => continue,
            };
            let mut base = Vec::new();
            fr_board::search_tree::calculate_tree_shapes(item, 0, ctx, &mut base);
            let ctx45 = fr_board::search_tree::ShapeContext {
                angle_restriction: AngleRestriction::FortyFive,
                ..ctx
            };
            let mut fam = Vec::new();
            calculate_tree_shapes_for_family(item, 0, TreeAngleFamily::FortyFive, ctx45, &mut fam);
            total[bucket] += 1;
            for (b, f) in base.iter().zip(&fam) {
                if let (
                    Some(fr_board::shape::TileShape::Simplex(s)),
                    Some(fr_board::shape::TileShape::Oct(o)),
                ) = (&b.shape, &f.shape)
                {
                    if s.area() != o.area() {
                        changed[bucket] += 1;
                        break;
                    }
                }
            }
        }
    }
    let names = [
        "Trace",
        "Pin",
        "Via",
        "ObstacleArea",
        "ConductionArea",
        "BoardOutline",
    ];
    eprintln!("=== 45° family post-pass region-change census (10 fixtures) ===");
    for i in 0..6 {
        eprintln!(
            "  {:<14} items={:<6} region-changed={}",
            names[i], total[i], changed[i]
        );
    }
}
