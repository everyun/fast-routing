//! Test parsing and routing on user file v3-route-free.dsn.

use fr_autoroute::{BatchAutorouter, BatchRouterSettings};
use fr_io::parse_dsn;
use std::fs;
use std::path::Path;

#[test]
fn test_v3_route_free_stats() {
    let home = std::env::var("HOME").unwrap_or_default();
    let file_path = Path::new(&home).join("Downloads/v3-route-free.dsn");
    if !file_path.exists() {
        return;
    }

    let content = fs::read_to_string(&file_path).expect("failed to read dsn");
    let doc = parse_dsn(&content).expect("failed to parse dsn");

    println!("=== v3-route-free.dsn Parsed Structure ===");
    println!("PCB Name: {}", doc.pcb_name);
    println!("Layers: {}", doc.layers.len());
    println!("Components: {}", doc.components.len());
    println!("Packages: {}", doc.packages.len());
    println!("Nets: {}", doc.nets.len());

    let mut total_pins = 0;
    for net in &doc.nets {
        total_pins += net.pins.len();
    }
    println!("Total Net Pins in DSN: {}", total_pins);

    let job = fr_core::RoutingJob::new(&content);
    let mut board = job.build_board_from_dsn(&doc);
    println!("BasicBoard Pins: {}", board.pins.len());
    println!("BasicBoard Layer Count: {}", board.layer_count);

    let net_ids: Vec<i32> = (1..=20.min(doc.nets.len() as i32)).collect();
    let router = BatchAutorouter::new(BatchRouterSettings {
        max_passes: 1,
        ..Default::default()
    });
    let stats = router.route_board(&mut board, &net_ids);
    println!("Routed Traces: {}", board.traces.len());
    println!("Stats Completed Nets: {}", stats.completed_nets);
    println!("Stats Unrouted Nets: {}", stats.unrouted_nets);
    assert_eq!(doc.components.len(), 324);
    assert_eq!(doc.packages.len(), 75);
    assert_eq!(doc.nets.len(), 489);
    assert_eq!(board.pins.len(), 2217);
}
