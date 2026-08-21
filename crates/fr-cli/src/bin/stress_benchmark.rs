//! Extreme Multi-Scale Stress Benchmark for Rust Fast-Routing Engine.
//!
//! Generates and routes industrial-scale super-dense multi-chip stress designs:
//! 1. Tier-1: 4x Quad-BGA Matrix (5,000 Pins, 1,200 Nets, 8 Layers)
//! 2. Tier-2: 8x Octa-FPGA Array (10,000 Pins, 2,500 Nets, 16 Layers)
//! 3. Tier-3: Extreme Server Multi-SoC Board (20,000 Pins, 5,000 Nets, 32 Layers)

use fr_autoroute::{BatchAutorouter, BatchRouterSettings};
use fr_board::{BasicBoard, Pin};
use fr_geometry::planar::{IntBox, IntPoint};
use std::time::Instant;

fn generate_stress_board(
    name: &str,
    bga_count: usize,
    bga_grid_dim: usize, // e.g. 34 for ~1156 balls per BGA
    layer_count: usize,
) -> (BasicBoard, Vec<i32>) {
    let board_size = 200_000; // 200mm in 1um
    let bounding_box = IntBox::new(0, 0, board_size, board_size);
    let mut board = BasicBoard::new(name, layer_count, bounding_box);

    let mut pin_id = 1;
    let mut net_id = 1;
    let pitch = 800; // 0.8mm pitch (800 um)
    let pad_radius = 200; // 200 um radius

    let cols = (bga_count as f64).sqrt().ceil() as usize;

    for bga_idx in 0..bga_count {
        let bga_col = bga_idx % cols;
        let bga_row = bga_idx / cols;

        let bga_origin_x = 25_000 + (bga_col as i32) * (board_size / (cols as i32 + 1));
        let bga_origin_y = 25_000 + (bga_row as i32) * (board_size / (cols as i32 + 1));

        let mut bga_pins = Vec::new();

        for r in 0..bga_grid_dim {
            for c in 0..bga_grid_dim {
                let px = bga_origin_x + (c as i32) * pitch;
                let py = bga_origin_y + (r as i32) * pitch;

                let center = IntPoint::new(px, py);
                let pad_box = IntBox::new(
                    px - pad_radius,
                    py - pad_radius,
                    px + pad_radius,
                    py + pad_radius,
                );

                bga_pins.push((center, pad_box));
            }
        }

        // Pair up pins into 2-pin / 4-pin differential net connections
        for chunk in bga_pins.chunks(2) {
            if chunk.len() == 2 {
                for (p_idx, &(center, pad_box)) in chunk.iter().enumerate() {
                    board.pins.push(Pin::new(
                        pin_id,
                        net_id,
                        4,
                        1,
                        (p_idx + 1) as i32,
                        center,
                        pad_box,
                        0,
                        (layer_count - 1) as i32,
                        pad_radius,
                    ));
                    pin_id += 1;
                }
                net_id += 1;
            }
        }
    }

    let net_ids: Vec<i32> = (1..net_id).collect();
    (board, net_ids)
}

fn main() {
    println!("\n==========================================================================================================");
    println!("             ULTRA-SCALE PCB STRESS BENCHMARK (MULTI-CHIP MEGA BGA MATRICES)                             ");
    println!("==========================================================================================================");
    println!(
        "{:<38} | {:<8} | {:<8} | {:<8} | {:<14} | {:<12}",
        "Stress Test Level / Architecture", "Pins", "Nets", "Layers", "Route Time", "Trace Segs"
    );
    println!("{:-<38}-|-{:-<8}-|-{:-<8}-|-{:-<8}-|-{:-<14}-|-{:-<12}", "", "", "", "", "", "");

    let test_cases = [
        ("Tier-1: Quad-BGA (4x SoC Arrays)", 4, 34, 8),
        ("Tier-2: Octa-FPGA (8x Ultra-Density)", 8, 36, 16),
        ("Tier-3: Extreme Server MCM Array", 16, 36, 32),
    ];

    let router = BatchAutorouter::new(BatchRouterSettings {
        max_passes: 1,
        ..Default::default()
    });

    for (desc, bga_count, grid_dim, layers) in test_cases {
        let (mut board, net_ids) = generate_stress_board(desc, bga_count, grid_dim, layers);
        let pin_count = board.pins.len();
        let net_count = net_ids.len();

        let t0 = Instant::now();
        let _stats = router.route_board(&mut board, &net_ids);
        let duration = t0.elapsed();

        let dur_str = if duration.as_secs_f64() >= 1.0 {
            format!("{:.2} s", duration.as_secs_f64())
        } else {
            format!("{:.1} ms", duration.as_secs_f64() * 1000.0)
        };

        println!(
            "{:<38} | {:<8} | {:<8} | {:<8} | {:<14} | {:<12}",
            desc, pin_count, net_count, layers, dur_str, board.traces.len()
        );
    }
    println!("==========================================================================================================\n");
}
