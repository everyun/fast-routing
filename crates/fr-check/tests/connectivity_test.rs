//! Connectivity, the ratsnest node set, and the two exemptions that decide
//! whether a pair of items is "the same net" or "a violation".
//!
//! `drc-connectivity.md` §2.4 (the tie pin), §4.3 (exact-point contacts),
//! §4.5 (tails) and §4.6 (the v2 filter and what survives it).

mod support;

use fr_board::item::{ItemKind, LayerRange, ObstacleKind};
use fr_board::NetNos;
use fr_check::{
    board_connectivity, check_clearance, connectivity, ContactGraph, UniformClearance,
};
use fr_geometry::planar::{IntBox, IntPoint};
use support::{keepout_item, pin_item, pour_item, straight_trace, via_item, Scaffold};

// ---------------------------------------------------------------------------
// the tie-pin exemption (§2.4)
// ---------------------------------------------------------------------------

/// Two traces of *different* nets that both end at a pin carrying *both* nets
/// are allowed to overlap. That is what a tie pin is for, and without the
/// exemption every star point on a board would report a violation.
#[test]
fn a_multi_net_tie_pin_exempts_the_two_traces_that_meet_on_it() {
    let overlap = |pin_nets: &[i32]| {
        let mut board = Scaffold::new(1);
        let pin = board.insert_one(
            pin_item(pin_nets, IntPoint::new(0, 0), LayerRange::single(0)),
            IntBox::new(-20, -20, 20, 20),
            0,
        );
        let (a, a_shape) =
            straight_trace(&[1], 0, IntPoint::new(0, 0), IntPoint::new(100, 0), 10);
        let (b, b_shape) =
            straight_trace(&[2], 0, IntPoint::new(0, 0), IntPoint::new(0, 100), 10);
        let id_a = board.insert_one(a, a_shape, 0);
        let id_b = board.insert_one(b, b_shape, 0);
        let rules = UniformClearance(20);
        let violations = check_clearance(&board.view(&rules));
        let _ = pin;
        violations
            .iter()
            .any(|v| (v.a, v.b) == (id_a, id_b) || (v.a, v.b) == (id_b, id_a))
    };

    // The two traces' copper genuinely overlaps in box(0, 0, 10, 10).
    assert!(
        overlap(&[1]),
        "a pin on only one of the two nets is not a tie pin"
    );
    assert!(
        !overlap(&[1, 2]),
        "a pin carrying both nets exempts the pair"
    );
}

/// The exemption is anchored at an *endpoint*. Two traces crossing mid-span next
/// to a tie pin are still a violation.
#[test]
fn the_tie_pin_exemption_needs_the_traces_to_end_on_the_pin() {
    let mut board = Scaffold::new(1);
    board.insert_one(
        pin_item(&[1, 2], IntPoint::new(0, 0), LayerRange::single(0)),
        IntBox::new(-20, -20, 20, 20),
        0,
    );
    // B runs past A's midpoint; neither trace ends where the other one does.
    let (a, a_shape) =
        straight_trace(&[1], 0, IntPoint::new(0, 0), IntPoint::new(100, 0), 10);
    let (b, b_shape) =
        straight_trace(&[2], 0, IntPoint::new(50, -50), IntPoint::new(50, 50), 10);
    let id_a = board.insert_one(a, a_shape, 0);
    let id_b = board.insert_one(b, b_shape, 0);

    let rules = UniformClearance(20);
    let violations = check_clearance(&board.view(&rules));
    assert!(violations
        .iter()
        .any(|v| (v.a, v.b) == (id_a, id_b) || (v.a, v.b) == (id_b, id_a)));
}

// ---------------------------------------------------------------------------
// contacts (§4.3)
// ---------------------------------------------------------------------------

/// A trace connects only at its two end corners. Crossing another trace of the
/// same net on the same layer mid-span is *not* a connection — upstream keeps
/// traces split at junctions precisely because of this.
#[test]
fn traces_connect_at_their_ends_only_never_mid_span() {
    let mut board = Scaffold::new(1);
    let (a, a_shape) = straight_trace(&[1], 0, IntPoint::new(0, 0), IntPoint::new(100, 0), 10);
    let (b, b_shape) = straight_trace(&[1], 0, IntPoint::new(50, -50), IntPoint::new(50, 50), 10);
    let id_a = board.insert_one(a, a_shape, 0);
    let id_b = board.insert_one(b, b_shape, 0);

    let rules = UniformClearance(0);
    let view = board.view(&rules);
    let graph = ContactGraph::build(&view);
    assert_eq!(graph.contacts(id_a).count(), 0);
    assert_eq!(graph.contacts(id_b).count(), 0);

    // End to end, they do connect.
    let mut board = Scaffold::new(1);
    let (a, a_shape) = straight_trace(&[1], 0, IntPoint::new(0, 0), IntPoint::new(100, 0), 10);
    let (b, b_shape) = straight_trace(&[1], 0, IntPoint::new(100, 0), IntPoint::new(100, 100), 10);
    let id_a = board.insert_one(a, a_shape, 0);
    let id_b = board.insert_one(b, b_shape, 0);
    let view = board.view(&rules);
    let graph = ContactGraph::build(&view);
    assert_eq!(graph.contacts(id_a).collect::<Vec<_>>(), vec![id_b]);
}

