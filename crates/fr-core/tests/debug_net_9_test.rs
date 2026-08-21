//! Debug Net 9 routing in detail.

use fr_autoroute::{analyze_net_connectivity, BatchAutorouter, BatchRouterSettings, MazeSearchSettings};
use fr_io::parse_dsn;
use std::fs;
use std::path::Path;

#[test]
fn test_debug_net_9() {
    let home = std::env::var("HOME").unwrap_or_default();
    let file_path = Path::new(&home).join("Downloads/v3-route-free.dsn");
    if !file_path.exists() {
        return;
    }

    let content = fs::read_to_string(&file_path).expect("failed to read dsn");
    let doc = parse_dsn(&content).expect("failed to parse dsn");

    let job = fr_core::RoutingJob::new(&content);
    let mut board = job.build_board_from_dsn(&doc);

    let net_id = 9;
    let router = BatchAutorouter::new(BatchRouterSettings {
        max_passes: 1,
        maze_settings: MazeSearchSettings {
            step_size: (150.0 * doc.resolution).round() as i32,
            max_expansion_nodes: 100_000,
            ..Default::default()
        },
        ..Default::default()
    });

    println!("\n=== Routing Net 9 ===");
    router.route_board(&mut board, &[net_id]);

    let traces = board.get_traces_for_net(net_id);
    let vias = board.get_vias_for_net(net_id);
    let pins = board.get_pins_for_net(net_id);

    println!("Traces: {}", traces.len());
    for (i, t) in traces.iter().enumerate() {
        println!("  Trace #{}: layer={}, half_w={}, corners={:?}", i, t.layer, t.half_width, t.corner_points);
    }
    println!("Vias: {}", vias.len());
    for (i, v) in vias.iter().enumerate() {
        println!("  Via #{}: center=({}, {}), layers=[{}, {}]", i, v.center.x, v.center.y, v.first_layer, v.last_layer);
    }
    println!("Pins: {}", pins.len());
    for (i, p) in pins.iter().enumerate() {
        println!("  Pin #{}: center=({}, {}), layers=[{}, {}], pad_box={:?}", i, p.center.x, p.center.y, p.first_layer, p.last_layer, p.pad_bounding_box);
    }

    let status = analyze_net_connectivity(&board, net_id);
    println!("Status: num_components={}, is_fully_connected={}", status.num_components, status.is_fully_connected);
}
