//! Tests for T3: Parallel Connection Routing.
//!
//! Verifies:
//! 1. Zero added DRC clearance violations across N ∈ {1, 2, 8}.
//! 2. Determinism: identical board outputs when run twice at the same N.
//! 3. Unrouted connection non-regression across thread counts.
//! 4. Conflict detection: conflicting/crossing nets cleanly fall back to serial routing.

use fr_board::board::{Board, Layer, LayerStructure};
use fr_board::item::{BoardOutline, FixedState, Item, ItemKind, LayerRange, Pin};
use fr_board::net_nos::NetNos;
use fr_board::shape::{Padstack as BoardPadstack, TileShape};
use fr_geometry::planar::{IntBox, IntPoint, Polyline};
use fr_router::batch::threads::Threads;
use fr_router::batch::{incomplete_count, BatchRouter};
use fr_router::test_support::empty_board;
use fr_rules::board_rules::BoardRules;
use fr_rules::router_settings::RouterSettings;

struct MultiNetBoard {
    board: Board,
    rules: BoardRules,
    settings: RouterSettings,
}

/// Creates a multi-net synthetic board with `num_nets` point-to-point pin pairs.
///
/// If `cross` is false, nets are laid out in horizontal parallel tracks (spatially disjoint).
/// If `cross` is true, some nets are laid out vertically across the horizontal ones (spatially conflicting).
fn make_multi_net_board(num_nets: usize, cross: bool) -> MultiNetBoard {
    let layers = LayerStructure::new(vec![Layer::signal("L0"), Layer::signal("L1")]);
    let (mut board, mut rules, settings) = empty_board(layers);

    let (ox, oy) = (6_000, 6_000);
    board.insert_item(Item::new(
        NetNos::EMPTY,
        0,
        FixedState::SystemFixed,
        ItemKind::BoardOutline(BoardOutline {
            borders: vec![Polyline::from_int_points(&[
                IntPoint::new(-ox, -oy),
                IntPoint::new(ox, -oy),
                IntPoint::new(ox, oy),
                IntPoint::new(-ox, oy),
                IntPoint::new(-ox, -oy),
            ])],
            keepout_outside_outline: false,
        }),
    ));

    let pad_no = board.padstacks_mut().push(BoardPadstack::uniform(
        0,
        0,
        TileShape::Box(IntBox::new(-100, -100, 100, 100)),
    ));

    for net_idx in 0..num_nets {
        let net_no = (net_idx + 1) as i32;
        rules.nets.add(format!("N{net_no}"), net_no, false, 0);

        let (p1, p2) = if cross && net_idx % 2 == 1 {
            // Vertical net crossing the horizontal tracks
            let x = -1500 + (net_idx as i32) * 500;
            (IntPoint::new(x, -1800), IntPoint::new(x, 1800))
        } else {
            // Horizontal net
            let y = -1500 + (net_idx as i32) * 400;
            (IntPoint::new(-2000, y), IntPoint::new(2000, y))
        };

        for (pin_idx, center) in [p1, p2].into_iter().enumerate() {
            board.insert_pin(
                Pin {
                    center,
                    padstack_no: pad_no,
                    layers: LayerRange::new(0, 0),
                    pin_no: pin_idx as u32,
                },
                NetNos::of(net_no),
                1,
            );
        }
    }

    MultiNetBoard {
        board,
        rules,
        settings,
    }
}

fn board_copper_snapshot(board: &Board) -> Vec<(i32, Vec<IntPoint>)> {
    let mut out = Vec::new();
    for (_id, item) in board.store().iter() {
        if let ItemKind::Trace(tr) = &item.kind {
            let pts: Vec<IntPoint> = tr
                .polyline
                .corner_arr()
                .iter()
                .map(|p| match p {
                    fr_geometry::planar::Point::Int(ip) => *ip,
                    other => other.to_float().round(),
                })
                .collect();
            out.push((tr.layer, pts));
        }
    }
    out
}

fn clearance_violations(board: &Board, rules: &BoardRules) -> usize {
    let shapes = fr_check::shape_store(board);
    let view = fr_check::StoreView::new(
        board.store(),
        &shapes,
        &rules.clearance_matrix,
        board.layers().len(),
    );
    fr_check::check_clearance(&view).len()
}

#[test]
fn parallel_connections_zero_added_violations_across_thread_counts() {
    for &threads in &[1, 2, 4, 8] {
        let mut fixture = make_multi_net_board(8, false);
        let mut router = BatchRouter::new(&fixture.rules, &fixture.settings)
            .with_parallel_connections(Threads::new(threads));
        let outcome = router.autoroute_pass(&mut fixture.board, 1);

        assert!(
            outcome.routed > 0,
            "must route connections at threads={threads}"
        );
        assert_eq!(
            incomplete_count(&mut fixture.board),
            0,
            "all nets must be routed cleanly at threads={threads}"
        );

        let violations = clearance_violations(&fixture.board, &fixture.rules);
        assert_eq!(
            violations, 0,
            "must have 0 clearance violations at threads={threads}"
        );
    }
}

#[test]
fn parallel_connections_determinism_at_fixed_n() {
    for &threads in &[2, 4, 8] {
        let mut fixture1 = make_multi_net_board(8, true);
        let mut router1 = BatchRouter::new(&fixture1.rules, &fixture1.settings)
            .with_parallel_connections(Threads::new(threads));
        let _outcome1 = router1.autoroute_pass(&mut fixture1.board, 1);
        let snap1 = board_copper_snapshot(&fixture1.board);

        let mut fixture2 = make_multi_net_board(8, true);
        let mut router2 = BatchRouter::new(&fixture2.rules, &fixture2.settings)
            .with_parallel_connections(Threads::new(threads));
        let _outcome2 = router2.autoroute_pass(&mut fixture2.board, 1);
        let snap2 = board_copper_snapshot(&fixture2.board);

        assert_eq!(
            snap1, snap2,
            "board copper geometry must be bit-identical between runs at threads={threads}"
        );
    }
}

#[test]
fn parallel_connections_handles_spatial_conflicts() {
    // Board with crossing nets that trigger conflict detection & serial fallback
    let mut fixture = make_multi_net_board(6, true);
    let mut router = BatchRouter::new(&fixture.rules, &fixture.settings)
        .with_parallel_connections(Threads::new(4));
    let outcome = router.autoroute_pass(&mut fixture.board, 1);

    assert!(outcome.routed > 0);
    let violations = clearance_violations(&fixture.board, &fixture.rules);
    assert_eq!(
        violations, 0,
        "conflicting nets routed in parallel pass must have 0 clearance violations"
    );
}
