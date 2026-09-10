//! Board snapshot to GUI render model.
//!
//! Converts a [`fr_board::Board`] into a self-contained [`RenderModel`] for gpui rendering,
//! matching upstream Freerouting Java GUI visual structures, default layer colors,
//! and ratsnest connectivity.

use fr_board::item::{AreaShape as BoardAreaShape, ItemId, ItemKind, PlacedArea};
use fr_board::shape::TileShape;
use fr_board::Board;
use fr_check::adapter::{convert_shape, item_bounding_box};
use fr_check::connectivity::board_connectivity;
use fr_check::view::{ItemShape, ShapeStore, StoreView, UniformClearance};
use fr_geometry::planar::{IntBox, IntPoint};

/// Default coordinate resolution factor: board units to millimetres.
/// For classic Specctra / KiCad 10 000 units/mm (0.1 µm), 1 board unit = 0.0001 mm.
pub const DEFAULT_RESOLUTION: f64 = 0.0001;

/// The complete render model for one snapshot frame.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderModel {
    /// Layer table, index = layer number.
    pub layers: Vec<LayerInfo>,
    /// Bounding box: [xmin, ymin, xmax, ymax] in board units.
    pub bbox: [f64; 4],
    /// Board units to mm scaling factor.
    pub resolution: f64,
    /// Routed traces.
    pub traces: Vec<TraceShape>,
    /// Vias.
    pub vias: Vec<ViaShape>,
    /// Component pads / pins.
    pub pads: Vec<PadShape>,
    /// Copper planes, keepouts, board outlines, other areas.
    pub areas: Vec<AreaShape>,
    /// Ratsnest airline missing connections.
    pub ratsnest: Vec<RatLine>,
    /// Current routing engine statistics.
    pub stats: Stats,
}

/// Metadata and default display color for one layer.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerInfo {
    pub name: String,
    pub is_signal: bool,
    /// RGBA in [0.0, 1.0].
    pub color: [f32; 4],
}

/// Renderable routed trace polyline.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceShape {
    pub layer: u16,
    pub net: i32,
    pub half_width: f64,
    pub points: Vec<[f64; 2]>,
    pub fixed: FixState,
}

/// Renderable via.
#[derive(Debug, Clone, PartialEq)]
pub struct ViaShape {
    pub layer_from: u16,
    pub layer_to: u16,
    pub net: i32,
    pub center: [f64; 2],
    pub radius: f64,
    pub fixed: FixState,
}

/// Renderable component pad / pin.
#[derive(Debug, Clone, PartialEq)]
pub struct PadShape {
    pub layer: u16,
    pub net: i32,
    pub polygon: Vec<[f64; 2]>,
    pub is_smd: bool,
    pub component: String,
    pub pin: String,
}

/// Renderable polygon area (copper pour, keepout, outline).
#[derive(Debug, Clone, PartialEq)]
pub struct AreaShape {
    pub layer: u16,
    pub net: i32,
    pub polygon: Vec<[f64; 2]>,
    pub kind: AreaKind,
}

/// Kind of area shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AreaKind {
    Plane,
    Keepout,
    Outline,
    Other,
}

/// One unrouted connection flight line in the ratsnest.
#[derive(Debug, Clone, PartialEq)]
pub struct RatLine {
    pub net: i32,
    pub from: [f64; 2],
    pub to: [f64; 2],
    pub layer_from: u16,
    pub layer_to: u16,
}

/// Fix state of an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FixState {
    #[default]
    Unfixed,
    ShoveFixed,
    UserFixed,
    SystemFixed,
}

impl From<fr_board::item::FixedState> for FixState {
    fn from(s: fr_board::item::FixedState) -> Self {
        match s {
            fr_board::item::FixedState::Unfixed => FixState::Unfixed,
            fr_board::item::FixedState::ShoveFixed => FixState::ShoveFixed,
            fr_board::item::FixedState::UserFixed => FixState::UserFixed,
            fr_board::item::FixedState::SystemFixed => FixState::SystemFixed,
        }
    }
}

