//! Multilayer parity tests: via rules, blind/buried vias, layer preference costs,
//! DestinationDistance heuristics on multilayer boards, and plane ConductionArea handling.

use fr_board::board::{Board, Layer, LayerStructure};
use fr_board::clearance::ClearanceTable;
use fr_board::item::{BoardOutline, Item, ItemKind};
use fr_board::net_nos::NetNos;
use fr_geometry::planar::{FloatPoint, IntBox, IntPoint, Polyline};
use fr_rules::board_rules::BoardRules;
use fr_rules::clearance_matrix::ClearanceMatrix;
use fr_rules::layer::{Layer as RulesLayer, LayerStructure as RulesLayerStructure};
use fr_rules::padstack::{PadShape, Padstack as RulesPadstack};
use fr_rules::router_settings::{BoardAspectRatio, RouterSettings};
use fr_rules::via_info::ViaInfo;
use fr_rules::via_rule::ViaRule;

use fr_router::ctrl::AutorouteControl;
use fr_router::maze::heuristic::DestinationDistance;

const TEST_NET: i32 = 1;
const TEST_HALF_WIDTH: i32 = 50;
const TEST_CLEARANCE: i32 = 20;

/// Builds a synthetic 4-layer or 6-layer board rules and layer structure.
fn make_rules_and_settings(
    layer_names: &[&str],
    is_signal: &[bool],
    via_rule: ViaRule,
) -> (BoardRules, RouterSettings) {
    let mut rules_layers = Vec::new();
    for (&name, &sig) in layer_names.iter().zip(is_signal.iter()) {
        rules_layers.push(RulesLayer::new(name, sig));
    }
    let r_ls = RulesLayerStructure::new(rules_layers);
    let matrix = ClearanceMatrix::default_instance(r_ls.clone(), TEST_CLEARANCE);
    let mut rules = BoardRules::new(r_ls.clone(), matrix);
    if let Some(net_class) = rules.net_classes.get_mut(0) {
        net_class.set_trace_half_width(TEST_HALF_WIDTH);
    }
    if !via_rule.is_empty() {
        rules.via_rules.push(via_rule.clone());
        if let Some(net_class) = rules.net_classes.get_mut(0) {
            net_class.set_via_rule(Some(via_rule));
        }
    }

    let mut settings = RouterSettings::default();
    settings.set_layer_count(layer_names.len());
    let aspect = BoardAspectRatio::from_bounding_box(6000.0, 3000.0);
    settings.apply_board_specific_optimizations(aspect, &r_ls);

    (rules, settings)
}

fn make_board(layer_names: &[&str], is_signal: &[bool]) -> Board {
    let mut layers = Vec::new();
    for (&name, &sig) in layer_names.iter().zip(is_signal.iter()) {
        layers.push(if sig {
            Layer::signal(name)
        } else {
            Layer::plane(name)
        });
    }
    let layer_struct = LayerStructure::new(layers);
    let clearance_table = ClearanceTable::uniform(2, layer_struct.len(), TEST_CLEARANCE);
    let mut board = Board::new(layer_struct, clearance_table);

    let outline_poly = Polyline::from_int_points(&[
        IntPoint::new(-3000, -1500),
        IntPoint::new(3000, -1500),
        IntPoint::new(3000, 1500),
        IntPoint::new(-3000, 1500),
        IntPoint::new(-3000, -1500),
    ]);
    let outline_item = Item::new(
        NetNos::EMPTY,
        0,
        fr_board::item::FixedState::SystemFixed,
        ItemKind::BoardOutline(BoardOutline {
            borders: vec![outline_poly],
            keepout_outside_outline: false,
        }),
    );
    board.insert_item(outline_item);
    board
}

