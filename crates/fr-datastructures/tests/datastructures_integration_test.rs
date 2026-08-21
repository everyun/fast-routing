//! Comprehensive integration tests for fr-datastructures modules:
//! - ShapeTree / MinAreaTree
//! - PlanarDelaunayTriangulation
//! - UndoableObjects
//! - IdentifierType, IndentFileWriter, ArrayStack, Stoppable, TimeLimit, IdGenerator

use fr_datastructures::{
    ArrayStack, AtomicIdGenerator, AtomicStoppable, BoundingBox2D, DelaunayEdge, IdGenerator,
    IdentifierType, IndentFileWriter, MinAreaTree, PlanarDelaunayTriangulation, Point2D,
    SequentialIdGenerator, Stoppable, TimeLimit, UndoableObjects,
};

#[test]
fn test_min_area_tree_spatial_indexing_and_queries() {
    let mut tree = MinAreaTree::new();

    // Insert 100 non-overlapping 10x10 tiles in a 10x10 grid
    let mut handles = Vec::new();
    for i in 0..10 {
        for j in 0..10 {
            let x = i * 20;
            let y = j * 20;
            let bbox = BoundingBox2D::new(x, y, x + 10, y + 10);
            let handle = tree.insert(bbox, (i, j), 0);
            handles.push((handle, i, j));
        }
    }

    assert_eq!(tree.len(), 100);

    // Query covering the center 2x2 tiles: x in [20, 50], y in [20, 50]
    // Expects (1,1), (1,2), (2,1), (2,2)
    let query_box = BoundingBox2D::new(20, 20, 50, 50);
    let hits = tree.overlaps(&query_box);
    let matched_coords: Vec<(i32, i32)> = hits.iter().map(|(_, _, &(i, j), _)| (i, j)).collect();

    assert!(matched_coords.contains(&(1, 1)));
    assert!(matched_coords.contains(&(1, 2)));
    assert!(matched_coords.contains(&(2, 1)));
    assert!(matched_coords.contains(&(2, 2)));

    // Query non-overlapping area: (-100, -100) to (-50, -50)
    let empty_query = BoundingBox2D::new(-100, -100, -50, -50);
    let empty_hits = tree.overlaps(&empty_query);
    assert!(empty_hits.is_empty());

    // Remove 50 elements and verify tree size and queries
    for (handle, i, j) in handles.iter().take(50) {
        let removed = tree.remove(*handle);
        assert_eq!(removed, Some((*i, *j)));
    }
    assert_eq!(tree.len(), 50);

    // Iteration check
    let remaining_count = tree.iter_leaves().count();
    assert_eq!(remaining_count, 50);

    // Clear the rest
    tree.clear();
    assert!(tree.is_empty());
    assert_eq!(tree.len(), 0);
}

#[test]
fn test_delaunay_triangulation_and_mst() {
    // 9 points in a 3x3 grid
    let mut points = Vec::new();
    for i in 0..3 {
        for j in 0..3 {
            points.push((Point2D::new(i as f64 * 10.0, j as f64 * 10.0), (i, j)));
        }
    }

    let triangulation = PlanarDelaunayTriangulation::new(points);
    let edges = triangulation.get_edge_lines();
    assert!(!edges.is_empty());

    // MST of 9 points must have exactly 8 edges
    let mst = triangulation.minimum_spanning_tree();
    assert_eq!(mst.len(), 8);

    // For a regular 3x3 grid of step 10.0, standard MST weight is 8 * 10.0 = 80.0
    let total_mst_length: f64 = mst.iter().map(|e: &DelaunayEdge<(i32, i32)>| e.distance).sum();
    assert!((total_mst_length - 80.0).abs() < 1e-4);
}

#[test]
fn test_delaunay_collinear_or_few_points() {
    // Empty
    let empty_dt: PlanarDelaunayTriangulation<i32> = PlanarDelaunayTriangulation::new(vec![]);
    assert!(empty_dt.get_edge_lines().is_empty());
    assert!(empty_dt.minimum_spanning_tree().is_empty());

    // 1 point
    let single_dt = PlanarDelaunayTriangulation::new(vec![(Point2D::new(1.0, 2.0), 42)]);
    assert!(single_dt.get_edge_lines().is_empty());
    assert!(single_dt.minimum_spanning_tree().is_empty());

    // 2 points
    let two_dt = PlanarDelaunayTriangulation::new(vec![
        (Point2D::new(0.0, 0.0), 1),
        (Point2D::new(3.0, 4.0), 2),
    ]);
    assert_eq!(two_dt.get_edge_lines().len(), 1);
    let mst = two_dt.minimum_spanning_tree();
    assert_eq!(mst.len(), 1);
    assert!((mst[0].distance - 5.0).abs() < 1e-4);
}