/// Same point, same net, different layers is not a contact.
#[test]
fn contacts_require_a_shared_layer() {
    let mut board = Scaffold::new(2);
    let (a, a_shape) = straight_trace(&[1], 0, IntPoint::new(0, 0), IntPoint::new(100, 0), 10);
    let (b, b_shape) = straight_trace(&[1], 1, IntPoint::new(100, 0), IntPoint::new(100, 100), 10);
    let id_a = board.insert_one(a, a_shape, 0);
    let id_b = board.insert_one(b, b_shape, 1);
    let rules = UniformClearance(0);
    let view = board.view(&rules);
    let graph = ContactGraph::build(&view);
    assert_eq!(graph.contacts(id_a).count(), 0);
    assert_eq!(graph.contacts(id_b).count(), 0);
}

/// A conduction area connects by containing the other item's connection point.
/// Two conduction areas never contact each other, however much copper they
/// share.
#[test]
fn conduction_areas_connect_by_containment_but_never_to_each_other() {
    let mut board = Scaffold::new(1);
    let pour_a = board.insert_one(
        pour_item(&[1], 0, IntBox::new(0, 0, 1000, 1000), false),
        IntBox::new(0, 0, 1000, 1000),
        0,
    );
    let pour_b = board.insert_one(
        pour_item(&[1], 0, IntBox::new(500, 0, 1500, 1000), false),
        IntBox::new(500, 0, 1500, 1000),
        0,
    );
    let pin = board.insert_one(
        pin_item(&[1], IntPoint::new(200, 200), LayerRange::single(0)),
        IntBox::new(190, 190, 210, 210),
        0,
    );
    let outside = board.insert_one(
        pin_item(&[1], IntPoint::new(2000, 2000), LayerRange::single(0)),
        IntBox::new(1990, 1990, 2010, 2010),
        0,
    );

    let rules = UniformClearance(0);
    let view = board.view(&rules);
    let graph = ContactGraph::build(&view);
    assert_eq!(graph.contacts(pin).collect::<Vec<_>>(), vec![pour_a]);
    assert_eq!(graph.contacts(pour_a).collect::<Vec<_>>(), vec![pin]);
    assert_eq!(graph.contacts(pour_b).count(), 0);
    assert_eq!(graph.contacts(outside).count(), 0);
}

// ---------------------------------------------------------------------------
// tails and the ratsnest node set (§4.5, §4.6)
// ---------------------------------------------------------------------------

/// An isolated via is a tail and is **not** a ratsnest node, so it neither adds
/// an incomplete of its own nor gets an airline drawn to it.
#[test]
fn an_isolated_via_is_a_tail_and_leaves_the_node_set() {
    let mut board = Scaffold::new(2);
    let p1 = board.insert_one(
        pin_item(&[1], IntPoint::new(0, 0), LayerRange::single(0)),
        IntBox::new(-10, -10, 10, 10),
        0,
    );
    let p2 = board.insert_one(
        pin_item(&[1], IntPoint::new(1000, 0), LayerRange::single(0)),
        IntBox::new(990, -10, 1010, 10),
        0,
    );
    let via = board.insert(
        via_item(&[1], IntPoint::new(500, 500), LayerRange::new(0, 1)),
        vec![
            fr_check::ItemShape::new(IntBox::new(490, 490, 510, 510), 0),
            fr_check::ItemShape::new(IntBox::new(490, 490, 510, 510), 1),
        ],
    );

    let rules = UniformClearance(0);
    let view = board.view(&rules);
    let graph = ContactGraph::build(&view);
    assert!(graph.is_tail(via), "no contacts at all => tail");
    assert!(!graph.is_tail(p1));

    let net = connectivity(&view, 1);
    assert_eq!(net.node_components, 2, "the two pins, not the via");
    assert_eq!(net.incompletes, 1);
    assert_eq!(net.airlines.len(), 1);
    let a = &net.airlines[0];
    assert!(
        a.from_item != via && a.to_item != via,
        "no airline may terminate on a tail via: {a:?}"
    );
    assert_eq!(
        [a.from_item.min(a.to_item), a.from_item.max(a.to_item)],
        [p1.min(p2), p1.max(p2)]
    );
    // max_connections counts Pins and ConductionAreas only.
    assert_eq!(net.max_connections, 1);
}

