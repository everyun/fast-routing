//! Test routing signal nets 7 to 30 on v3-route-free.dsn.

use fr_autoroute::{analyze_net_connectivity, BatchAutorouter, BatchRouterSettings, MazeSearchSettings};
use fr_io::parse_dsn;
use std::fs;
use std::path::Path;

#[test]
fn test_route_signal_nets() {
    let home = std::env::var("HOME").unwrap_or_default();
    let file_path = Path::new(&home).join("Downloads/v3-route-free.dsn");
    if !file_path.exists() {
        return;
    }

    let content = fs::read_to_string(&file_path).expect("failed to read dsn");
    let doc = parse_dsn(&content).expect("failed to parse dsn");

    let job = fr_core::RoutingJob::new(&content);
    let mut board = job.build_board_from_dsn(&doc);

    // Route signal nets (nets 7 to 30)
    let net_ids: Vec<i32> = (7..=30).collect();
    let router = BatchAutorouter::new(BatchRouterSettings {
        max_passes: 3,
        maze_settings: MazeSearchSettings {
            step_size: (150.0 * doc.resolution).round() as i32, // 1500 units for resolution 10
            max_expansion_nodes: 100_000,
            ..Default::default()
        },
        ..Default::default()
    });

    println!("\n=== Routing Signal Nets 7 to 30 ===");
    let stats = router.route_board(&mut board, &net_ids);

    println!("\n=== Routing Results for Nets 7 to 30 ===");
    for &net_id in &net_ids {
        let after = analyze_net_connectivity(&board, net_id);
        println!("After Net #{:2} [{}]: DisjointComponents={}, FullyConnected={}", net_id, doc.nets[net_id as usize - 1].name, after.num_components, after.is_fully_connected);
    }
    println!("\n>>> Completed: {}/{} Nets, Generated Vias: {} <<<", stats.completed_nets, stats.total_nets, stats.total_vias);
    assert!(stats.completed_nets >= 5, "Must complete routed signal nets");
}