impl From<FixState> for fr_board::item::FixedState {
    fn from(s: FixState) -> Self {
        match s {
            FixState::Unfixed => fr_board::item::FixedState::Unfixed,
            FixState::ShoveFixed => fr_board::item::FixedState::ShoveFixed,
            FixState::UserFixed => fr_board::item::FixedState::UserFixed,
            FixState::SystemFixed => fr_board::item::FixedState::SystemFixed,
        }
    }
}

/// Router engine statistics reported to the UI.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Stats {
    pub incomplete: u32,
    pub violations: u32,
    pub pass: u32,
    pub elapsed_s: f64,
    pub score: f64,
    pub items: u32,
}

/// Default layer color matching Java Freerouting `ItemColorTableModel`.
///
/// Signal layer 0: Red `(200, 52, 52)`
/// Back signal layer: Blue `(77, 127, 196)`
/// Inner signal layers: 6-color rotation (light green, orange, cyan, pink, lavender, turquoise).
/// Non-signal (power/plane): Gray `(128, 128, 128)`.
pub fn default_layer_color(
    layer: usize,
    total_layers: usize,
    is_signal: bool,
    signal_layer_index: usize,
) -> [f32; 4] {
    if !is_signal {
        return [128.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0, 1.0];
    }
    if layer == 0 {
        [200.0 / 255.0, 52.0 / 255.0, 52.0 / 255.0, 1.0]
    } else if total_layers > 1 && layer == total_layers - 1 {
        [77.0 / 255.0, 127.0 / 255.0, 196.0 / 255.0, 1.0]
    } else {
        match signal_layer_index % 6 {
            1 => [127.0 / 255.0, 200.0 / 255.0, 127.0 / 255.0, 1.0], // light green
            2 => [206.0 / 255.0, 125.0 / 255.0, 44.0 / 255.0, 1.0],  // orange
            3 => [79.0 / 255.0, 203.0 / 255.0, 203.0 / 255.0, 1.0],  // cyan
            4 => [219.0 / 255.0, 98.0 / 255.0, 139.0 / 255.0, 1.0],  // pink
            5 => [167.0 / 255.0, 165.0 / 255.0, 198.0 / 255.0, 1.0], // lavender
            _ => [40.0 / 255.0, 204.0 / 255.0, 217.0 / 255.0, 1.0],  // turquoise
        }
    }
}

/// Takes a snapshot of `board` with default resolution (0.0001 mm / unit).
pub fn snapshot(board: &Board, stats: Stats) -> RenderModel {
    snapshot_with_resolution(board, stats, DEFAULT_RESOLUTION)
}

