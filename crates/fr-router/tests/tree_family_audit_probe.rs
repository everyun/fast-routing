//! Audit probe (tree-family-audit): for every item of every 45°-restricted
//! fixture, compare the 45°-family tree tile against the any-angle (base)
//! tree tile. Upstream's 45° tree (`ShapeSearchTree45Degree`) re-flavors ONLY
//! ObstacleArea/ConductionArea (`:442-451`) and BoardOutline (`:453-460`) to
//! their enclosing octagons, and builds DrillItems as octagons directly
//! (`:411-440`); `PolylineTrace` has **no** override and keeps the base
//! class's concave Simplex strips (`ShapeSearchTree.java:1039-1051`). Our
//! family post-pass (`calculate_tree_shapes_for_family`) octagonizes
//! *everything*, so any trace tile that differs between the two trees here is
//! a divergence from upstream.

use std::path::PathBuf;

use fr_board::search_tree::{calculate_tree_shapes_for_family, TreeAngleFamily};
use fr_board::shape::AngleRestriction;
use fr_io::specctra;

fn fixture(name: &str) -> fr_io::specctra::ImportedDesign {
    let bytes = std::fs::read(PathBuf::from(format!(
        "/tmp/freerouting-upstream/fixtures/{name}"
    )))
    .unwrap_or_else(|e| panic!("fixture {name}: {e}"));
    specctra::import(&bytes).expect("imports")
}

#[test]
fn audit_45_family_vs_base_per_item_type() {
    let fixtures = [
        "Issue054-tairakb.dsn",
        "Issue110-RelayModule.dsn",
        "Issue110-Pajalnaja_stancija.dsn",
        "Issue163-pic_programmer.dsn",
        "Issue155-CH376_MCP795_Module.dsn",
        "Issue034-Green14SegLED.dsn",
    ];

    // shape context stubs: compensation zeroed — the flavor pass is the only
    // difference being measured, and it does not depend on compensation.
    let clearances = fr_board::clearance::ClearanceTable::new(2, 2);
    for name in fixtures {
        let design = fixture(name);
        if design.board.angle_restriction() != AngleRestriction::FortyFive {
            eprintln!("{name}: not a 45° board, skipped");
            continue;
        }
        let layer_count = design.board.layers().len();
        let padstacks = design.board.padstacks();
        let max_width = design.board.max_tree_shape_width();
        let mut diffs = [0usize; 6]; // per ItemClass bucket
        let mut checked = 0usize;

        for (id, item) in design.board.store().iter() {
            let _slot = design.board.store().slot_of(id).unwrap();
            let ctx = fr_board::search_tree::ShapeContext {
                clearances: &clearances,
                padstacks,
                layer_count,
                max_tree_shape_width: max_width,
                angle_restriction: AngleRestriction::None, // base = any-angle
            };
            let mut base = Vec::new();
            let mut fam = Vec::new();
            fr_board::search_tree::calculate_tree_shapes(item, 0, ctx, &mut base);
            let ctx45 = fr_board::search_tree::ShapeContext {
                angle_restriction: AngleRestriction::FortyFive,
                ..ctx
            };
            calculate_tree_shapes_for_family(item, 0, TreeAngleFamily::FortyFive, ctx45, &mut fam);
            checked += 1;
            let same = base.len() == fam.len()
                && base.iter().zip(&fam).all(|(b, f)| {
                    b.layer == f.layer
                        && match (&b.shape, &f.shape) {
                            (Some(a), Some(c)) => a == c,
                            (None, None) => true,
                            _ => false,
                        }
                });
            if !same {
                let bucket = match &item.kind {
                    fr_board::item::ItemKind::Trace(_) => 0,
                    fr_board::item::ItemKind::Pin(_) => 1,
                    fr_board::item::ItemKind::Via(_) => 2,
                    fr_board::item::ItemKind::ObstacleArea(_) => 3,
                    fr_board::item::ItemKind::ConductionArea(_) => 4,
                    fr_board::item::ItemKind::BoardOutline(_) => 5,
                    _ => continue,
                };
                diffs[bucket] += 1;
                if diffs[bucket] <= 2 {
                    let which = [
                        "Trace",
                        "Pin",
                        "Via",
                        "ObstacleArea",
                        "ConductionArea",
                        "BoardOutline",
                    ][bucket];
                    eprintln!(
                        "{name}: {which} item #{id} DIFFERS: base={} fam={}",
                        base.iter()
                            .map(|t| format!("{:?}", t.shape.is_some()))
                            .collect::<Vec<_>>()
                            .join(","),
                        fam.iter()
                            .map(|t| format!("{:?}", t.shape.is_some()))
                            .collect::<Vec<_>>()
                            .join(","),
                    );
                }
            }
        }
        eprintln!(
            "{name}: checked {checked} items, diffs trace={} pin={} via={} obs={} cond={} outline={}",
            diffs[0], diffs[1], diffs[2], diffs[3], diffs[4], diffs[5]
        );
    }
}
