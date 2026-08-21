//! End-to-end routing job orchestrator and Specctra pipeline manager.

use crate::board_statistics::BoardStatistics;
use fr_autoroute::batch_autorouter::NetRoutingRule;
use fr_autoroute::{BatchAutorouter, BatchRouterSettings};
use fr_board::{BasicBoard, FixedState, Pin, PolylineTrace, Via};
use fr_drc::DesignRulesChecker;
use fr_geometry::planar::{IntBox, IntPoint};
use fr_io::{parse_dsn, DsnDocument, DsnPadstack, DsnPadstackShape, DsnPoint, DsnVia, DsnWire, SesWriter};
use std::collections::HashMap;

/// Result of a complete routing pipeline execution.
#[derive(Debug, Clone)]
pub struct JobResult {
    pub pcb_name: String,
    pub statistics: BoardStatistics,
    pub ses_content: String,
    pub is_clean: bool,
}

/// Headless PCB routing job orchestrator.
pub struct RoutingJob {
    pub dsn_content: String,
    pub router_settings: BatchRouterSettings,
}

impl RoutingJob {
    pub fn new(dsn_content: &str) -> Self {
        RoutingJob {
            dsn_content: dsn_content.to_string(),
            router_settings: BatchRouterSettings::default(),
        }
    }