/// Takes a snapshot of `board` with an explicit coordinate resolution factor.
pub fn snapshot_with_resolution(board: &Board, mut stats: Stats, resolution: f64) -> RenderModel {
    let layer_count = board.layers().len();

    // 1. Build layer table with Java default colors
    let mut layers = Vec::with_capacity(layer_count);
    let mut signal_layer_no = 0usize;
    for l in 0..layer_count {
        let is_sig = board.layers().is_signal(l as i32);
        let name = board
            .layers()
            .name(l as i32)
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("Layer_{l}"));
        let color = default_layer_color(l, layer_count, is_sig, signal_layer_no);
        if is_sig {
            signal_layer_no += 1;
        }
        layers.push(LayerInfo {
            name,
            is_signal: is_sig,
            color,
        });
    }

    // 2. Iterate items: collect traces, vias, pads, areas, and calculate bounding box
    let mut traces = Vec::new();
    let mut vias = Vec::new();
    let mut pads = Vec::new();
    let mut areas = Vec::new();
    let mut total_bbox = IntBox::EMPTY;

    for (_, item) in board.store().iter_ascending() {
        let bbox = item_bounding_box(board, item);
        if !bbox.is_empty() {
            total_bbox = total_bbox.union(&bbox);
        }

        let net = item.nets.as_slice().first().copied().unwrap_or(0);
        let fixed = FixState::from(item.fixed_state);

        match &item.kind {
            ItemKind::Trace(trace) => {
                let points = trace
                    .polyline
                    .corner_approx_arr()
                    .into_iter()
                    .map(|fp| [fp.x, fp.y])
                    .collect();
                traces.push(TraceShape {
                    layer: trace.layer.max(0) as u16,
                    net,
                    half_width: trace.half_width as f64,
                    points,
                    fixed,
                });
            }
            ItemKind::Via(via) => {
                let radius = if let Some(padstack) = board.padstacks().get(via.padstack_no) {
                    if padstack.drill_radius > 0 {
                        padstack.drill_radius as f64
                    } else {
                        padstack
                            .shapes
                            .iter()
                            .filter_map(|s| s.as_ref())
                            .next()
                            .map(|s| s.bounding_box().max_width() / 2.0)
                            .unwrap_or(0.0)
                    }
                } else {
                    0.0
                };
                vias.push(ViaShape {
                    layer_from: via.layers.first.max(0) as u16,
                    layer_to: via.layers.last.max(0) as u16,
                    net,
                    center: [via.center.x as f64, via.center.y as f64],
                    radius,
                    fixed,
                });
            }
            ItemKind::Pin(pin) => {
                let is_smd = pin.layers.is_single();
                let component = if item.component_no > 0 {
                    format!("C{}", item.component_no)
                } else {
                    String::new()
                };
                let pin_str = pin.pin_no.to_string();

                if let Some(padstack) = board.padstacks().get(pin.padstack_no) {
                    for layer in pin.layers.first..=pin.layers.last {
                        if let Some(shape) = padstack.shape_on_layer(layer) {
                            let polygon = tile_shape_to_polygon(shape, pin.center);
                            if !polygon.is_empty() {
                                pads.push(PadShape {
                                    layer: layer.max(0) as u16,
                                    net,
                                    polygon,
                                    is_smd,
                                    component: component.clone(),
                                    pin: pin_str.clone(),
                                });
                            }
                        }
                    }
                }
            }
            ItemKind::ObstacleArea(obstacle) => {
                let polygons = placed_area_to_polygons(&obstacle.area);
                for polygon in polygons {
                    areas.push(AreaShape {
                        layer: obstacle.layer.max(0) as u16,
                        net,
                        polygon,
                        kind: AreaKind::Keepout,
                    });
                }
            }
            ItemKind::ConductionArea(conduction) => {
                let polygons = placed_area_to_polygons(&conduction.area);
                for polygon in polygons {
                    areas.push(AreaShape {
                        layer: conduction.layer.max(0) as u16,
                        net,
                        polygon,
                        kind: AreaKind::Plane,
                    });
                }
            }
            ItemKind::BoardOutline(outline) => {
                for border in &outline.borders {
                    let mut polygon: Vec<[f64; 2]> = border
                        .corner_approx_arr()
                        .into_iter()
                        .map(|p| [p.x, p.y])
                        .collect();
                    if polygon.len() > 1 && polygon[0] == *polygon.last().unwrap() {
                        polygon.pop();
                    }
                    if !polygon.is_empty() {
                        areas.push(AreaShape {
                            layer: 0,
                            net: 0,
                            polygon,
                            kind: AreaKind::Outline,
                        });
                    }
                }
            }
            ItemKind::ComponentOutline(comp) => {
                let layer = if comp.is_front {
                    0
                } else {
                    layer_count.saturating_sub(1) as u16
                };
                let polygons = placed_area_to_polygons(&comp.area);
                for polygon in polygons {
                    areas.push(AreaShape {
                        layer,
                        net: 0,
                        polygon,
                        kind: AreaKind::Other,
                    });
                }
            }
        }
    }

    // 3. Compute ratsnest (missing connection airlines)
    let ratsnest = compute_ratsnest(board);

    // 4. Populate / update stats
    if stats.items == 0 {
        stats.items = board.store().len() as u32;
    }
    if stats.incomplete == 0 && !ratsnest.is_empty() {
        stats.incomplete = ratsnest.len() as u32;
    }

    let bbox = if total_bbox.is_empty() {
        [0.0, 0.0, 0.0, 0.0]
    } else {
        [
            total_bbox.ll.x as f64,
            total_bbox.ll.y as f64,
            total_bbox.ur.x as f64,
            total_bbox.ur.y as f64,
        ]
    };

    RenderModel {
        layers,
        bbox,
        resolution,
        traces,
        vias,
        pads,
        areas,
        ratsnest,
        stats,
    }
}

