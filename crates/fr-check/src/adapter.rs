//! Wiring a built [`fr_board::Board`] to the referee, and reproducing the two
//! board-derived quantities the `java -drc` report prints.
//!
//! [`mod@crate::view`] promises "five one-liners" once the search-tree manager
//! exists. It exists, so this is them: [`shape_store`] copies the default tree's
//! per-item tile shapes into a [`ShapeStore`], after which the ordinary
//! [`StoreView`](crate::view::StoreView) is a [`BoardView`](crate::BoardView)
//! over any imported board.
//!
//! The second half of the module is *reporting*, not checking.
//! `DesignRulesChecker.convertToDrcViolation` (`drc-connectivity.md` §4.8) does
//! not print item ids or layers — it prints each item's **bounding-box centre of
//! gravity** and the required clearance, both converted to the report unit. Gate
//! G1.2a matches our violation multiset against Java's on exactly those numbers,
//! so [`item_bounding_box`] has to be `Item.bounding_box()` and nothing else.
//!
//! # `Item.bounding_box()` is not the tile-shape union
//!
//! It is tempting to union the default tree's tile shapes and be done. That is
//! wrong for two of the five kinds, and wrong in a way that moves the centre:
//!
//! | kind | upstream `bounding_box()` | tile-shape union |
//! |---|---|---|
//! | `PolylineTrace` | `lines.bounding_box().offset(half_width)` (`PolylineTrace.java:100`) | the segment offset shapes stop *at* the end corners, so the union is short by `half_width` at each end — and asymmetrically so on a bent trace |
//! | `DrillItem` | union of the **pad** shapes that exist (`DrillItem.java:179`) | v2 also synthesises a hole obstacle on padless inner layers (`search-tree-obstacles.md` §9.1), which can be wider than the pad |
//! | `BoardOutline` | union of the outline polygons (`BoardOutline.java:89`) | per-layer strips of half-width 100 around the border lines |
//! | `ObstacleArea` / `ConductionArea` | `getArea().boundingBox()` | same box, up to the convex-decomposition caveat below |
//!
//! So each kind is transcribed from its own `bounding_box()` override.
//!
//! # The one place this cannot be exact
//!
//! Area geometry is deliberately over-approximated one layer down and this
//! module inherits it: [`fr_board::search_tree::placed_area_pieces`] turns an
//! `AreaShape::Polygon` into its bounding octagon and — for a placement rotation
//! that is not a multiple of 90° — replaces every piece with the octagon of the
//! disc that *any* rotation of it stays inside, because `fr-geometry` has no
//! rotated-polygon type yet. Both are safe for a router (never a missed
//! obstacle) and both are visible to the referee, which does not want "safe", it
//! wants "identical". A 1 mm keepout on a component placed at 330° becomes a
//! 16 mm disc, and that is the entire ObstacleArea residue of gate G1.2a
//! (Issue054 +44, Issue022 +4, Issue029 +4 records against an oracle that has
//! exactly **one** ObstacleArea record in the whole 45-board corpus). The fix
//! belongs to `fr-geometry`/`fr-board`, not here.

use fr_board::item::{Item, ItemKind};
use fr_board::search_tree::placed_area_pieces;
use fr_board::{Board, ItemId};
use fr_geometry::planar::{FloatPoint, IntBox};

use crate::shape::TileShape;
use crate::view::{ItemShape, ShapeStore};

/// Copies the default search tree's tile shapes into a [`ShapeStore`].
///
/// The default tree is the uncompensated one (`compensated_class == 0`), which
/// is the tree `Item.clearanceViolations()` reads — see the module docs of
/// [`mod@crate::clearance`]. Padless index positions (`ItemTileShape::shape ==
/// None`) are dropped: the referee addresses shapes by position in a flat slice
/// and never derives a layer from a shape index, so the hole in the index space
/// carries no information here.
pub fn shape_store(board: &Board) -> ShapeStore {
    let mut store = ShapeStore::new();
    let tree = board.default_tree();
    let items = board.store();
    for id in items.ids() {
        let Some(slot) = items.slot_of(id) else {
            continue;
        };
        let shapes = tree
            .item_shapes(slot, id)
            .iter()
            .filter_map(|entry| {
                entry.shape.as_ref().map(|shape| ItemShape {
                    shape: convert_shape(shape),
                    layer: entry.layer,
                })
            })
            .collect::<Vec<_>>();
        store.set(id, shapes);
    }
    store
}

/// `fr_board::TileShape` → the referee's [`TileShape`].
///
/// The two enums are deliberately separate (see [`mod@crate::view`]); they carry
/// the same `fr-geometry` payloads, so this is a rename.
pub fn convert_shape(shape: &fr_board::TileShape) -> TileShape {
    match shape {
        fr_board::TileShape::Box(b) => TileShape::Box(*b),
        fr_board::TileShape::Oct(o) => TileShape::Octagon(*o),
        fr_board::TileShape::Simplex(s) => TileShape::Simplex(s.clone()),
    }
}