#[test]
fn test_undoable_objects_nested_transactions() {
    let mut db: UndoableObjects<String, String> = UndoableObjects::new();

    // Base Level 0
    db.insert("net_gnd".into(), "v1".into());
    db.insert("net_vcc".into(), "v1".into());

    // Level 1: Modify net_gnd, insert net_clk
    db.generate_snapshot();
    assert_eq!(db.stack_level(), 1);

    db.save_for_undo(&"net_gnd".into());
    if let Some(val) = db.get_mut(&"net_gnd".into()) {
        *val = "v2".into();
    }
    db.insert("net_clk".into(), "v1".into());

    // Level 2: Delete net_vcc, modify net_clk
    db.generate_snapshot();
    assert_eq!(db.stack_level(), 2);

    assert!(db.delete(&"net_vcc".into()));
    db.save_for_undo(&"net_clk".into());
    if let Some(val) = db.get_mut(&"net_clk".into()) {
        *val = "v2".into();
    }

    assert_eq!(db.get(&"net_gnd".into()), Some(&"v2".into()));
    assert_eq!(db.get(&"net_vcc".into()), None);
    assert_eq!(db.get(&"net_clk".into()), Some(&"v2".into()));

    // Undo Level 2 -> back to Level 1
    let mut cancelled = Vec::new();
    let mut restored = Vec::new();
    assert!(db.undo(&mut cancelled, &mut restored));
    assert_eq!(db.stack_level(), 1);

    assert_eq!(db.get(&"net_gnd".into()), Some(&"v2".into()));
    assert_eq!(db.get(&"net_vcc".into()), Some(&"v1".into()));
    assert_eq!(db.get(&"net_clk".into()), Some(&"v1".into()));

    // Undo Level 1 -> back to Level 0
    cancelled.clear();
    restored.clear();
    assert!(db.undo(&mut cancelled, &mut restored));
    assert_eq!(db.stack_level(), 0);

    assert_eq!(db.get(&"net_gnd".into()), Some(&"v1".into()));
    assert_eq!(db.get(&"net_vcc".into()), Some(&"v1".into()));
    assert_eq!(db.get(&"net_clk".into()), None);

    // Redo Level 0 -> Level 1
    cancelled.clear();
    restored.clear();
    assert!(db.redo(&mut cancelled, &mut restored));
    assert_eq!(db.stack_level(), 1);
    assert_eq!(db.get(&"net_gnd".into()), Some(&"v2".into()));
    assert_eq!(db.get(&"net_clk".into()), Some(&"v1".into()));

    // Redo Level 1 -> Level 2
    cancelled.clear();
    restored.clear();
    assert!(db.redo(&mut cancelled, &mut restored));
    assert_eq!(db.stack_level(), 2);
    assert_eq!(db.get(&"net_vcc".into()), None);
    assert_eq!(db.get(&"net_clk".into()), Some(&"v2".into()));

    // Pop snapshot (commit level 2 into level 1)
    assert!(db.pop_snapshot());
    assert_eq!(db.stack_level(), 1);
    assert_eq!(db.get(&"net_vcc".into()), None);
    assert_eq!(db.get(&"net_clk".into()), Some(&"v2".into()));
}

#[test]
fn test_identifier_type_and_writer() {
    let ident = IdentifierType::specctra_default();
    assert_eq!(ident.format_identifier("net_normal"), "net_normal");
    assert_eq!(ident.format_identifier("net with space"), "\"net with space\"");
    assert_eq!(ident.format_identifier("(parens)"), "\"(parens)\"");

    let mut buf = Vec::new();
    let mut writer = IndentFileWriter::new(&mut buf);
    writer.start_scope_newline().unwrap();
    writer.write_str("structure").unwrap();
    writer.start_scope_newline().unwrap();
    writer.write_str("layer ").unwrap();
    ident.write_to("Top (Copper)", &mut writer).unwrap();
    writer.end_scope().unwrap();
    writer.end_scope().unwrap();

    let output = String::from_utf8(buf).unwrap();
    assert!(output.contains("(structure\n  (layer \"Top (Copper)\"\n  )\n)"));
}

#[test]
fn test_array_stack_operations() {
    let mut stack = ArrayStack::new(2);
    for i in 0..100 {
        stack.push(i);
    }
    assert_eq!(stack.len(), 100);
    assert_eq!(stack.peek(), Some(&99));
    for i in (0..100).rev() {
        assert_eq!(stack.pop(), Some(i));
    }
    assert!(stack.is_empty());
}

#[test]
fn test_time_limit_and_stoppable() {
    let mut tl = TimeLimit::new(100);
    assert!(!tl.limit_exceeded());
    tl.multiply(2.0);
    assert_eq!(tl.limit_millis(), 200);

    let stoppable = AtomicStoppable::new();
    assert!(!stoppable.is_stop_requested());
    stoppable.request_stop();
    assert!(stoppable.is_stop_requested());
}

#[test]
fn test_id_generators() {
    let mut seq = SequentialIdGenerator::with_start(100);
    assert_eq!(seq.new_no(), 101);
    assert_eq!(seq.new_no(), 102);
    assert_eq!(seq.max_generated_no(), 102);

    let atom = AtomicIdGenerator::with_start(200);
    assert_eq!(atom.next_id(), 201);
    assert_eq!(atom.next_id(), 202);
    assert_eq!(atom.max_id(), 202);
}