#[test]
fn test_multilayer_board_trace_costs_and_aspect_ratio_boost() {
    // 4 signal layers: signal_layer_count = 4 > 2
    // Outer layers (0 and 3) should have outer_add_costs = 0.2 * 4 = 0.8 added.
    let layer_names = ["F.Cu", "In1.Cu", "In2.Cu", "B.Cu"];
    let is_signal = [true, true, true, true];
    let (_rules, settings) = make_rules_and_settings(&layer_names, &is_signal, ViaRule::empty());

    // Outer layers (0 and 3) must be 1.0 + 0.8 = 1.8
    assert!((settings.preferred_direction_trace_costs(0) - 1.8).abs() < 1e-9);
    assert!((settings.preferred_direction_trace_costs(3) - 1.8).abs() < 1e-9);

    // Inner layers (1 and 2) stay at 1.0
    assert!((settings.preferred_direction_trace_costs(1) - 1.0).abs() < 1e-9);
    assert!((settings.preferred_direction_trace_costs(2) - 1.0).abs() < 1e-9);

    // Also check non-signal power plane layers (e.g. 4-layer with 2 planes: F.Cu, In1.Cu(power), In2.Cu(power), B.Cu)
    let is_signal_with_planes = [true, false, false, true];
    let (_, settings_planes) =
        make_rules_and_settings(&layer_names, &is_signal_with_planes, ViaRule::empty());

    // Power planes are marked not routable (layer_active = false)
    assert!(settings_planes.layer_active(0));
    assert!(!settings_planes.layer_active(1));
    assert!(!settings_planes.layer_active(2));
    assert!(settings_planes.layer_active(3));
}

#[test]
fn test_multilayer_padstack_and_via_rule_spans() {
    // Through-hole via: layers 0..3
    let th_shapes = vec![
        Some(PadShape::Circle { radius: 150 }),
        Some(PadShape::Circle { radius: 150 }),
        Some(PadShape::Circle { radius: 150 }),
        Some(PadShape::Circle { radius: 150 }),
    ];
    let th_padstack = RulesPadstack::new("Via[0-3]", 1, th_shapes, true, false);
    assert_eq!(th_padstack.from_layer(), 0);
    assert_eq!(th_padstack.to_layer(), 3);

    // Blind via (top to inner1): layers 0..1
    let blind_top_shapes = vec![
        Some(PadShape::Circle { radius: 100 }),
        Some(PadShape::Circle { radius: 100 }),
        None,
        None,
    ];
    let blind_top_padstack = RulesPadstack::new("Via[0-1]", 2, blind_top_shapes, true, false);
    assert_eq!(blind_top_padstack.from_layer(), 0);
    assert_eq!(blind_top_padstack.to_layer(), 1);

    // Buried via (inner1 to inner2): layers 1..2
    let buried_shapes = vec![
        None,
        Some(PadShape::Circle { radius: 100 }),
        Some(PadShape::Circle { radius: 100 }),
        None,
    ];
    let buried_padstack = RulesPadstack::new("Via[1-2]", 3, buried_shapes, true, false);
    assert_eq!(buried_padstack.from_layer(), 1);
    assert_eq!(buried_padstack.to_layer(), 2);

    // Blind via (inner2 to bottom): layers 2..3
    let blind_bot_shapes = vec![
        None,
        None,
        Some(PadShape::Circle { radius: 100 }),
        Some(PadShape::Circle { radius: 100 }),
    ];
    let blind_bot_padstack = RulesPadstack::new("Via[2-3]", 4, blind_bot_shapes, true, false);
    assert_eq!(blind_bot_padstack.from_layer(), 2);
    assert_eq!(blind_bot_padstack.to_layer(), 3);

    // Build a ViaRule containing all 4 via padstacks
    let mut via_rule = ViaRule::new("hdi_rule");
    via_rule.append_via(ViaInfo::new("Via[0-3]", th_padstack, 1, true));
    via_rule.append_via(ViaInfo::new("Via[0-1]", blind_top_padstack, 1, true));
    via_rule.append_via(ViaInfo::new("Via[1-2]", buried_padstack, 1, true));
    via_rule.append_via(ViaInfo::new("Via[2-3]", blind_bot_padstack, 1, true));

    assert_eq!(via_rule.via_count(), 4);
    assert_eq!(
        via_rule.get_layer_range(0, 3).map(|v| v.name()),
        Some("Via[0-3]")
    );
    assert_eq!(
        via_rule.get_layer_range(0, 1).map(|v| v.name()),
        Some("Via[0-1]")
    );
    assert_eq!(
        via_rule.get_layer_range(1, 2).map(|v| v.name()),
        Some("Via[1-2]")
    );
    assert_eq!(
        via_rule.get_layer_range(2, 3).map(|v| v.name()),
        Some("Via[2-3]")
    );
    assert_eq!(via_rule.get_layer_range(0, 2), None);
}

