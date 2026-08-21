//! Headless PCB routing job pipeline (Load DSN -> Route -> DRC Check -> Export SES).

use crate::board_statistics::BoardStatistics;
use fr_autoroute::{BatchAutorouter, BatchRouterSettings};
use fr_board::{BasicBoard, Pin};
use fr_drc::DesignRulesChecker;
use fr_geometry::planar::{IntBox, IntPoint};
use fr_io::{parse_dsn, DsnDocument, DsnWire, SesWriter};

/// Result of a complete routing job execution.
#[derive(Debug, Clone)]
pub struct JobResult {
    pub pcb_name: String,
    pub statistics: BoardStatistics,
    pub ses_content: String,
    pub is_clean: bool,
}

/// Headless Routing Job runner.
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

        // 3. Collect net IDs
        let net_ids: Vec<i32> = (1..=dsn.nets.len() as i32).collect();

        // 4. Run Batch Autorouter (Multi-threaded with Rayon)
        let router = BatchAutorouter::new(self.router_settings.clone());
        let stats = router.route_board(&mut board, &net_ids);

        // 5. Run DRC Checker
        let drc = DesignRulesChecker::new(&board);
        let violations = drc.get_all_clearance_violations(250.0); // 250um default clearance

        // 6. Compute objective score
        let board_stats =
            BoardStatistics::compute(&board, stats.unrouted_nets, violations.len());
        let is_clean = board_stats.unrouted_net_count == 0 && board_stats.clearance_violation_count == 0;

        // 7. Generate Specctra .ses session output
        let writer = SesWriter::new(&dsn.pcb_name);
        let mut ses_wires = Vec::new();
        for trace in &board.traces {
            let net_name = dsn.nets.get((trace.header.net_no_arr[0] - 1) as usize)
                .map(|n| n.name.clone())
                .unwrap_or_else(|| "GND".to_string());
            let layer_name = dsn.layers.get(trace.layer as usize)
                .map(|l| l.name.clone())
                .unwrap_or_else(|| "F.Cu".to_string());

            let points: Vec<fr_io::DsnPoint> = trace
                .corner_points
                .iter()
                .map(|p| fr_io::DsnPoint {
                    x: p.x as f64,
                    y: p.y as f64,
                })
                .collect();

            ses_wires.push(DsnWire {
                net_name,
                layer: layer_name,
                width: (trace.half_width * 2) as f64,
                points,
            });
        }

        let net_names: Vec<String> = dsn.nets.iter().map(|n| n.name.clone()).collect();
        let ses_content = writer.write_session(&ses_wires, &[], &net_names);

        Ok(JobResult {
            pcb_name: dsn.pcb_name,
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

        // Convert components and pins
        let mut item_id = 1;
        for (net_idx, net) in dsn.nets.iter().enumerate() {
            let net_no = (net_idx + 1) as i32;
            for (pin_idx, net_pin) in net.pins.iter().enumerate() {
                let comp = dsn.components.iter().find(|c| c.name == net_pin.component_name);
                let (cx, cy) = if let Some(c) = comp {
                    let pkg = dsn.packages.iter().find(|p| p.name == c.package_name);
                    let pin_offset = pkg.and_then(|p| p.pins.iter().find(|pin| pin.pin_id == net_pin.pin_id));
                    if let Some(p_off) = pin_offset {
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

                let pin_center = IntPoint::new(cx, cy);
                let pad_box = IntBox::new(cx - 500, cy - 500, cx + 500, cy + 500);

                board.pins.push(Pin::new(
                    item_id,
                    net_no,
                    4, // Pin clearance class
                    1,
                    (pin_idx + 1) as i32,
                    pin_center,
                    pad_box,
                    0,
                    (layer_count - 1) as i32,
                    400,
                ));
                item_id += 1;
            }
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
                    (pins "U1-4" "U1-8")
                )
            )
        )
        "#;

        let job = RoutingJob::new(sample);
        let result = job.execute().unwrap();
        assert_eq!(result.pcb_name, "test_routing_job");
        assert!(result.ses_content.contains("(session \"test_routing_job\""));
    }
}
