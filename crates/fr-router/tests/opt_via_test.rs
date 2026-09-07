//! Unit tests for `OptViaAlgo.opt_via_location`.
//!
//! Tests verify that via optimization moves vias along connected traces to shorten
//! more expensive layer segments, matching Java 2.3.0 release behavior exactly.

use fr_board::item::ItemKind;
use fr_geometry::planar::IntPoint;
use fr_io::specctra::import;
use fr_router::insert_forced::ChangedArea;
use fr_router::pull_tight::PullTightAlgo;
use fr_rules::router_settings::ExpansionCostFactor;

const TWO_LAYER_VIA_DSN: &str = r#"(pcb test.dsn
  (resolution um 1)
  (unit um)
  (structure
    (layer L0 (type signal))
    (layer L1 (type signal))
    (boundary (rect pcb -10000 -10000 20000 20000))
    (via "via_pad")
    (rule (width 100) (clearance 100))
  )
  (library
    (padstack "via_pad" (shape (circle L0 500)) (shape (circle L1 500)))
  )
  (network (net net1))
  (wiring
    (wire (path L0 100 0 0 1000 0) (net net1))
    (via "via_pad" 1000 0 (net net1))
    (wire (path L1 100 1000 0 1000 1000) (net net1))
  )
)"#;

const FANOUT_VIA_DSN: &str = r#"(pcb test.dsn
  (resolution um 1)
  (unit um)
  (structure
    (layer L0 (type signal))
    (layer L1 (type signal))
    (boundary (rect pcb -10000 -10000 20000 20000))
    (via "via_pad")
    (rule (width 100) (clearance 100))
  )
  (library
    (padstack "via_pad" (shape (circle L0 500)) (shape (circle L1 500)))
  )
  (network (net net1))
  (wiring
    (wire (path L0 100 0 0 1000 0 1000 1000) (net net1))
    (via "via_pad" 1000 1000 (net net1))
  )
)"#;

#[test]
fn opt_via_moves_to_shorten_more_expensive_layer() {
    let mut design = import(TWO_LAYER_VIA_DSN.as_bytes()).unwrap();

    let mut via_id = None;
    for (id, item) in design.board.store().iter() {
        if let ItemKind::Via(v) = &item.kind {
            assert_eq!(v.center, IntPoint::new(1000, 0));
            via_id = Some(id);
            break;
        }
    }
    let via_id = via_id.expect("via exists");

    // Trace costs: layer 0 cheap (1.0), layer 1 expensive (2.0).
    // Moving via toward (0, 0) shortens the expensive layer 1 trace.
    let trace_costs = [
        ExpansionCostFactor {
            horizontal: 1.0,
            vertical: 1.0,
        },
        ExpansionCostFactor {
            horizontal: 2.0,
            vertical: 2.0,
        },
    ];

    let mut changed_area = ChangedArea::new(design.board.layers().len());
    let mut algo = PullTightAlgo::new(
        &mut design.board,
        &design.rules,
        &[],
        None,
        100,
        None,
        None,
        0,
    );

    let moved = algo.opt_via_location(via_id, Some(&trace_costs), 10, &mut changed_area);
    assert!(moved, "via should move towards (0, 0)");

    let item = design.board.store().get(via_id).unwrap();
    if let ItemKind::Via(v) = &item.kind {
        assert_eq!(
            v.center,
            IntPoint::new(0, 0),
            "via should be pulled to (0, 0) matching upstream Java"
        );
    } else {
        panic!("not a via");
    }
}

#[test]
fn opt_via_stays_when_moving_would_increase_cost() {
    let mut design = import(TWO_LAYER_VIA_DSN.as_bytes()).unwrap();

    let mut via_id = None;
    for (id, item) in design.board.store().iter() {
        if let ItemKind::Via(v) = &item.kind {
            assert_eq!(v.center, IntPoint::new(1000, 0));
            via_id = Some(id);
            break;
        }
    }
    let via_id = via_id.expect("via exists");

    // Trace costs: layer 0 (1.0), layer 1 (1.0) - equal costs.
    // Moving via would not reduce cost, so it stays.
    let trace_costs = [
        ExpansionCostFactor {
            horizontal: 1.0,
            vertical: 1.0,
        },
        ExpansionCostFactor {
            horizontal: 1.0,
            vertical: 1.0,
        },
    ];

    let mut changed_area = ChangedArea::new(design.board.layers().len());
    let mut algo = PullTightAlgo::new(
        &mut design.board,
        &design.rules,
        &[],
        None,
        100,
        None,
        None,
        0,
    );

    let moved = algo.opt_via_location(via_id, Some(&trace_costs), 10, &mut changed_area);
    assert!(!moved, "via should not move when costs are equal");

    let item = design.board.store().get(via_id).unwrap();
    if let ItemKind::Via(v) = &item.kind {
        assert_eq!(
            v.center,
            IntPoint::new(1000, 0),
            "via should remain at (1000, 0)"
        );
    }
}

#[test]
fn opt_fanout_via_pulls_back_along_trace() {
    let mut design = import(FANOUT_VIA_DSN.as_bytes()).unwrap();

    let mut via_id = None;
    for (id, item) in design.board.store().iter() {
        if let ItemKind::Via(v) = &item.kind {
            assert_eq!(v.center, IntPoint::new(1000, 1000));
            via_id = Some(id);
            break;
        }
    }
    let via_id = via_id.expect("via exists");

    let trace_costs = [
        ExpansionCostFactor {
            horizontal: 1.0,
            vertical: 1.0,
        },
        ExpansionCostFactor {
            horizontal: 1.0,
            vertical: 1.0,
        },
    ];

    let mut changed_area = ChangedArea::new(design.board.layers().len());
    let mut algo = PullTightAlgo::new(
        &mut design.board,
        &design.rules,
        &[],
        None,
        100,
        None,
        None,
        0,
    );

    let moved = algo.opt_via_location(via_id, Some(&trace_costs), 10, &mut changed_area);
    assert!(moved, "fanout via should move along trace");

    let item = design.board.store().get(via_id).unwrap();
    if let ItemKind::Via(v) = &item.kind {
        assert_eq!(
            v.center,
            IntPoint::new(0, 0),
            "fanout via should be pulled back through both corners to (0, 0) matching upstream Java"
        );
    }
}
