//! Mock loader for DSN files and sample board generator.
//!
//! Converts imported Specctra designs into the contract [`RenderModel`].

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use fr_board::item::{FixedState, ItemKind};
use fr_board::search_tree::placed_area_pieces;
use fr_board::TileShape;
use fr_core::snapshot::*;
use fr_io::specctra::ast::BoardShape;
use fr_io::specctra::ImportedDesign;

fn map_fix_state(f: FixedState) -> FixState {
    match f {
        FixedState::Unfixed => FixState::Unfixed,
        FixedState::ShoveFixed => FixState::ShoveFixed,
        FixedState::UserFixed => FixState::UserFixed,
        FixedState::SystemFixed => FixState::SystemFixed,
    }
}

/// Upstream Freerouting 2.3.0 layer colors:
/// Top layer: Red [200, 52, 52]
/// Bottom layer: Blue [77, 127, 196]
/// Inner layers: Green [127, 200, 127], Orange [206, 125, 44], Cyan [79, 203, 203], Pink [219, 98, 139]
fn layer_color(index: usize, total: usize, is_signal: bool) -> [f32; 4] {
    if total <= 1 || index == 0 {
        [200.0 / 255.0, 52.0 / 255.0, 52.0 / 255.0, 1.0]
    } else if index == total - 1 {
        [77.0 / 255.0, 127.0 / 255.0, 196.0 / 255.0, 1.0]
    } else if is_signal {
        match index % 6 {
            1 => [127.0 / 255.0, 200.0 / 255.0, 127.0 / 255.0, 1.0],
            2 => [206.0 / 255.0, 125.0 / 255.0, 44.0 / 255.0, 1.0],
            3 => [79.0 / 255.0, 203.0 / 255.0, 203.0 / 255.0, 1.0],
            4 => [219.0 / 255.0, 98.0 / 255.0, 139.0 / 255.0, 1.0],
            5 => [167.0 / 255.0, 165.0 / 255.0, 198.0 / 255.0, 1.0],
            _ => [40.0 / 255.0, 204.0 / 255.0, 217.0 / 255.0, 1.0],
        }
    } else {
        [100.0 / 255.0, 120.0 / 255.0, 100.0 / 255.0, 0.5]
    }
}

fn tile_shape_to_polygon(shape: &TileShape, offset_x: f64, offset_y: f64) -> Vec<[f64; 2]> {
    match shape {
        TileShape::Box(b) => vec![
            [b.ll.x as f64 + offset_x, b.ll.y as f64 + offset_y],
            [b.ur.x as f64 + offset_x, b.ll.y as f64 + offset_y],
            [b.ur.x as f64 + offset_x, b.ur.y as f64 + offset_y],
            [b.ll.x as f64 + offset_x, b.ur.y as f64 + offset_y],
        ],
        TileShape::Oct(o) => (0..8)
            .map(|i| {
                let c = o.corner(i);
                [c.x as f64 + offset_x, c.y as f64 + offset_y]
            })
            .collect(),
        TileShape::Simplex(s) => s
            .corner_approx_arr()
            .into_iter()
            .map(|p| [p.x + offset_x, p.y + offset_y])
            .collect(),
    }
}

fn board_shape_to_polygon(shape: &BoardShape) -> Vec<[f64; 2]> {
    match shape {
        BoardShape::Box(b) => vec![
            [b.ll.x as f64, b.ll.y as f64],
            [b.ur.x as f64, b.ll.y as f64],
            [b.ur.x as f64, b.ur.y as f64],
            [b.ll.x as f64, b.ur.y as f64],
        ],
        BoardShape::Polygon(pts) => pts.iter().map(|p| [p.x as f64, p.y as f64]).collect(),
        BoardShape::Octagon(o) => (0..8)
            .map(|i| {
                let c = o.corner(i);
                [c.x as f64, c.y as f64]
            })
            .collect(),
        BoardShape::Simplex(s) => s
            .corner_approx_arr()
            .into_iter()
            .map(|p| [p.x, p.y])
            .collect(),
        BoardShape::Circle(c) => {
            let n = 24;
            let mut pts = Vec::with_capacity(n);
            let cx = c.center.x as f64;
            let cy = c.center.y as f64;
            let r = c.radius as f64;
            for i in 0..n {
                let theta = (i as f64) * 2.0 * std::f64::consts::PI / (n as f64);
                pts.push([cx + r * theta.cos(), cy + r * theta.sin()]);
            }
            pts
        }
    }
}