    /// Executes the entire end-to-end routing pipeline.
    pub fn execute(&self) -> Result<JobResult, String> {
        // 1. Parse DSN
        let dsn = parse_dsn(&self.dsn_content)?;

        // 2. Build BasicBoard
        let mut board = self.build_board_from_dsn(&dsn);

        // 3. Resolve best via padstack matching the board stackup
        let layer_count = dsn.layers.len().max(2);
        let default_via_padstack = dsn.classes
            .iter()
            .find_map(|c| c.via_rule.clone())
            .or_else(|| {
                dsn.padstacks
                    .iter()
                    .find(|p| p.name.to_ascii_lowercase().contains("via"))
                    .map(|p| p.name.clone())
            })
            .unwrap_or_else(|| format!("Via[0-{}]_600:300_um", layer_count - 1));

        let mut router_settings = self.router_settings.clone();
        if router_settings.default_rule.via_padstack_name == "Via[0-1]_600:300_um" || router_settings.default_rule.via_padstack_name.is_empty() {
            router_settings.default_rule.via_padstack_name = default_via_padstack.clone();
        }

        // Build per-net NetClass routing rules
        let mut net_rules = HashMap::new();
        for (net_idx, net) in dsn.nets.iter().enumerate() {
            let net_id = (net_idx + 1) as i32;
            let matching_class = dsn.classes.iter().find(|c| c.net_names.iter().any(|name| name == &net.name));

            let width = matching_class.and_then(|c| c.width).unwrap_or(250.0);
            let half_w = (width * dsn.resolution / 2.0).round().max(50.0) as i32;

            let via_name = matching_class
                .and_then(|c| c.via_rule.clone())
                .unwrap_or_else(|| default_via_padstack.clone());

            net_rules.insert(
                net_id,
                NetRoutingRule {
                    trace_half_width: half_w,
                    clearance_class: 2,
                    via_padstack_name: via_name,
                    via_pad_radius: 300,
                    via_drill_radius: 150,
                },
            );
        }
        router_settings.net_rules = net_rules;

        // 4. Collect net IDs
        let net_ids: Vec<i32> = (1..=dsn.nets.len() as i32).collect();

        // 5. Run Batch Autorouter (Multi-threaded with Rayon)
        let router = BatchAutorouter::new(router_settings.clone());
        let stats = router.route_board(&mut board, &net_ids);

        // 6. Run DRC Checker
        let drc = DesignRulesChecker::new(&board);
        let violations = drc.get_all_clearance_violations(250.0);

        // 7. Compute objective score
        let board_stats = BoardStatistics::compute(
            &board,
            stats.total_nets,
            stats.completed_nets,
            stats.unrouted_nets,
            violations.len(),
        );
        let is_clean = board_stats.unrouted_net_count == 0 && board_stats.clearance_violation_count == 0;

        // 8. Generate Specctra .ses session output with contact endpoint snapping
        let writer = SesWriter::with_resolution(&dsn.pcb_name, &dsn.unit, dsn.resolution);
        let mut ses_wires = Vec::new();
        for trace in &board.traces {
            let net_idx = (trace.header.net_no_arr.first().copied().unwrap_or(1) - 1) as usize;
            let net_name = dsn.nets.get(net_idx)
                .map(|n| n.name.clone())
                .unwrap_or_else(|| "GND".to_string());
            let layer_name = dsn.layers.get(trace.layer as usize)
                .map(|l| l.name.clone())
                .unwrap_or_else(|| "F.Cu".to_string());

            let mut corner_points = trace.corner_points.clone();

            // Snap start corner to contacted Pin center
            if let Some(first) = corner_points.first_mut() {
                for pin in &board.pins {
                    if pin.header.net_no_arr.contains(&((net_idx + 1) as i32)) {
                        if trace.layer >= pin.first_layer && trace.layer <= pin.last_layer {
                            let threshold = pin.pad_bounding_box.width().max(pin.pad_bounding_box.height()) / 2 + trace.half_width + 50;
                            if first.is_contained_in(&pin.pad_bounding_box) || (first.x - pin.center.x).abs().max((first.y - pin.center.y).abs()) <= threshold {
                                *first = pin.center;
                                break;
                            }
                        }
                    }
                }
            }

            // Snap end corner to contacted Pin center
            if let Some(last) = corner_points.last_mut() {
                for pin in &board.pins {
                    if pin.header.net_no_arr.contains(&((net_idx + 1) as i32)) {
                        if trace.layer >= pin.first_layer && trace.layer <= pin.last_layer {
                            let threshold = pin.pad_bounding_box.width().max(pin.pad_bounding_box.height()) / 2 + trace.half_width + 50;
                            if last.is_contained_in(&pin.pad_bounding_box) || (last.x - pin.center.x).abs().max((last.y - pin.center.y).abs()) <= threshold {
                                *last = pin.center;
                                break;
                            }
                        }
                    }
                }
            }

            let points: Vec<DsnPoint> = corner_points
                .iter()
                .map(|p| DsnPoint {
                    x: p.x as f64 / dsn.resolution,
                    y: p.y as f64 / dsn.resolution,
                })
                .collect();

            let fixed_type = match trace.header.fixed_state {
                FixedState::UserFixed => Some("protect".to_string()),
                FixedState::SystemFixed => Some("fix".to_string()),
                _ => None,
            };

            ses_wires.push(DsnWire {
                net_name,
                layer: layer_name,
                width: (trace.half_width * 2) as f64 / dsn.resolution,
                points,
                fixed_type,
            });
        }

        let mut ses_vias = Vec::new();
        for via in &board.vias {
            let net_idx = (via.header.net_no_arr.first().copied().unwrap_or(1) - 1) as usize;
            let net_name = dsn.nets.get(net_idx)
                .map(|n| n.name.clone())
                .unwrap_or_else(|| "GND".to_string());

            let fixed_type = match via.header.fixed_state {
                FixedState::UserFixed => Some("protect".to_string()),
                FixedState::SystemFixed => Some("fix".to_string()),
                _ => None,
            };

            ses_vias.push(DsnVia {
                net_name,
                padstack_name: via.padstack_name.clone(),
                x: via.center.x as f64 / dsn.resolution,
                y: via.center.y as f64 / dsn.resolution,
                fixed_type,
            });
        }

        // Ensure all referenced via padstacks exist in padstack definitions
        let mut padstacks = dsn.padstacks.clone();
        for via in &ses_vias {
            if !padstacks.iter().any(|p| p.name == via.padstack_name) {
                let shapes: Vec<DsnPadstackShape> = dsn.layers.iter().map(|lyr| {
                    DsnPadstackShape {
                        layer: lyr.name.clone(),
                        shape_type: "circle".to_string(),
                        dimensions: vec![600.0],
                        points: Vec::new(),
                    }
                }).collect();
                padstacks.push(DsnPadstack {
                    name: via.padstack_name.clone(),
                    shapes,
                });
            }
        }

        let net_names: Vec<String> = dsn.nets.iter().map(|n| n.name.clone()).collect();
        let ses_content = writer.write_full_session(
            &dsn.components,
            &dsn.packages,
            &padstacks,
            &ses_wires,
            &ses_vias,
            &net_names,
        );

        Ok(JobResult {
            pcb_name: dsn.pcb_name.clone(),
            statistics: board_stats,
            ses_content,
            is_clean,
        })
    }

