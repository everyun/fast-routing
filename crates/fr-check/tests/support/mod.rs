//! A hand-built board for the referee tests.
//!
//! The checker must not assume how a board was constructed, so the tests do not
//! go through the DSN importer or the search-tree manager: they push items into
//! an [`ItemStore`] and hand the tile shapes over directly. That is exactly the
//! [`BoardView`](fr_check::BoardView) contract, so a test board and a real board
//! are indistinguishable to everything under test.

#![allow(dead_code)]

use fr_board::item::{
    AreaShape, ConductionArea, FixedState, Item, ItemId, ItemKind, LayerRange, ObstacleArea,
    ObstacleKind, Pin, PlacedArea, PolylineTrace, Via,
};
use fr_board::{ItemStore, NetNos};
use fr_check::view::{ClearanceSource, ItemShape, ShapeStore, StoreView};
use fr_check::TileShape;
use fr_geometry::planar::{IntBox, IntPoint, Polyline};

/// An item store plus the tile shapes somebody would have derived for it.
pub struct Scaffold {
    pub store: ItemStore,
    pub shapes: ShapeStore,
    pub layer_count: usize,
}

impl Scaffold {
    pub fn new(layer_count: usize) -> Self {
        Scaffold {
            store: ItemStore::new(),
            shapes: ShapeStore::new(),
            layer_count,
        }
    }

    /// Inserts an item together with its tile shapes.
    pub fn insert(&mut self, item: Item, shapes: Vec<ItemShape>) -> ItemId {
        let id = self.store.insert(item);
        self.shapes.set(id, shapes);
        id
    }

    /// Inserts an item whose only tile shape is `shape` on `layer`.
    pub fn insert_one(&mut self, item: Item, shape: impl Into<TileShape>, layer: i32) -> ItemId {
        self.insert(item, vec![ItemShape::new(shape, layer)])
    }

    pub fn view<'a, C: ClearanceSource>(&'a self, clearance: &'a C) -> StoreView<'a, C> {
        StoreView::new(&self.store, &self.shapes, clearance, self.layer_count)
    }
}

// --- item constructors -----------------------------------------------------

pub fn trace_item(nets: &[i32], layer: i32, corners: &[IntPoint], half_width: i32) -> Item {
    Item::new(
        NetNos::from_slice(nets),
        1,
        FixedState::Unfixed,
        ItemKind::Trace(PolylineTrace {
            polyline: Polyline::from_int_points(corners),
            layer,
            half_width,
        }),
    )
}

pub fn pin_item(nets: &[i32], center: IntPoint, layers: LayerRange) -> Item {
    Item::new(
        NetNos::from_slice(nets),
        1,
        FixedState::SystemFixed,
        ItemKind::Pin(Pin {
            center,
            padstack_no: 0,
            pin_no: 0,
            layers,
        }),
    )
}

pub fn via_item(nets: &[i32], center: IntPoint, layers: LayerRange) -> Item {
    Item::new(
        NetNos::from_slice(nets),
        1,
        FixedState::Unfixed,
        ItemKind::Via(Via {
            center,
            padstack_no: 0,
            layers,
            attach_allowed: true,
            is_escape_via: false,
            escape_via_smd_layer: -1,
        }),
    )
}

pub fn keepout_item(nets: &[i32], layer: i32, area: IntBox, kind: ObstacleKind) -> Item {
    Item::new(
        NetNos::from_slice(nets),
        1,
        FixedState::SystemFixed,
        ItemKind::ObstacleArea(ObstacleArea {
            area: PlacedArea::new(AreaShape::Box(area)),
            layer,
            kind,
        }),
    )
}

pub fn pour_item(nets: &[i32], layer: i32, area: IntBox, is_obstacle: bool) -> Item {
    Item::new(
        NetNos::from_slice(nets),
        1,
        FixedState::SystemFixed,
        ItemKind::ConductionArea(ConductionArea {
            area: PlacedArea::new(AreaShape::Box(area)),
            layer,
            is_obstacle,
            is_filled: true,
        }),
    )
}

/// A straight two-corner trace and the single tile shape a search tree would
/// derive for it.
///
/// For an axis-parallel segment `polyline.offsetShape(halfWidth, 0)` is the
/// segment's box grown **perpendicular** to the run only — the end lines cap it
/// flush with the corners. A diagonal segment falls back to the bounding box
/// grown in both directions, which is a superset; no test relies on the
/// difference.
pub fn straight_trace(
    nets: &[i32],
    layer: i32,
    from: IntPoint,
    to: IntPoint,
    half_width: i32,
) -> (Item, TileShape) {
    let item = trace_item(nets, layer, &[from, to], half_width);
    let hw = half_width;
    let (lo_x, hi_x) = (from.x.min(to.x), from.x.max(to.x));
    let (lo_y, hi_y) = (from.y.min(to.y), from.y.max(to.y));
    let boxed = if from.y == to.y {
        IntBox::new(lo_x, lo_y - hw, hi_x, hi_y + hw)
    } else if from.x == to.x {
        IntBox::new(lo_x - hw, lo_y, hi_x + hw, hi_y)
    } else {
        IntBox::new(lo_x - hw, lo_y - hw, hi_x + hw, hi_y + hw)
    };
    (item, TileShape::Box(boxed))
}
