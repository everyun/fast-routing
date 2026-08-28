//! Synthetic boards for the crate's own tests, and for the integration tests.
//!
//! Public behind the `test-support` feature so `tests/` can build the same
//! boards the unit tests use without duplicating the wiring. It is not part of
//! the router's API contract — downstream code should build boards through
//! `fr-io` or `fr-board` directly.

use fr_board::board::{Board, Layer, LayerStructure};
use fr_board::clearance::ClearanceTable;
use fr_board::item::{BoardOutline, Item, ItemId, ItemKind, LayerRange, Pin};
use fr_board::net_nos::NetNos;
use fr_board::shape::{Padstack as BoardPadstack, TileShape};
use fr_geometry::planar::{IntBox, IntPoint, Polyline};
use fr_rules::board_rules::BoardRules;
use fr_rules::clearance_matrix::ClearanceMatrix;
use fr_rules::layer::{Layer as RulesLayer, LayerStructure as RulesLayerStructure};
use fr_rules::padstack::{PadShape, Padstack as RulesPadstack};
use fr_rules::router_settings::{BoardAspectRatio, RouterSettings};
use fr_rules::via_info::ViaInfo;
use fr_rules::via_rule::ViaRule;

use crate::ctrl::AutorouteControl;
use crate::engine::AutorouteEngine;

/// The net every fixture routes.
pub const TEST_NET: i32 = 1;

/// The trace half width every fixture uses.
pub const TEST_HALF_WIDTH: i32 = 50;

/// The clearance every fixture uses.
pub const TEST_CLEARANCE: i32 = 20;

/// Half the width and height of the fixtures' board outline.
pub const OUTLINE_HALF_EXTENT: (i32, i32) = (3_000, 1_500);

/// A board plus the rules and settings the engine needs.
pub struct TestFixture {
    /// The board.
    pub board: Board,
    /// The rules.
    pub rules: BoardRules,
    /// The router settings, already through
    /// [`apply_board_specific_optimizations`](RouterSettings::apply_board_specific_optimizations).
    pub settings: RouterSettings,
    pads: Vec<ItemId>,
    pad_centers: Vec<IntPoint>,
    trace: Option<ItemId>,
}

impl TestFixture {
    /// An empty two-signal-layer board with a routable net and a via rule.
    pub fn empty_two_layer() -> TestFixture {
        build(2, &[], None)
    }

    /// Two SMD-ish pads of [`TEST_NET`] on layer 0, 4000 units apart, on an
    /// otherwise empty two-layer board. This is the "tiny synthetic board" of
    /// the integration test.
    pub fn two_layer_with_pads() -> TestFixture {
        build(2, &[IntPoint::new(-2000, 0), IntPoint::new(2000, 0)], None)
    }

    /// [`two_layer_with_pads`](Self::two_layer_with_pads) with a foreign-net
    /// trace straight across the middle, for the ripup tests.
    pub fn with_foreign_trace() -> TestFixture {
        build(
            2,
            &[IntPoint::new(-2000, 0), IntPoint::new(2000, 0)],
            Some((IntPoint::new(0, -1500), IntPoint::new(0, 1500))),
        )
    }

    /// A pure-SMD net: every connectable item is a single-layer pin.
    pub fn pure_smd_two_layer() -> TestFixture {
        Self::two_layer_with_pads()
    }

    /// The pads, in insertion order.
    pub fn pad_ids(&self) -> Vec<ItemId> {
        self.pads.clone()
    }

    /// The pad centres, in insertion order.
    pub fn pad_centers(&self) -> Vec<IntPoint> {
        self.pad_centers.clone()
    }

    /// The foreign-net trace, when the fixture has one.
    pub fn trace_id(&self) -> ItemId {
        self.trace.expect("fixture has no trace")
    }

    /// The control for [`TEST_NET`].
    pub fn control(&self) -> AutorouteControl {
        AutorouteControl::for_net(&self.board, &self.rules, &self.settings, TEST_NET)
    }

