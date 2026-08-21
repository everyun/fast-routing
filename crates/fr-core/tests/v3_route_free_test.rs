//! Test parsing and routing on user file v3-route-free.dsn.

use fr_autoroute::{analyze_net_connectivity, BatchAutorouter, BatchRouterSettings};
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
    println!("DSN Parsed Wires: {}", doc.wires.len());
    println!("DSN Parsed Vias: {}", doc.vias.len());

    assert_eq!(doc.components.len(), 324);
    assert_eq!(doc.packages.len(), 75);
    assert_eq!(doc.nets.len(), 489);
    assert_eq!(doc.wires.len(), 1669, "Must parse all 1669 pre-existing wires");
    assert_eq!(doc.vias.len(), 47, "Must parse all 47 pre-existing vias");

    let job = fr_core::RoutingJob::new(&content);
    let mut board = job.build_board_from_dsn(&doc);
    println!("BasicBoard Pins: {}", board.pins.len());
    println!("BasicBoard Layer Count: {}", board.layer_count);
    assert_eq!(board.pins.len(), 2217);
    assert!(board.traces.len() >= 1669, "Board must retain pre-existing wires");
    assert!(board.vias.len() >= 47, "Board must retain pre-existing vias");

    // Print initial connectivity breakdown across first 15 nets
    for net_id in 1..=15 {
        let net = &doc.nets[net_id - 1];
        let pins = board.get_pins_for_net(net_id as i32);
        let traces = board.get_traces_for_net(net_id as i32);
        let vias = board.get_vias_for_net(net_id as i32);
        let status = analyze_net_connectivity(&board, net_id as i32);
        println!(
            "Net #{:2} [{:20}]: Pins={}, Traces={}, Vias={}, DisjointComponents={}, FullyConnected={}",
            net_id, net.name, pins.len(), traces.len(), vias.len(), status.num_components, status.is_fully_connected
        );
        if !status.is_fully_connected {
            for (idx, (anchor, lyr)) in status.component_anchors.iter().enumerate() {
                println!("    Anchor #{}: ({}, {}) on Layer {}", idx, anchor.x, anchor.y, lyr);
            }
        }
    }

    let net_ids: Vec<i32> = (1..=20.min(doc.nets.len() as i32)).collect();
    let router = BatchAutorouter::new(BatchRouterSettings {
        max_passes: 1,
        ..Default::default()
    });
    let stats = router.route_board(&mut board, &net_ids);
    println!("Routed Traces: {}", board.traces.len());
    println!("Stats Completed Nets: {}", stats.completed_nets);
    println!("Stats Unrouted Nets: {}", stats.unrouted_nets);
}
