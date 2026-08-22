//! Full board routing completion convergence test on v3-route-free.dsn.

use fr_autoroute::{analyze_net_connectivity, BatchAutorouter, BatchRouterSettings, MazeSearchSettings};
use fr_io::parse_dsn;
use std::fs;
use std::path::Path;

#[test]
fn test_converge_all_489_nets() {
    let home = std::env::var("HOME").unwrap_or_default();
    let file_path = Path::new(&home).join("Downloads/v3-route-free.dsn");
    if !file_path.exists() {
        return;
    }

    let content = fs::read_to_string(&file_path).expect("failed to read dsn");
    let doc = parse_dsn(&content).expect("failed to parse dsn");

    let job = fr_core::RoutingJob::new(&content);
    let mut board = job.build_board_from_dsn(&doc);

    let net_ids: Vec<i32> = (1..=doc.nets.len() as i32).collect();
    let router = BatchAutorouter::new(BatchRouterSettings {
        max_passes: 5,
        maze_settings: MazeSearchSettings {
            step_size: (150.0 * doc.resolution).round() as i32,
            max_expansion_nodes: 100_000,
            bend_cost: 40.0,
            layer_change_cost: 150.0,
        },
        ..Default::default()
    });

    println!("\n=== Running Deep Autorouting Convergence on all 489 Nets ===");
    let stats = router.route_board(&mut board, &net_ids);
    println!("Final Result: Completed={}/{}, Vias={}, Length={:.1}mm", stats.completed_nets, stats.total_nets, stats.total_vias, stats.total_trace_length * 0.001);

    let mut unrouted_list = Vec::new();
    for &net_id in &net_ids {
        let status = analyze_net_connectivity(&board, net_id);
        if !status.is_fully_connected && board.get_pins_for_net(net_id).len() > 1 {
            unrouted_list.push((net_id, &doc.nets[net_id as usize - 1].name, status.num_components, board.get_pins_for_net(net_id).len()));
        }
    }
    println!("Remaining Unrouted Multi-Pin Nets Count: {}", unrouted_list.len());
    for (id, name, comps, pins) in unrouted_list.iter().take(20) {
        println!("  Unrouted Net #{:3} [{}]: pins={}, disjoint_clusters={}", id, name, pins, comps);
    }
}