    pub fn build_board_from_dsn(&self, dsn: &DsnDocument) -> BasicBoard {
        let mut min_x = 0;
        let mut min_y = 0;
        let mut max_x = 100_000;
        let mut max_y = 100_000;

        for pt in &dsn.boundary_points {
            let px = (pt.x * dsn.resolution) as i32;
            let py = (pt.y * dsn.resolution) as i32;
            min_x = min_x.min(px);
            min_y = min_y.min(py);
            max_x = max_x.max(px);
            max_y = max_y.max(py);
        }

        let bounding_box = IntBox::new(min_x, min_y, max_x, max_y);
        let layer_count = dsn.layers.len().max(2);
        let mut board = BasicBoard::new(&dsn.pcb_name, layer_count, bounding_box);

        // 1. Convert components, padstacks, and pins with exact layer spans
        let mut item_id = 1;
        for (net_idx, net) in dsn.nets.iter().enumerate() {
            let net_no = (net_idx + 1) as i32;
            for (pin_idx, net_pin) in net.pins.iter().enumerate() {
                let comp = dsn.components.iter().find(|c| c.name == net_pin.component_name);
                let mut padstack_name = "default".to_string();
                let mut half_w = 400;
                let mut half_h = 400;
                let mut is_smd = true;

                let (cx, cy) = if let Some(c) = comp {
                    let pkg = dsn.packages.iter().find(|p| p.name == c.package_name);
                    let pin_offset = pkg.and_then(|p| p.pins.iter().find(|pin| pin.pin_id == net_pin.pin_id));
                    if let Some(p_off) = pin_offset {
                        padstack_name = p_off.padstack_name.clone();
                        let rot_rad = c.rotation.to_radians();
                        let rx = p_off.x * rot_rad.cos() - p_off.y * rot_rad.sin();
                        let ry = p_off.x * rot_rad.sin() + p_off.y * rot_rad.cos();
                        (
                            ((c.x + rx) * dsn.resolution) as i32,
                            ((c.y + ry) * dsn.resolution) as i32,
                        )
                    } else {
                        ((c.x * dsn.resolution) as i32 + (pin_idx as i32) * 2540, (c.y * dsn.resolution) as i32)
                    }
                } else {
                    ((pin_idx as i32) * 5000, 5000)
                };

                // Analyze padstack dimensions and layer span
                if let Some(ps) = dsn.padstacks.iter().find(|p| p.name == padstack_name) {
                    if ps.shapes.len() > 1 || ps.name.contains("[A]") || ps.name.to_ascii_lowercase().contains("thru") {
                        is_smd = false;
                    }
                    if let Some(first_shape) = ps.shapes.first() {
                        if first_shape.shape_type == "circle" && !first_shape.dimensions.is_empty() {
                            let r = (first_shape.dimensions[0] * dsn.resolution / 2.0) as i32;
                            half_w = r;
                            half_h = r;
                        } else if !first_shape.dimensions.is_empty() {
                            half_w = (first_shape.dimensions[0] * dsn.resolution / 2.0) as i32;
                            half_h = if first_shape.dimensions.len() > 1 {
                                (first_shape.dimensions[1] * dsn.resolution / 2.0) as i32
                            } else {
                                half_w
                            };
                        }
                    }
                }

                let is_back = comp.map(|c| c.side == "back").unwrap_or(false);
                let (first_layer, last_layer) = if is_smd {
                    if is_back {
                        ((layer_count - 1) as i32, (layer_count - 1) as i32)
                    } else {
                        (0, 0)
                    }
                } else {
                    (0, (layer_count - 1) as i32)
                };

                let pin_center = IntPoint::new(cx, cy);
                let pad_box = IntBox::new(cx - half_w, cy - half_h, cx + half_w, cy + half_h);

                board.pins.push(Pin::new(
                    item_id,
                    net_no,
                    4, // Pin clearance class
                    1,
                    (pin_idx + 1) as i32,
                    pin_center,
                    pad_box,
                    first_layer,
                    last_layer,
                    half_w.min(half_h),
                ));
                item_id += 1;
            }
        }

        // 2. Import pre-existing wires and traces with exact FixedState
        for wire in &dsn.wires {
            let net_idx = dsn.nets.iter().position(|n| n.name == wire.net_name).unwrap_or(0);
            let net_no = (net_idx + 1) as i32;
            let layer_idx = dsn.layers.iter().position(|l| l.name == wire.layer).unwrap_or(0) as i32;
            let points: Vec<IntPoint> = wire
                .points
                .iter()
                .map(|p| IntPoint::new((p.x * dsn.resolution) as i32, (p.y * dsn.resolution) as i32))
                .collect();

            if points.len() >= 2 {
                let mut trace = PolylineTrace::new(
                    item_id,
                    net_no,
                    2,
                    layer_idx,
                    (wire.width * dsn.resolution / 2.0) as i32,
                    points,
                );
                trace.header.fixed_state = match wire.fixed_type.as_deref() {
                    Some("protect") => FixedState::UserFixed,
                    Some("fix") => FixedState::SystemFixed,
                    _ => FixedState::Unfixed,
                };
                board.insert_trace(trace);
                item_id += 1;
            }
        }

        // 3. Import pre-existing vias with exact FixedState
        for via in &dsn.vias {
            let net_idx = dsn.nets.iter().position(|n| n.name == via.net_name).unwrap_or(0);
            let net_no = (net_idx + 1) as i32;
            let center = IntPoint::new((via.x * dsn.resolution) as i32, (via.y * dsn.resolution) as i32);
            let mut via_item = Via::new(
                item_id,
                net_no,
                2,
                center,
                &via.padstack_name,
                0,
                (layer_count - 1) as i32,
                300,
                150,
            );
            via_item.header.fixed_state = match via.fixed_type.as_deref() {
                Some("protect") => FixedState::UserFixed,
                Some("fix") => FixedState::SystemFixed,
                _ => FixedState::Unfixed,
            };
            board.insert_via(via_item);
            item_id += 1;
        }

        board
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_end_to_end_job_execution() {
        let sample = r#"
        (pcb "test_routing_job"
            (parser
                (unit mm)
                (resolution mm 1000)
            )
            (structure
                (layer F.Cu (type signal))
                (layer B.Cu (type signal))
                (boundary
                    (path pcb 0 0 0 100 0 100 100 0 100 0 0)
                )
            )
            (placement
                (component "DIP8"
                    (place "U1" 10.0 20.0 front 0.0)
                )
            )
            (network
                (net "GND"
                    (pins "U1-1" "U1-2")
                )
            )
        )
        "#;

        let job = RoutingJob::new(sample);
        let result = job.execute().unwrap();
        assert_eq!(result.pcb_name, "test_routing_job");
        assert!(result.ses_content.contains("(session \"test_routing_job.ses\""));
        assert!(result.ses_content.contains("(base_design \"test_routing_job.dsn\")"));
        assert!(result.ses_content.contains("(placement"));
        assert!(result.ses_content.contains("(was_is"));
        assert!(result.ses_content.contains("(routes"));
    }
}