/// A via with a single contact is still a tail, and a trace hanging off it is
/// not a ratsnest node either — so a half-routed net reports the same one
/// incomplete as a bare one.
#[test]
fn a_via_with_one_contact_is_a_tail_and_traces_are_never_nodes() {
    let mut board = Scaffold::new(2);
    let p1 = board.insert_one(
        pin_item(&[1], IntPoint::new(0, 0), LayerRange::single(0)),
        IntBox::new(-10, -10, 10, 10),
        0,
    );
    let p2 = board.insert_one(
        pin_item(&[1], IntPoint::new(1000, 0), LayerRange::single(0)),
        IntBox::new(990, -10, 1010, 10),
        0,
    );
    let (t, t_shape) = straight_trace(&[1], 0, IntPoint::new(0, 0), IntPoint::new(500, 0), 10);
    let trace = board.insert_one(t, t_shape, 0);
    let via = board.insert(
        via_item(&[1], IntPoint::new(500, 0), LayerRange::new(0, 1)),
        vec![
            fr_check::ItemShape::new(IntBox::new(490, -10, 510, 10), 0),
            fr_check::ItemShape::new(IntBox::new(490, -10, 510, 10), 1),
        ],
    );

    let rules = UniformClearance(0);
    let view = board.view(&rules);
    let graph = ContactGraph::build(&view);
    assert!(graph.is_tail(via), "one contact => tail");
    assert!(!graph.is_tail(trace), "both ends contacted => not a tail");

    let net = connectivity(&view, 1);
    // Groups over the filtered items: {p1, trace} and {p2}. The trace carries no
    // ratsnest corner, so both groups reduce to one pin each.
    assert_eq!(net.components, 2);
    assert_eq!(net.node_components, 2);
    assert_eq!(net.incompletes, 1);
    assert_eq!(
        [
            net.airlines[0].from_item.min(net.airlines[0].to_item),
            net.airlines[0].from_item.max(net.airlines[0].to_item)
        ],
        [p1.min(p2), p1.max(p2)]
    );
}

/// A via whose contacts all span the identical layer pair changes no layer and
/// is a tail (Via.java:170-187) — but it is still a bridge for the connected-set
/// closure, because upstream's `getConnectedSet` walks the *unfiltered* board.
#[test]
fn a_via_between_two_same_layer_traces_is_a_tail_yet_still_conducts() {
    let mut board = Scaffold::new(2);
    let p1 = board.insert_one(
        pin_item(&[1], IntPoint::new(0, 0), LayerRange::single(0)),
        IntBox::new(-10, -10, 10, 10),
        0,
    );
    let p2 = board.insert_one(
        pin_item(&[1], IntPoint::new(1000, 0), LayerRange::single(0)),
        IntBox::new(990, -10, 1010, 10),
        0,
    );
    let (t1, s1) = straight_trace(&[1], 0, IntPoint::new(0, 0), IntPoint::new(500, 0), 10);
    let (t2, s2) = straight_trace(&[1], 0, IntPoint::new(500, 0), IntPoint::new(1000, 0), 10);
    board.insert_one(t1, s1, 0);
    board.insert_one(t2, s2, 0);
    let via = board.insert(
        via_item(&[1], IntPoint::new(500, 0), LayerRange::new(0, 1)),
        vec![
            fr_check::ItemShape::new(IntBox::new(490, -10, 510, 10), 0),
            fr_check::ItemShape::new(IntBox::new(490, -10, 510, 10), 1),
        ],
    );

    let rules = UniformClearance(0);
    let view = board.view(&rules);
    let graph = ContactGraph::build(&view);
    assert!(
        graph.is_tail(via),
        "both contacts are layer-0 traces, so the via transits nothing"
    );

    let net = connectivity(&view, 1);
    assert_eq!(net.components, 1, "the closure runs through the filtered via");
    assert_eq!(net.node_components, 1);
    assert_eq!(net.incompletes, 0);
    assert!(net.airlines.is_empty());
    let _ = (p1, p2);
}