/// `Item.bounding_box()`, per subclass override.
///
/// Returns [`IntBox::EMPTY`] for an item with no geometry — upstream's
/// `IntBox.EMPTY` accumulator when every shape is null, and what a
/// `ComponentOutline` always yields.
pub fn item_bounding_box(board: &Board, item: &Item) -> IntBox {
    match &item.kind {
        // `PolylineTrace.java:100-102`.
        ItemKind::Trace(trace) => trace
            .polyline
            .bounding_box()
            .offset(f64::from(trace.half_width)),
        // `DrillItem.java:179-186`: union over the padstack's own layer span of
        // the pads that exist, translated to the item centre. `bounding_box` and
        // integer translation commute, so the shapes are never materialised.
        ItemKind::Pin(pin) => padstack_bounding_box(board, pin.padstack_no, pin.center),
        ItemKind::Via(via) => padstack_bounding_box(board, via.padstack_no, via.center),
        // `ObstacleArea.java:124`.
        ItemKind::ObstacleArea(area) => area_bounding_box(&area.area),
        ItemKind::ConductionArea(area) => area_bounding_box(&area.area),
        // `BoardOutline.java:89-94` — the outline polygons, not the tree strips.
        ItemKind::BoardOutline(outline) => {
            outline.borders.iter().fold(IntBox::EMPTY, |acc, border| {
                acc.union(&border.bounding_box())
            })
        }
        // `tile_shape_count() == 0`, so it can never appear in a violation.
        ItemKind::ComponentOutline(_) => IntBox::EMPTY,
    }
}

fn padstack_bounding_box(
    board: &Board,
    padstack_no: u32,
    center: fr_geometry::planar::IntPoint,
) -> IntBox {
    let Some(padstack) = board.padstacks().get(padstack_no) else {
        return IntBox::EMPTY;
    };
    let mut result = IntBox::EMPTY;
    for layer in padstack.from_layer..=padstack.to_layer {
        if let Some(pad) = padstack.shape_on_layer(layer) {
            result = result.union(&pad.bounding_box());
        }
    }
    if result.is_empty() {
        return result;
    }
    IntBox::new(
        result.ll.x + center.x,
        result.ll.y + center.y,
        result.ur.x + center.x,
        result.ur.y + center.y,
    )
}

fn area_bounding_box(area: &fr_board::PlacedArea) -> IntBox {
    placed_area_pieces(area)
        .iter()
        .fold(IntBox::EMPTY, |acc, piece| acc.union(&piece.bounding_box()))
}

/// `IntBox.centre_of_gravity()` — `PolylineShape.centre_of_gravity` on a box,
/// i.e. the arithmetic mean of the four corners.
///
/// Written as the mean of the four corners rather than as the midpoint of the
/// diagonal because that is upstream's loop; on integer bounds the two agree
/// bit-for-bit, and keeping the loop shape makes the transcription checkable.
pub fn centre_of_gravity(box_: &IntBox) -> FloatPoint {
    let x =
        f64::from(box_.ll.x) + f64::from(box_.ur.x) + f64::from(box_.ur.x) + f64::from(box_.ll.x);
    let y =
        f64::from(box_.ll.y) + f64::from(box_.ll.y) + f64::from(box_.ur.y) + f64::from(box_.ur.y);
    FloatPoint::new(x / 4.0, y / 4.0)
}

/// The [`ItemId`]s of every item on the board, in the store's order.
///
/// A convenience for callers that need the id list before they have a view.
pub fn item_ids(board: &Board) -> Vec<ItemId> {
    board.store().ids().collect()
}

/// `DesignRulesChecker.isHole` (`:417-424`) — a `Via` **or a `Pin`**.
///
/// Upstream's own comment concedes the classification is wrong ("Pins are
/// treated as holes for DRC classification to match expected output, although
/// this might include SMT pins"), and it is transcribed with the wrongness
/// intact: an SMD pad with no drill at all still labels its violations
/// `hole_clearance`, which is why 1 138 of Issue027's 1 138 records carry that
/// type on a board whose overlaps are pad-on-pad.
pub fn is_hole(item: &Item) -> bool {
    matches!(item.kind, ItemKind::Pin(_) | ItemKind::Via(_))
}