/// Loads a Specctra DSN file and converts it to [`RenderModel`].
pub fn load(dsn_path: impl AsRef<Path>) -> Result<RenderModel, Box<dyn std::error::Error>> {
    let bytes = fs::read(dsn_path)?;
    let design = fr_io::specctra::import(&bytes)?;
    Ok(convert_design(design))
}

fn convert_design(design: ImportedDesign) -> RenderModel {
    let layer_count = design.board.layers().len();
    let mut layers = Vec::with_capacity(layer_count);
    for i in 0..layer_count {
        if let Some(l) = design.board.layers().get(i as i32) {
            layers.push(LayerInfo {
                name: l.name.clone(),
                is_signal: l.is_signal,
                color: layer_color(i, layer_count, l.is_signal),
            });
        }
    }

    let bbox = [
        design.bounds.ll.x as f64,
        design.bounds.ll.y as f64,
        design.bounds.ur.x as f64,
        design.bounds.ur.y as f64,
    ];

    let unit_um = design.unit.micrometres();
    let res = design.resolution as f64;
    let scale = design.scale_factor;
    let resolution = unit_um / (res * scale * 1000.0);

    let mut traces = Vec::new();
    let mut vias = Vec::new();
    let mut pads = Vec::new();
    let mut areas = Vec::new();

    // Board outline and outline holes
    for s in &design.outline_shapes {
        let poly = board_shape_to_polygon(s);
        if !poly.is_empty() {
            areas.push(AreaShape {
                layer: 0,
                net: 0,
                polygon: poly,
                kind: AreaKind::Outline,
            });
        }
    }
    for s in &design.outline_holes {
        let poly = board_shape_to_polygon(s);
        if !poly.is_empty() {
            areas.push(AreaShape {
                layer: 0,
                net: 0,
                polygon: poly,
                kind: AreaKind::Keepout,
            });
        }
    }

    // Pins by net for ratsnest construction
    let mut pins_by_net: HashMap<i32, Vec<([f64; 2], u16)>> = HashMap::new();

    let store = design.board.store();
    for (_id, item) in store.iter() {
        let net = item.nets.as_slice().first().copied().unwrap_or(0);
        let fixed = map_fix_state(item.fixed_state);

        match &item.kind {
            ItemKind::Trace(t) => {
                let pts: Vec<[f64; 2]> = t
                    .polyline
                    .corner_approx_arr()
                    .into_iter()
                    .map(|p| [p.x, p.y])
                    .collect();
                if pts.len() >= 2 {
                    traces.push(TraceShape {
                        layer: t.layer.max(0) as u16,
                        net,
                        half_width: t.half_width as f64,
                        points: pts,
                        fixed,
                    });
                }
            }
            ItemKind::Via(v) => {
                let padstack = design.board.padstacks().get(v.padstack_no);
                let radius = padstack
                    .map(|p| {
                        if p.drill_radius > 0 {
                            (p.drill_radius as f64) * 1.5
                        } else {
                            3000.0
                        }
                    })
                    .unwrap_or(3000.0);
                vias.push(ViaShape {
                    layer_from: v.layers.first.max(0) as u16,
                    layer_to: v.layers.last.max(0) as u16,
                    net,
                    center: [v.center.x as f64, v.center.y as f64],
                    radius,
                    fixed,
                });
            }
            ItemKind::Pin(p) => {
                let is_smd = p.layers.is_single();
                let padstack = design.board.padstacks().get(p.padstack_no);
                let comp_name = if item.component_no >= 0
                    && (item.component_no as usize) < design.components.len()
                {
                    design.components[item.component_no as usize].name.clone()
                } else {
                    String::new()
                };
                let pin_name = format!("{}", p.pin_no);

                let center = [p.center.x as f64, p.center.y as f64];
                if net > 0 {
                    pins_by_net
                        .entry(net)
                        .or_default()
                        .push((center, p.layers.first.max(0) as u16));
                }

                for layer in p.layers.first..=p.layers.last {
                    let poly = if let Some(shape) = padstack.and_then(|ps| ps.shape_on_layer(layer))
                    {
                        tile_shape_to_polygon(shape, p.center.x as f64, p.center.y as f64)
                    } else {
                        let r = 2500.0;
                        vec![
                            [center[0] - r, center[1] - r],
                            [center[0] + r, center[1] - r],
                            [center[0] + r, center[1] + r],
                            [center[0] - r, center[1] + r],
                        ]
                    };
                    pads.push(PadShape {
                        layer: layer.max(0) as u16,
                        net,
                        polygon: poly,
                        is_smd,
                        component: comp_name.clone(),
                        pin: pin_name.clone(),
                    });
                }
            }
            ItemKind::ObstacleArea(o) => {
                let pieces = placed_area_pieces(&o.area);
                for shape in pieces {
                    let poly = tile_shape_to_polygon(&shape, 0.0, 0.0);
                    if !poly.is_empty() {
                        areas.push(AreaShape {
                            layer: o.layer.max(0) as u16,
                            net,
                            polygon: poly,
                            kind: AreaKind::Keepout,
                        });
                    }
                }
            }
            ItemKind::ConductionArea(c) => {
                let pieces = placed_area_pieces(&c.area);
                for shape in pieces {
                    let poly = tile_shape_to_polygon(&shape, 0.0, 0.0);
                    if !poly.is_empty() {
                        areas.push(AreaShape {
                            layer: c.layer.max(0) as u16,
                            net,
                            polygon: poly,
                            kind: AreaKind::Plane,
                        });
                    }
                }
            }
            ItemKind::BoardOutline(b) => {
                for border in &b.borders {
                    let poly: Vec<[f64; 2]> = border
                        .corner_approx_arr()
                        .into_iter()
                        .map(|p| [p.x, p.y])
                        .collect();
                    if !poly.is_empty() {
                        areas.push(AreaShape {
                            layer: 0,
                            net: 0,
                            polygon: poly,
                            kind: AreaKind::Outline,
                        });
                    }
                }
            }
            ItemKind::ComponentOutline(_) => {}
        }
    }

    // Construct ratsnest (unconnected flylines)
    let mut ratsnest = Vec::new();
    for (&net, pins) in &pins_by_net {
        if pins.len() >= 2 {
            for i in 0..pins.len() - 1 {
                ratsnest.push(RatLine {
                    net,
                    from: pins[i].0,
                    to: pins[i + 1].0,
                    layer_from: pins[i].1,
                    layer_to: pins[i + 1].1,
                });
            }
        }
    }

    let total_items = (traces.len() + vias.len() + pads.len() + areas.len()) as u32;
    let stats = Stats {
        incomplete: ratsnest.len() as u32,
        violations: 0,
        pass: 0,
        elapsed_s: 0.0,
        score: (ratsnest.len() as f64) * 100.0,
        items: total_items,
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

/// Builds a realistic 4-layer sample board for fallback and quick testing.
pub fn sample_board() -> RenderModel {
    let layers = vec![
        LayerInfo {
            name: "F.Cu".into(),
            is_signal: true,
            color: [200.0 / 255.0, 52.0 / 255.0, 52.0 / 255.0, 1.0],
        },
        LayerInfo {
            name: "In1.Cu (GND)".into(),
            is_signal: false,
            color: [127.0 / 255.0, 200.0 / 255.0, 127.0 / 255.0, 0.6],
        },
        LayerInfo {
            name: "In2.Cu (VCC)".into(),
            is_signal: false,
            color: [206.0 / 255.0, 125.0 / 255.0, 44.0 / 255.0, 0.6],
        },
        LayerInfo {
            name: "B.Cu".into(),
            is_signal: true,
            color: [77.0 / 255.0, 127.0 / 255.0, 196.0 / 255.0, 1.0],
        },
    ];

    let w = 100_000.0;
    let h = 80_000.0;
    let bbox = [0.0, 0.0, w, h];
    let resolution = 0.001; // 1000 units = 1 mm

    let areas = vec![
        // Board Outline
        AreaShape {
            layer: 0,
            net: 0,
            polygon: vec![[0.0, 0.0], [w, 0.0], [w, h], [0.0, h]],
            kind: AreaKind::Outline,
        },
        // Keepout area (e.g. antenna / mounting hole clearance)
        AreaShape {
            layer: 0,
            net: 0,
            polygon: vec![
                [5_000.0, 5_000.0],
                [15_000.0, 5_000.0],
                [15_000.0, 15_000.0],
                [5_000.0, 15_000.0],
            ],
            kind: AreaKind::Keepout,
        },
    ];

    let mut pads = Vec::new();
    let mut vias = Vec::new();
    let mut traces = Vec::new();
    let mut ratsnest = Vec::new();

    // MCU QFP-32 in the center
    let mcu_cx = w / 2.0;
    let mcu_cy = h / 2.0;
    let pad_w = 800.0;
    let pad_h = 2000.0;

    let mut pin_no = 1;
    // 8 pins per side
    for i in 0..8 {
        let x = mcu_cx - 7000.0 + (i as f64) * 2000.0;
        let y_bot = mcu_cy - 9000.0;
        let y_top = mcu_cy + 9000.0;

        pads.push(PadShape {
            layer: 0,
            net: pin_no,
            polygon: vec![
                [x - pad_w, y_bot - pad_h],
                [x + pad_w, y_bot - pad_h],
                [x + pad_w, y_bot + pad_h],
                [x - pad_w, y_bot + pad_h],
            ],
            is_smd: true,
            component: "U1".into(),
            pin: format!("{}", pin_no),
        });
        pin_no += 1;

        pads.push(PadShape {
            layer: 0,
            net: pin_no,
            polygon: vec![
                [x - pad_w, y_top - pad_h],
                [x + pad_w, y_top - pad_h],
                [x + pad_w, y_top + pad_h],
                [x - pad_w, y_top + pad_h],
            ],
            is_smd: true,
            component: "U1".into(),
            pin: format!("{}", pin_no),
        });
        pin_no += 1;
    }

    // Connect some traces from MCU to vias and peripheral components
    for i in 0..6 {
        let px = mcu_cx - 5000.0 + (i as f64) * 2000.0;
        let py = mcu_cy - 9000.0;
        let vy = py - 8000.0 - (i as f64) * 1500.0;
        let vx = px - 3000.0;

        traces.push(TraceShape {
            layer: 0,
            net: i + 1,
            half_width: 300.0,
            points: vec![[px, py], [px, py - 4000.0], [vx, vy]],
            fixed: FixState::Unfixed,
        });

        vias.push(ViaShape {
            layer_from: 0,
            layer_to: 3,
            net: i + 1,
            center: [vx, vy],
            radius: 900.0,
            fixed: FixState::Unfixed,
        });

        // Bottom layer escape trace
        traces.push(TraceShape {
            layer: 3,
            net: i + 1,
            half_width: 300.0,
            points: vec![[vx, vy], [vx - 10000.0, vy], [vx - 15000.0, vy - 5000.0]],
            fixed: FixState::Unfixed,
        });
    }

    // Unrouted ratsnest lines
    for i in 6..12 {
        let px = mcu_cx - 7000.0 + (i as f64) * 2000.0;
        let py = mcu_cy + 9000.0;
        let target_x = px + 15000.0;
        let target_y = py + 12000.0;

        // Peripheral component pad
        pads.push(PadShape {
            layer: 0,
            net: i + 1,
            polygon: vec![
                [target_x - 1000.0, target_y - 1000.0],
                [target_x + 1000.0, target_y - 1000.0],
                [target_x + 1000.0, target_y + 1000.0],
                [target_x - 1000.0, target_y + 1000.0],
            ],
            is_smd: true,
            component: format!("R{}", i),
            pin: "1".into(),
        });

        ratsnest.push(RatLine {
            net: i + 1,
            from: [px, py],
            to: [target_x, target_y],
            layer_from: 0,
            layer_to: 0,
        });
    }

    let stats = Stats {
        incomplete: ratsnest.len() as u32,
        violations: 0,
        pass: 0,
        elapsed_s: 0.0,
        score: (ratsnest.len() as f64) * 100.0,
        items: (traces.len() + vias.len() + pads.len() + areas.len()) as u32,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_board_has_expected_structure() {
        let model = sample_board();
        assert_eq!(model.layers.len(), 4);
        assert_eq!(model.layers[0].name, "F.Cu");
        assert_eq!(model.layers[3].name, "B.Cu");
        assert!(!model.traces.is_empty());
        assert!(!model.vias.is_empty());
        assert!(!model.pads.is_empty());
        assert!(!model.ratsnest.is_empty());
        assert!(model.stats.items > 0);
    }

    #[test]
    fn layer_palette_matches_java_palette() {
        let f_cu = layer_color(0, 4, true);
        assert!((f_cu[0] - 200.0 / 255.0).abs() < 1e-4);
        assert!((f_cu[1] - 52.0 / 255.0).abs() < 1e-4);

        let b_cu = layer_color(3, 4, true);
        assert!((b_cu[0] - 77.0 / 255.0).abs() < 1e-4);
        assert!((b_cu[1] - 127.0 / 255.0).abs() < 1e-4);
    }
}