#[test]
fn test_autoroute_control_multilayer_via_masks() {
    let layer_names = ["F.Cu", "In1.Cu", "In2.Cu", "B.Cu"];
    let is_signal = [true, true, true, true];

    let th_shapes = vec![
        Some(PadShape::Circle { radius: 150 }),
        Some(PadShape::Circle { radius: 150 }),
        Some(PadShape::Circle { radius: 150 }),
        Some(PadShape::Circle { radius: 150 }),
    ];
    let blind_top_shapes = vec![
        Some(PadShape::Circle { radius: 100 }),
        Some(PadShape::Circle { radius: 100 }),
        None,
        None,
    ];
    let buried_shapes = vec![
        None,
        Some(PadShape::Circle { radius: 100 }),
        Some(PadShape::Circle { radius: 100 }),
        None,
    ];

    let mut via_rule = ViaRule::new("multi_via_rule");
    via_rule.append_via(ViaInfo::new(
        "Via[0-3]",
        RulesPadstack::new("Via[0-3]", 1, th_shapes, true, false),
        1,
        true,
    ));
    via_rule.append_via(ViaInfo::new(
        "Via[0-1]",
        RulesPadstack::new("Via[0-1]", 2, blind_top_shapes, true, false),
        1,
        true,
    ));
    via_rule.append_via(ViaInfo::new(
        "Via[1-2]",
        RulesPadstack::new("Via[1-2]", 3, buried_shapes, true, false),
        1,
        true,
    ));

    let (rules, settings) = make_rules_and_settings(&layer_names, &is_signal, via_rule);
    let board = make_board(&layer_names, &is_signal);

    let ctrl = AutorouteControl::for_net(&board, &rules, &settings, TEST_NET);

    assert_eq!(ctrl.layer_count, 4);
    assert_eq!(ctrl.via_info_arr.len(), 3);

    // Check via masks
    assert_eq!(ctrl.via_info_arr[0].from_layer, 0);
    assert_eq!(ctrl.via_info_arr[0].to_layer, 3);
    assert_eq!(ctrl.via_info_arr[1].from_layer, 0);
    assert_eq!(ctrl.via_info_arr[1].to_layer, 1);
    assert_eq!(ctrl.via_info_arr[2].from_layer, 1);
    assert_eq!(ctrl.via_info_arr[2].to_layer, 2);

    // Max via radius is max over all layers (150 on layer 0, 150 on layer 3)
    assert!((ctrl.max_via_radius - 150.0).abs() < 1e-9);
    assert!(ctrl.min_normal_via_cost > 0.0);
    assert!((ctrl.min_cheap_via_cost - 0.8 * ctrl.min_normal_via_cost).abs() < 1e-9);
}

#[test]
fn test_destination_distance_multilayer_heuristics() {
    let trace_costs = vec![
        fr_rules::router_settings::ExpansionCostFactor {
            horizontal: 1.8,
            vertical: 2.2,
        },
        fr_rules::router_settings::ExpansionCostFactor {
            horizontal: 1.0,
            vertical: 1.4,
        },
        fr_rules::router_settings::ExpansionCostFactor {
            horizontal: 1.4,
            vertical: 1.0,
        },
        fr_rules::router_settings::ExpansionCostFactor {
            horizontal: 2.2,
            vertical: 1.8,
        },
    ];
    let layer_active = vec![true, true, true, true];
    let min_normal_via_cost = 1000.0;
    let min_cheap_via_cost = 800.0;

    let mut dest_dist = DestinationDistance::new(
        &trace_costs,
        &layer_active,
        min_normal_via_cost,
        min_cheap_via_cost,
    );

    // Add a destination box on layer 3 (solder side) at (2000, 1000)
    let dest_box = IntBox::new(1950, 950, 2050, 1050);
    dest_dist.join(&dest_box, 3);
    assert!(dest_dist.has_destination());

    // From layer 0 (component side) at (-2000, -1000), distance must include via penalty
    let query_point = FloatPoint::new(-2000.0, -1000.0);
    let dist_layer0 = dest_dist.calculate(&query_point, 0);
    let dist_layer3 = dest_dist.calculate(&query_point, 3);

    // Layer 0 to layer 3 must include at least min_normal_via_cost
    assert!(dist_layer0 >= dist_layer3);
    assert!(dist_layer0 >= min_normal_via_cost);

    // From layer 1 (inner layer)
    let dist_layer1 = dest_dist.calculate(&query_point, 1);
    assert!(dist_layer1 >= min_normal_via_cost);
}