/// The `type` string the KiCad DRC report gives a clearance violation between
/// two items.
///
/// **There is no second geometric check behind `hole_clearance`.** Upstream runs
/// one clearance pass and labels each record afterwards
/// (`convertToDrcViolation`, `:389-392`); the mechanism that *would* make drill
/// holes a distinct obstacle — the `hole_edge` clearance class and the
/// `drillHoleClearanceDelta` inflation of drill-item tree shapes
/// (`drc-connectivity.md` §1.3, §1.4) — is inert at the shipped
/// `holeClearanceUm = 0.0`, and on the `-drc` code path is never configured at
/// all. So `clearance` and `hole_clearance` partition one multiset, and their
/// sum is the `M` of `"(N unrouted and M violations)"`.
pub fn violation_type(a: &Item, b: &Item) -> &'static str {
    if is_hole(a) || is_hole(b) {
        "hole_clearance"
    } else {
        "clearance"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fr_board::item::{LayerRange, PolylineTrace, Via};
    use fr_board::{ClearanceTable, LayerStructure, NetNos, Padstack, TileShape as BoardTileShape};
    use fr_geometry::planar::{IntPoint, Polyline};

    fn board_with_one_layer() -> Board {
        Board::new(
            LayerStructure::signal_layers(1),
            ClearanceTable::uniform(2, 1, 20),
        )
    }

    #[test]
    fn centre_of_gravity_is_the_box_centre() {
        let c = centre_of_gravity(&IntBox::new(-3, 0, 5, 10));
        assert_eq!(c.x, 1.0);
        assert_eq!(c.y, 5.0);
    }

    #[test]
    fn a_trace_box_is_the_corner_box_grown_by_the_half_width() {
        let mut board = board_with_one_layer();
        let polyline = Polyline::from_int_points(&[IntPoint::new(0, 0), IntPoint::new(100, 0)]);
        let id = board.insert_item(Item::new(
            NetNos::from_slice(&[1]),
            1,
            fr_board::FixedState::Unfixed,
            ItemKind::Trace(PolylineTrace {
                polyline,
                layer: 0,
                half_width: 10,
            }),
        ));
        let item = board.store().get(id).expect("just inserted");
        // Upstream grows the *corner* box, so the ends stick out by half_width;
        // the tile shapes stop at the corners and would give x in [0, 100].
        assert_eq!(
            item_bounding_box(&board, item),
            IntBox::new(-10, -10, 110, 10)
        );
    }

    #[test]
    fn a_drill_item_box_is_the_pad_union_at_the_centre() {
        let mut board = board_with_one_layer();
        let padstack_no = board.padstacks_mut().push(Padstack::uniform(
            0,
            0,
            BoardTileShape::Box(IntBox::new(-4, -6, 4, 6)),
        ));
        let id = board.insert_item(Item::new(
            NetNos::from_slice(&[1]),
            1,
            fr_board::FixedState::Unfixed,
            ItemKind::Via(Via {
                center: IntPoint::new(50, 70),
                padstack_no,
                layers: LayerRange::new(0, 0),
                attach_allowed: false,
                is_escape_via: false,
                escape_via_smd_layer: -1,
            }),
        ));
        let item = board.store().get(id).expect("just inserted");
        assert_eq!(item_bounding_box(&board, item), IntBox::new(46, 64, 54, 76));
        assert_eq!(
            centre_of_gravity(&item_bounding_box(&board, item)),
            FloatPoint::new(50.0, 70.0)
        );
    }

    #[test]
    fn an_smd_pad_still_labels_its_violations_hole_clearance() {
        // Upstream's `isHole` is a type test, not a drill test, and it is the
        // whole of the `hole_clearance` semantics.
        let smd = Item::new(
            NetNos::from_slice(&[1]),
            1,
            fr_board::FixedState::Unfixed,
            ItemKind::Pin(fr_board::item::Pin {
                center: IntPoint::new(0, 0),
                padstack_no: 0,
                pin_no: 0,
                layers: LayerRange::new(0, 0),
            }),
        );
        let trace = Item::new(
            NetNos::from_slice(&[2]),
            1,
            fr_board::FixedState::Unfixed,
            ItemKind::Trace(PolylineTrace {
                polyline: Polyline::from_int_points(&[IntPoint::new(0, 0), IntPoint::new(10, 0)]),
                layer: 0,
                half_width: 1,
            }),
        );
        assert!(is_hole(&smd));
        assert!(!is_hole(&trace));
        assert_eq!(violation_type(&smd, &trace), "hole_clearance");
        assert_eq!(violation_type(&trace, &smd), "hole_clearance");
        assert_eq!(violation_type(&trace, &trace), "clearance");
    }

    #[test]
    fn the_shape_store_mirrors_the_default_tree() {
        let mut board = board_with_one_layer();
        let padstack_no = board.padstacks_mut().push(Padstack::round(0, 0, 5));
        let id = board.insert_item(Item::new(
            NetNos::from_slice(&[1]),
            1,
            fr_board::FixedState::Unfixed,
            ItemKind::Via(Via {
                center: IntPoint::new(0, 0),
                padstack_no,
                layers: LayerRange::new(0, 0),
                attach_allowed: false,
                is_escape_via: false,
                escape_via_smd_layer: -1,
            }),
        ));
        let store = shape_store(&board);
        assert_eq!(store.get(id).len(), 1);
        assert_eq!(store.get(id)[0].layer, 0);
    }
}