/// Converts a placed area directly into polygon corner lists, applying rotation, mirror, translation.
fn placed_area_to_polygons(area: &PlacedArea) -> Vec<Vec<[f64; 2]>> {
    let rotation = area.rotation_deg.rem_euclid(360.0);
    let rad = rotation.to_radians();
    let cos = rad.cos();
    let sin = rad.sin();
    let tx = area.translation.x as f64;
    let ty = area.translation.y as f64;

    let transform_pt = |x: f64, y: f64| -> [f64; 2] {
        let (rx, ry) = if rotation != 0.0 {
            (x * cos - y * sin, x * sin + y * cos)
        } else {
            (x, y)
        };
        let my = if area.side_changed { -ry } else { ry };
        [rx + tx, my + ty]
    };

    match &area.relative_area {
        BoardAreaShape::Box(b) => {
            if b.is_empty() {
                Vec::new()
            } else {
                vec![vec![
                    transform_pt(b.ll.x as f64, b.ll.y as f64),
                    transform_pt(b.ur.x as f64, b.ll.y as f64),
                    transform_pt(b.ur.x as f64, b.ur.y as f64),
                    transform_pt(b.ll.x as f64, b.ur.y as f64),
                ]]
            }
        }
        BoardAreaShape::Octagon(o) => {
            if o.is_empty() {
                Vec::new()
            } else {
                let mut pts: Vec<[f64; 2]> = Vec::with_capacity(8);
                for i in 0..8 {
                    let c = o.corner(i);
                    let pt = transform_pt(c.x as f64, c.y as f64);
                    if pts.last().is_none_or(|last| *last != pt) {
                        pts.push(pt);
                    }
                }
                if pts.len() > 1 && pts[0] == *pts.last().unwrap() {
                    pts.pop();
                }
                vec![pts]
            }
        }
        BoardAreaShape::Polygon(poly) => {
            let corners = poly.corner_array();
            if corners.is_empty() {
                Vec::new()
            } else {
                let mut pts: Vec<[f64; 2]> = corners
                    .iter()
                    .map(|p| {
                        let fp = p.to_float();
                        transform_pt(fp.x, fp.y)
                    })
                    .collect();
                if pts.len() > 1 && pts[0] == *pts.last().unwrap() {
                    pts.pop();
                }
                vec![pts]
            }
        }
        BoardAreaShape::Simplex(s) => {
            if s.is_empty() {
                Vec::new()
            } else {
                let mut pts: Vec<[f64; 2]> = s
                    .corner_approx_arr()
                    .into_iter()
                    .map(|p| transform_pt(p.x, p.y))
                    .collect();
                if pts.len() > 1 && pts[0] == *pts.last().unwrap() {
                    pts.pop();
                }
                vec![pts]
            }
        }
        BoardAreaShape::Convex(pieces) => pieces
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| {
                let mut pts: Vec<[f64; 2]> = s
                    .corner_approx_arr()
                    .into_iter()
                    .map(|p| transform_pt(p.x, p.y))
                    .collect();
                if pts.len() > 1 && pts[0] == *pts.last().unwrap() {
                    pts.pop();
                }
                pts
            })
            .collect(),
    }
}