/// A via that actually transits layers is not a tail and *is* a node.
#[test]
fn a_layer_changing_via_is_not_a_tail() {
    let mut board = Scaffold::new(2);
    board.insert_one(
        pin_item(&[1], IntPoint::new(0, 0), LayerRange::single(0)),
        IntBox::new(-10, -10, 10, 10),
        0,
    );
    board.insert_one(
        pin_item(&[1], IntPoint::new(1000, 0), LayerRange::single(1)),
        IntBox::new(990, -10, 1010, 10),
        1,
    );
    let (t1, s1) = straight_trace(&[1], 0, IntPoint::new(0, 0), IntPoint::new(500, 0), 10);
    let (t2, s2) = straight_trace(&[1], 1, IntPoint::new(500, 0), IntPoint::new(1000, 0), 10);
    board.insert_one(t1, s1, 0);
    board.insert_one(t2, s2, 1);
    let via = board.insert(
        via_item(&[1], IntPoint::new(500, 0), LayerRange::new(0, 1)),
        vec![
            fr_check::ItemShape::new(IntBox::new(490, -10, 510, 10), 0),
            fr_check::ItemShape::new(IntBox::new(490, -10, 510, 10), 1),
        ],
    );

    let rules = UniformClearance(0);
    let view = board.view(&rules);
    let graph = ContactGraph::build(&view);
    assert!(!graph.is_tail(via));

    let net = connectivity(&view, 1);
    assert_eq!(net.node_components, 1);
    assert_eq!(net.incompletes, 0);
}

/// `max_connections` counts pins and conduction areas, never traces or vias, and
/// clamps at zero for a net with a single endpoint (§4.7).
#[test]
fn max_connections_counts_pins_and_pours_only() {
    let mut board = Scaffold::new(1);
    // net 1: three pins and a plane -> 4 endpoints -> 3 connections.
    for (i, x) in [0, 1000, 2000].into_iter().enumerate() {
        let _ = i;
        board.insert_one(
            pin_item(&[1], IntPoint::new(x, 0), LayerRange::single(0)),
            IntBox::new(x - 10, -10, x + 10, 10),
            0,
        );
    }
    board.insert_one(
        pour_item(&[1], 0, IntBox::new(0, 5000, 1000, 6000), false),
        IntBox::new(0, 5000, 1000, 6000),
        0,
    );
    let (t, s) = straight_trace(&[1], 0, IntPoint::new(0, 0), IntPoint::new(1000, 0), 10);
    board.insert_one(t, s, 0);
    // net 2: a single pin -> 0 connections.
    board.insert_one(
        pin_item(&[2], IntPoint::new(0, 3000), LayerRange::single(0)),
        IntBox::new(-10, 2990, 10, 3010),
        0,
    );

    let rules = UniformClearance(0);
    let view = board.view(&rules);
    let all = board_connectivity(&view);
    assert_eq!(all.max_connections, 3);
    assert_eq!(all.nets.len(), 2);
    assert_eq!(all.nets[0].net, 1);
    assert_eq!(all.nets[0].max_connections, 3);
    assert_eq!(all.nets[1].max_connections, 0);
    assert_eq!(all.nets[1].incompletes, 0);
}

/// Keepouts and outlines are never connectable, so they never appear in a net's
/// item set even when they carry a net number.
#[test]
fn keepouts_are_not_connectable() {
    let mut board = Scaffold::new(1);
    let keepout = board.insert_one(
        keepout_item(&[1], 0, IntBox::new(0, 0, 100, 100), ObstacleKind::Keepout),
        IntBox::new(0, 0, 100, 100),
        0,
    );
    board.insert_one(
        pin_item(&[1], IntPoint::new(50, 50), LayerRange::single(0)),
        IntBox::new(40, 40, 60, 60),
        0,
    );
    let rules = UniformClearance(0);
    let view = board.view(&rules);
    assert!(!view
        .store()
        .get(keepout)
        .unwrap()
        .is_connectable());
    let net = connectivity(&view, 1);
    assert_eq!(net.max_connections, 0);
    assert_eq!(net.node_components, 1);
    assert_eq!(net.incompletes, 0);
}

/// A multi-net item belongs to every one of its nets.
#[test]
fn a_tie_pin_participates_in_both_of_its_nets() {
    let mut board = Scaffold::new(1);
    let mut tie = pin_item(&[], IntPoint::new(0, 0), LayerRange::single(0));
    tie.nets = NetNos::from_slice(&[1, 2]);
    assert!(matches!(tie.kind, ItemKind::Pin(_)));
    board.insert_one(tie, IntBox::new(-10, -10, 10, 10), 0);
    board.insert_one(
        pin_item(&[1], IntPoint::new(1000, 0), LayerRange::single(0)),
        IntBox::new(990, -10, 1010, 10),
        0,
    );
    board.insert_one(
        pin_item(&[2], IntPoint::new(0, 1000), LayerRange::single(0)),
        IntBox::new(-10, 990, 10, 1010),
        0,
    );

    let rules = UniformClearance(0);
    let view = board.view(&rules);
    let all = board_connectivity(&view);
    assert_eq!(all.nets.len(), 2);
    assert_eq!(all.max_connections, 2, "one connection in each net");
    assert_eq!(all.incomplete_count, 2);
}
