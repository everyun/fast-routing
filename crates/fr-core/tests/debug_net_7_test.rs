//! Diagnostic test for Net 7.

use fr_autoroute::{analyze_net_connectivity, LayerSpatialGrid, MazeSearchAlgo, MazeSearchSettings};
use fr_io::parse_dsn;
use std::fs;
use std::path::Path;

#[test]
fn test_debug_net_7() {
    let home = std::env::var("HOME").unwrap_or_default();
    let file_path = Path::new(&home).join("Downloads/v3-route-free.dsn");
    if !file_path.exists() {
        return;
    }

    let content = fs::read_to_string(&file_path).expect("failed to read dsn");
    let doc = parse_dsn(&content).expect("failed to parse dsn");

    let job = fr_core::RoutingJob::new(&content);
    let board = job.build_board_from_dsn(&doc);

    let net_id = 7;
    let net = &doc.nets[net_id - 1];
    let pins = board.get_pins_for_net(net_id as i32);
    let status = analyze_net_connectivity(&board, net_id as i32);

    println!("=== Debugging Net #{} [{}] ===", net_id, net.name);
    println!("Pins: {}", pins.len());
    for (i, p) in pins.iter().enumerate() {
        println!("  Pin #{}: center=({}, {}), layers=[{}, {}], pad_box=({:?})", i, p.center.x, p.center.y, p.first_layer, p.last_layer, p.pad_bounding_box);
    }
    println!("Component Anchors: {:?}", status.component_anchors);

    let layer_count = board.layer_count as i32;
    let mut net_grids: Vec<LayerSpatialGrid> = (0..board.layer_count)
        .map(|_| LayerSpatialGrid::new(board.bounding_box, 1000))
        .collect();

    // Insert foreign pin pads
    for other_pin in &board.pins {
        if other_pin.header.net_no_arr.first() != Some(&(net_id as i32)) {
            let min_l = other_pin.first_layer.max(0) as usize;
            let max_l = other_pin.last_layer.min(layer_count - 1) as usize;
            for l in min_l..=max_l.min(net_grids.len() - 1) {
                net_grids[l].insert(other_pin.pad_bounding_box);
            }
        }
    }

    let algo = MazeSearchAlgo::new(MazeSearchSettings {
        step_size: 1500,
        max_expansion_nodes: 100_000,
        ..Default::default()
    });

    for i in 0..status.component_anchors.len() {
        for j in (i + 1)..status.component_anchors.len() {
            let (start, start_layer) = status.component_anchors[i];
            let (target, target_layer) = status.component_anchors[j];
            println!("Attempting connection {} -> {}: ({}, {}) L{} to ({}, {}) L{}", i, j, start.x, start.y, start_layer, target.x, target.y, target_layer);
            let res = algo.find_path_3d_grid(start, start_layer, target, target_layer, layer_count, 125, &net_grids);
            println!("  Path found: {:?}", res.is_some());
            if let Some(ref p) = res {
                println!("  Segments: {}, Vias: {}, Cost: {}", p.segments.len(), p.vias.len(), p.total_cost);
            }
        }
    }
}