    /// An engine over this fixture.
    pub fn engine(&self) -> AutorouteEngine<'_> {
        AutorouteEngine::new(&self.board, &self.rules, self.control())
    }
}

/// An empty board and rules for an arbitrary layer stack.
pub fn empty_board(layers: LayerStructure) -> (Board, BoardRules, RouterSettings) {
    let layer_count = layers.len();
    let rules_layers = RulesLayerStructure::new(
        (0..layer_count)
            .map(|i| {
                RulesLayer::new(
                    layers.name(i as i32).unwrap_or("layer").to_string(),
                    layers.is_signal(i as i32),
                )
            })
            .collect(),
    );
    let mut rules = BoardRules::new(
        rules_layers.clone(),
        ClearanceMatrix::default_instance(rules_layers, TEST_CLEARANCE),
    );
    rules
        .net_classes
        .get_mut(0)
        .expect("default net class")
        .set_trace_half_width(TEST_HALF_WIDTH);
    rules.nets.add("N1", 1, false, 0);

    let via_padstack = RulesPadstack::new(
        "via",
        0,
        (0..layer_count)
            .map(|_| Some(PadShape::Circle { radius: 150 }))
            .collect(),
        false,
        false,
    );
    let mut via_rule = ViaRule::new("default");
    via_rule.append_via(ViaInfo::new("via", via_padstack, 1, false));
    rules.via_rules.push(via_rule.clone());
    rules
        .net_classes
        .get_mut(0)
        .expect("default net class")
        .set_via_rule(Some(via_rule));

    let board = Board::new(
        layers,
        ClearanceTable::uniform(2, layer_count, TEST_CLEARANCE),
    );

    let rules_for_settings = rules.layer_structure.clone();
    let mut settings = RouterSettings::default();
    settings.set_layer_count(layer_count);
    settings.apply_board_specific_optimizations(
        BoardAspectRatio::from_bounding_box(10_000.0, 10_000.0),
        &rules_for_settings,
    );
    (board, rules, settings)
}

fn build(
    layer_count: usize,
    pads: &[IntPoint],
    trace: Option<(IntPoint, IntPoint)>,
) -> TestFixture {
    let layers = LayerStructure::new(
        (0..layer_count)
            .map(|i| Layer::signal(format!("L{i}")))
            .collect(),
    );
    let (mut board, mut rules, settings) = empty_board(layers);

    // Every real board has an outline, and the search tree turns it into a
    // non-routable obstacle strip around every border line
    // (`search-tree-obstacles.md` §3.6). Without one the room tiling has edges
    // with no touching neighbour and the gap-room builder has nothing to
    // anchor on, so the fixtures carry one.
    let (ox, oy) = OUTLINE_HALF_EXTENT;
    board.insert_item(Item::new(
        NetNos::EMPTY,
        0,
        fr_board::item::FixedState::SystemFixed,
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

    // One square pad shape on layer 0 for every pin.
    let padstack_no = board.padstacks_mut().push(BoardPadstack::uniform(
        0,
        0,
        TileShape::Box(IntBox::new(-200, -200, 200, 200)),
    ));

    let mut pad_ids = Vec::new();
    for (index, center) in pads.iter().enumerate() {
        let id = board.insert_pin(
            Pin {
                center: *center,
                padstack_no,
                pin_no: index as u32,
                layers: LayerRange::new(0, 0),
            },
            NetNos::of(TEST_NET),
            1,
        );
        pad_ids.push(id);
    }

    let trace_id = trace.map(|(from, to)| {
        // A second net so the trace is a genuine obstacle to `TEST_NET`.
        rules.nets.add("N2", 1, false, 0);
        board
            .insert_trace_points(
                &[from, to],
                0,
                TEST_HALF_WIDTH,
                NetNos::of(2),
                1,
                fr_board::item::FixedState::Unfixed,
                None,
            )
            .expect("a two-corner trace inserts")
    });

    board.ensure_compensated_tree(1);

    TestFixture {
        board,
        rules,
        settings,
        pads: pad_ids,
        pad_centers: pads.to_vec(),
        trace: trace_id,
    }
}