/// Converts a [`TileShape`] at an offset into an array of polygon vertices.
fn tile_shape_to_polygon(shape: &TileShape, offset: IntPoint) -> Vec<[f64; 2]> {
    let ox = offset.x as f64;
    let oy = offset.y as f64;
    match shape {
        TileShape::Box(b) => {
            if b.is_empty() {
                Vec::new()
            } else {
                vec![
                    [b.ll.x as f64 + ox, b.ll.y as f64 + oy],
                    [b.ur.x as f64 + ox, b.ll.y as f64 + oy],
                    [b.ur.x as f64 + ox, b.ur.y as f64 + oy],
                    [b.ll.x as f64 + ox, b.ur.y as f64 + oy],
                ]
            }
        }
        TileShape::Oct(o) => {
            if o.is_empty() {
                Vec::new()
            } else {
                let mut pts: Vec<[f64; 2]> = Vec::with_capacity(8);
                for i in 0..8 {
                    let c = o.corner(i);
                    let pt = [c.x as f64 + ox, c.y as f64 + oy];
                    if pts.last().is_none_or(|last| *last != pt) {
                        pts.push(pt);
                    }
                }
                if pts.len() > 1 && pts[0] == *pts.last().unwrap() {
                    pts.pop();
                }
                pts
            }
        }
        TileShape::Simplex(s) => {
            if s.is_empty() {
                Vec::new()
            } else {
                let mut pts: Vec<[f64; 2]> = s
                    .corner_approx_arr()
                    .into_iter()
                    .map(|p| [p.x + ox, p.y + oy])
                    .collect();
                if pts.len() > 1 && pts[0] == *pts.last().unwrap() {
                    pts.pop();
                }
                pts
            }
        }
    }
}

/// Determines the primary layer of an item.
fn item_layer(board: &Board, id: ItemId) -> u16 {
    let Some(item) = board.store().get(id) else {
        return 0;
    };
    match &item.kind {
        ItemKind::Pin(pin) => pin.layers.first.max(0) as u16,
        ItemKind::Via(via) => via.layers.first.max(0) as u16,
        ItemKind::Trace(trace) => trace.layer.max(0) as u16,
        ItemKind::ObstacleArea(area) => area.layer.max(0) as u16,
        ItemKind::ConductionArea(area) => area.layer.max(0) as u16,
        ItemKind::BoardOutline(_) => 0,
        ItemKind::ComponentOutline(comp) => {
            if comp.is_front {
                0
            } else {
                board.layers().len().saturating_sub(1) as u16
            }
        }
    }
}

/// Builds a [`ShapeStore`] tailored for ratsnest connectivity.
///
/// `board_connectivity` only queries `view.shapes(id)` for [`ItemKind::ConductionArea`];
/// traces, vias and pins connect strictly at anchor points. Populating only conduction
/// areas avoids thousands of redundant search-tree queries.
fn connectivity_shape_store(board: &Board) -> ShapeStore {
    let mut store = ShapeStore::new();
    let tree = board.default_tree();
    let items = board.store();
    for id in items.ids() {
        let Some(item) = items.get(id) else { continue };
        if !matches!(item.kind, ItemKind::ConductionArea(_)) {
            continue;
        }
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

/// Computes the ratsnest airlines from board connectivity.
fn compute_ratsnest(board: &Board) -> Vec<RatLine> {
    let shapes = connectivity_shape_store(board);
    let view = StoreView::new(
        board.store(),
        &shapes,
        &UniformClearance(0),
        board.layers().len(),
    );
    let connectivity = board_connectivity(&view);
    let mut ratsnest = Vec::with_capacity(connectivity.incomplete_count);

    for net in &connectivity.nets {
        for airline in &net.airlines {
            let layer_from = item_layer(board, airline.from_item);
            let layer_to = item_layer(board, airline.to_item);
            ratsnest.push(RatLine {
                net: airline.net,
                from: [airline.from.x as f64, airline.from.y as f64],
                to: [airline.to.x as f64, airline.to.y as f64],
                layer_from,
                layer_to,
            });
        }
    }
    ratsnest
}
