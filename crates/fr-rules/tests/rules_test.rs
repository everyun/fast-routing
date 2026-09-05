use fr_geometry::{Direction, IntBox, IntOctagon};
use fr_rules::*;

fn create_test_layer_structure(count: usize) -> LayerStructure {
    let mut layers = Vec::with_capacity(count);
    for i in 0..count {
        let name = match i {
            0 => "top".to_string(),
            n if n == count - 1 => "bottom".to_string(),
            n => format!("inner_{n}"),
        };
        // inner odd layers can be power planes in some tests
        let is_signal = true;
        layers.push(Layer::new(name, is_signal));
    }
    LayerStructure::new(layers)
}

#[test]
fn test_default_item_clearance_classes() {
    let mut def = DefaultItemClearanceClasses::new();
    assert_eq!(def.get(ItemClass::None), 0);
    assert_eq!(def.get(ItemClass::Trace), 1);
    assert_eq!(def.get(ItemClass::Via), 1);
    assert_eq!(def.get(ItemClass::Pin), 1);
    assert_eq!(def.get(ItemClass::Smd), 1);
    assert_eq!(def.get(ItemClass::Area), 1);

    def.set(ItemClass::Trace, 3);
    assert_eq!(def.get(ItemClass::Trace), 3);

    def.set_all(5);
    assert_eq!(def.get(ItemClass::None), 0); // None should not be overwritten
    assert_eq!(def.get(ItemClass::Trace), 5);
    assert_eq!(def.get(ItemClass::Via), 5);
    assert_eq!(def.get(ItemClass::Pin), 5);
    assert_eq!(def.get(ItemClass::Smd), 5);
    assert_eq!(def.get(ItemClass::Area), 5);

    assert_eq!(ItemClass::from_ordinal(0), Some(ItemClass::None));
    assert_eq!(ItemClass::from_ordinal(1), Some(ItemClass::Trace));
    assert_eq!(ItemClass::from_ordinal(5), Some(ItemClass::Area));
    assert_eq!(ItemClass::from_ordinal(6), None);
}

#[test]
fn test_layer_structure() {
    let ls = create_test_layer_structure(4);
    assert_eq!(ls.len(), 4);
    assert_eq!(ls.signal_layer_count(), 4);
    assert_eq!(ls.get_no("top"), Some(0));
    assert_eq!(ls.get_no("inner_1"), Some(1));
    assert_eq!(ls.get_no("bottom"), Some(3));
    assert_eq!(ls.get_no("nonexistent"), None);

    assert_eq!(ls.get_signal_layer(0).map(|l| l.name.as_str()), Some("top"));
    assert_eq!(
        ls.get_signal_layer(1).map(|l| l.name.as_str()),
        Some("inner_1")
    );
    assert_eq!(ls.get_signal_layer_no(2), Some(2));
    assert_eq!(ls.get_layer_no(3), Some(3));
}

#[test]
fn test_clearance_matrix_basic() {
    let ls = create_test_layer_structure(4);
    let cm = ClearanceMatrix::default_instance(ls, 120);

    assert_eq!(cm.class_count(), 2);
    assert_eq!(cm.layer_count(), 4);
    assert_eq!(cm.get_no("null"), Some(0));
    assert_eq!(cm.get_no("default"), Some(1));
    assert_eq!(cm.get_no("DEFAULT"), Some(1)); // case-insensitive
    assert_eq!(cm.get_name(0), Some("null"));
    assert_eq!(cm.get_name(1), Some("default"));

    // Check default value initialization (class 1 to class 1 should be 120)
    for layer in 0..4 {
        assert_eq!(cm.get_value(1, 1, layer, false), 120);
        assert_eq!(
            cm.get_value(1, 1, layer, true),
            120 + CLEARANCE_SAFETY_MARGIN
        );
    }

    // Out-of-bounds queries return 0
    assert_eq!(cm.get_value(2, 1, 0, false), 0);
    assert_eq!(cm.get_value(1, 2, 0, false), 0);
    assert_eq!(cm.get_value(1, 1, 5, false), 0);
}

#[test]
fn test_clearance_matrix_even_normalization() {
    let ls = create_test_layer_structure(2);
    let mut cm = ClearanceMatrix::new(2, ls, &["null", "default"]);

    // Odd values are rounded up
    cm.set_value_on_layer(1, 1, 0, 15);
    assert_eq!(cm.get_value(1, 1, 0, false), 16);

    // Negative values are clamped to 0
    cm.set_value_on_layer(1, 1, 1, -10);
    assert_eq!(cm.get_value(1, 1, 1, false), 0);

    // i32::MAX is decremented
    cm.set_value_on_layer(1, 1, 0, i32::MAX);
    assert_eq!(cm.get_value(1, 1, 0, false), i32::MAX - 1);
}

#[test]
fn test_clearance_matrix_layer_dependent() {
    let ls = create_test_layer_structure(4);
    let mut cm = ClearanceMatrix::default_instance(ls, 100);

    assert!(!cm.is_layer_dependent(1, 1));
    assert!(!cm.is_inner_layer_dependent(1, 1));

    // Change layer 0 only
    cm.set_value_on_layer(1, 1, 0, 200);
    assert!(cm.is_layer_dependent(1, 1));
    assert!(!cm.is_inner_layer_dependent(1, 1));

    // Change inner layer 2
    cm.set_value_on_layer(1, 1, 2, 300);
    assert!(cm.is_inner_layer_dependent(1, 1));

    assert_eq!(cm.max_value_for_class(1, 0), 200);
    assert_eq!(cm.max_value_for_class(1, 2), 300);
    assert_eq!(cm.max_value_on_layer(0), 200);
    assert_eq!(cm.max_value_on_layer(2), 300);
}

#[test]
fn test_clearance_matrix_append_and_remove_class() {
    let ls = create_test_layer_structure(3);
    let mut cm = ClearanceMatrix::default_instance(ls, 100);

    assert_eq!(cm.class_count(), 2);
    assert!(cm.append_class("power"));
    assert_eq!(cm.class_count(), 3);
    assert_eq!(cm.get_no("power"), Some(2));
    assert!(!cm.append_class("power")); // cannot duplicate

    // Appended class should inherit values from class 1 (default)
    for layer in 0..3 {
        assert_eq!(cm.get_value(2, 1, layer, false), 100);
        assert_eq!(cm.get_value(1, 2, layer, false), 100);
        assert_eq!(cm.get_value(2, 2, layer, false), 100);
    }

    // Set custom value for power class
    cm.set_value(2, 2, 250);
    assert_eq!(cm.get_value(2, 2, 0, false), 250);
    assert_eq!(cm.get_value(1, 1, 0, false), 100);

    // Remove class
    cm.remove_class(2);
    assert_eq!(cm.class_count(), 2);
    assert_eq!(cm.get_no("power"), None);
}

#[test]
fn test_clearance_matrix_is_equal() {
    let ls = create_test_layer_structure(2);
    let mut cm = ClearanceMatrix::default_instance(ls, 100);
    cm.append_class("class2");

    assert!(cm.is_equal(1, 2));
    cm.set_value(2, 2, 200);
    assert!(!cm.is_equal(1, 2));
}

#[test]
fn test_padstack_and_padshape() {
    let box_shape = PadShape::Box(IntBox::new(-100, -50, 100, 50));
    assert_eq!(box_shape.max_width(), 200.0);
    assert_eq!(box_shape.bounding_box(), IntBox::new(-100, -50, 100, 50));

    let oct_shape = PadShape::Octagon(IntOctagon::new(-100, -100, 100, 100, -50, 50, -50, 50));
    assert_eq!(oct_shape.bounding_box(), IntBox::new(-100, -100, 100, 100));

    let circle_shape = PadShape::Circle { radius: 75 };
    assert_eq!(circle_shape.max_width(), 150.0);
    assert_eq!(circle_shape.bounding_box(), IntBox::new(-75, -75, 75, 75));

    let padstack = Padstack::new(
        "via_0.6:0.3",
        1,
        vec![Some(circle_shape.clone()), None, Some(circle_shape)],
        true,
        false,
    );

    assert_eq!(padstack.from_layer(), 0);
    assert_eq!(padstack.to_layer(), 2);
    assert_eq!(padstack.board_layer_count(), 3);
    assert!(padstack.get_shape(0).is_some());
    assert!(padstack.get_shape(1).is_none());

    // Drill radius parsed from name "via_0.6:0.3"
    let drill_r = padstack.get_drill_radius();
    assert!(drill_r > 0.0);

    // Trace exit directions
    let exit_dirs = padstack.get_trace_exit_directions(0, 1.5);
    assert!(exit_dirs.contains(&Direction::RIGHT));
    assert!(exit_dirs.contains(&Direction::LEFT));
    assert!(exit_dirs.contains(&Direction::UP));
    assert!(exit_dirs.contains(&Direction::DOWN));
}

#[test]
fn test_via_info_and_via_infos() {
    let mut via_infos = ViaInfos::new();
    assert_eq!(via_infos.count(), 0);
    assert!(via_infos.is_empty());

    let ps1 = Padstack::new(
        "ps1",
        1,
        vec![Some(PadShape::Circle { radius: 50 })],
        true,
        false,
    );
    let via1 = ViaInfo::new("via1", ps1, 1, true);

    let ps2 = Padstack::new(
        "ps2",
        2,
        vec![Some(PadShape::Circle { radius: 80 })],
        true,
        false,
    );
    let via2 = ViaInfo::new("via2", ps2, 1, false);

    assert!(via_infos.add(via1));
    assert!(via_infos.add(via2));
    assert!(!via_infos.add(ViaInfo::new(
        "via1",
        Padstack::new("ps_dup", 3, vec![], true, false),
        1,
        true
    )));

    assert_eq!(via_infos.count(), 2);
    assert!(via_infos.name_exists("via1"));
    assert!(via_infos.name_exists("via2"));
    assert!(!via_infos.name_exists("via3"));

    assert_eq!(via_infos.get(0).map(|v| v.name()), Some("via1"));
    assert_eq!(
        via_infos
            .get_by_name("via2")
            .map(|v| v.attach_smd_allowed()),
        Some(false)
    );

    assert!(via_infos.remove("via1"));
    assert_eq!(via_infos.count(), 1);
    assert!(!via_infos.name_exists("via1"));
}

#[test]
fn test_via_rule() {
    let mut rule = ViaRule::new("standard_vias");
    assert_eq!(rule.name, "standard_vias");
    assert_eq!(rule.via_count(), 0);

    let ps1 = Padstack::new(
        "ps_through",
        1,
        vec![
            Some(PadShape::Circle { radius: 60 }),
            Some(PadShape::Circle { radius: 60 }),
            Some(PadShape::Circle { radius: 60 }),
        ],
        true,
        false,
    );
    let via1 = ViaInfo::new("via_through", ps1, 1, true);

    let ps2 = Padstack::new(
        "ps_blind",
        2,
        vec![
            Some(PadShape::Circle { radius: 40 }),
            Some(PadShape::Circle { radius: 40 }),
            None,
        ],
        true,
        false,
    );
    let via2 = ViaInfo::new("via_blind", ps2, 1, true);

    rule.append_via(via1);
    rule.append_via(via2);

    assert_eq!(rule.via_count(), 2);
    assert!(rule.contains("via_through"));
    assert!(rule.contains_padstack("ps_blind"));

    let range_match = rule.get_layer_range(0, 2);
    assert_eq!(range_match.map(|v| v.name()), Some("via_through"));

    let blind_match = rule.get_layer_range(0, 1);
    assert_eq!(blind_match.map(|v| v.name()), Some("via_blind"));

    assert!(rule.swap("via_through", "via_blind"));
    assert_eq!(rule.get_via(0).map(|v| v.name()), Some("via_blind"));
    assert_eq!(rule.get_via(1).map(|v| v.name()), Some("via_through"));

    assert!(rule.remove_via("via_blind"));
    assert_eq!(rule.via_count(), 1);
}

#[test]
fn test_net_class_and_net_classes() {
    let ls = create_test_layer_structure(4);
    let mut ncs = NetClasses::new();
    assert_eq!(ncs.count(), 0);

    let idx1 = ncs.append("default", &ls, false);
    assert_eq!(idx1, 0);
    assert_eq!(ncs.count(), 1);

    let nc = ncs.get_mut(0).unwrap();
    nc.set_trace_half_width(150);
    nc.set_trace_clearance_class(1);

    assert_eq!(nc.get_trace_half_width(0), 150);
    assert_eq!(nc.get_trace_half_width(3), 150);
    assert!(!nc.trace_width_is_layer_dependent(&ls));

    nc.set_trace_half_width_on_layer(0, 200);
    assert!(nc.trace_width_is_layer_dependent(&ls));

    let idx2 = ncs.append_with_generated_name(&ls);
    assert_eq!(idx2, 1);
    assert_eq!(ncs.get(1).map(|c| c.name.as_str()), Some("class1"));

    let found = ncs.find(150, 1, None);
    assert!(found.is_none()); // because layer 0 was modified to 200

    let found_by_widths = ncs.find_by_widths(&[200, 150, 150, 150], 1, None);
    assert!(found_by_widths.is_some());
    assert_eq!(found_by_widths.unwrap().name, "default");
}

#[test]
fn test_net_and_nets() {
    let mut nets = Nets::new();
    assert_eq!(nets.len(), 0);
    assert!(nets.is_empty());

    let n1 = nets.add("GND", 1, true, 0).clone();
    assert_eq!(n1.net_number, 1);
    assert_eq!(n1.name, "GND");
    assert!(n1.contains_plane());

    let n2 = nets.add("VCC", 1, true, 0).clone();
    assert_eq!(n2.net_number, 2);

    let n3 = nets.add("CLK", 1, false, 1).clone();
    assert_eq!(n3.net_number, 3);
    assert!(!n3.contains_plane());

    assert_eq!(nets.len(), 3);
    assert_eq!(nets.max_net_no(), 3);

    assert_eq!(nets.get(1).map(|n| n.name.as_str()), Some("GND"));
    assert_eq!(nets.get(2).map(|n| n.name.as_str()), Some("VCC"));
    assert_eq!(nets.get(3).map(|n| n.name.as_str()), Some("CLK"));
    assert_eq!(nets.get(0), None);
    assert_eq!(nets.get(4), None);

    assert_eq!(
        nets.get_by_name_and_subnet("gnd", 1).map(|n| n.net_number),
        Some(1)
    );
    assert_eq!(nets.get_by_name("clk").len(), 1);

    assert!(Nets::is_normal_net_no(1));
    assert!(Nets::is_normal_net_no(MAX_LEGAL_NET_NO));
    assert!(!Nets::is_normal_net_no(0));
    assert!(!Nets::is_normal_net_no(HIDDEN_NET_NO));
}

#[test]
fn test_board_rules_aggregation() {
    let ls = create_test_layer_structure(4);
    let cm = ClearanceMatrix::default_instance(ls.clone(), 120);
    let mut rules = BoardRules::new(ls, cm);

    // Initial default net class check
    assert_eq!(rules.net_classes.count(), 1);
    assert_eq!(rules.get_default_net_class().name, "default");
    assert_eq!(rules.get_default_trace_half_width(0), 1500);

    // Default trace half width operations
    rules.set_default_trace_half_width(0, 1200);
    assert_eq!(rules.get_default_trace_half_width(0), 1200);
    assert_eq!(rules.get_min_trace_half_width(), 1200);

    rules.set_default_trace_half_widths(1000);
    assert_eq!(rules.get_default_trace_half_width(0), 1000);
    assert_eq!(rules.get_default_trace_half_width(3), 1000);

    // Nets registration through BoardRules
    rules.nets.add("SDA", 1, false, 0);
    rules.nets.add("SCL", 1, false, 0);
    assert_eq!(rules.get_trace_half_width(1, 0), 1000);
    assert!(!rules.trace_widths_are_layer_dependent(1));

    // Via setup
    let ps = Padstack::new(
        "via_padstack",
        1,
        vec![
            Some(PadShape::Circle { radius: 70 }),
            Some(PadShape::Circle { radius: 70 }),
            Some(PadShape::Circle { radius: 70 }),
            Some(PadShape::Circle { radius: 70 }),
        ],
        true,
        false,
    );
    let vi = ViaInfo::new("via_std", ps, 1, true);
    rules.via_infos.add(vi);

    rules.create_default_via_rule(0, "default_vias");
    assert!(rules.get_default_via_rule().is_some());
    assert_eq!(rules.get_default_via_rule().unwrap().name, "default_vias");
    assert_eq!(rules.get_default_via_diameter(), 140.0);

    // New net class creation
    let power_nc_idx = rules.get_new_net_class_with_name("PowerClass");
    let power_nc = rules.net_classes.get_mut(power_nc_idx).unwrap();
    power_nc.set_trace_half_width(3000);
    power_nc.set_trace_clearance_class(2);

    rules.nets.add("VDD", 1, true, power_nc_idx);
    assert_eq!(rules.get_trace_half_width(3, 0), 3000);

    // Clearance class cascading modification and removal
    rules.clearance_matrix.append_class("power_cl");
    assert_eq!(rules.clearance_matrix.get_no("power_cl"), Some(2));

    // Attempting to remove clearance class in use should return false
    assert!(!rules.remove_clearance_class(2));

    // Change clearance class to 1
    rules.change_clearance_class_no(2, 1);
    assert!(rules.remove_clearance_class(2));
    assert_eq!(rules.clearance_matrix.class_count(), 2);

    // Angle restriction & ignore conduction
    rules.set_trace_angle_restriction(AngleRestriction::NinetyDegree);
    assert_eq!(
        rules.get_trace_angle_restriction(),
        AngleRestriction::NinetyDegree
    );

    rules.set_ignore_conduction(false);
    assert!(!rules.get_ignore_conduction());

    rules.set_hole_clearance(150);
    assert_eq!(rules.get_hole_clearance(), 150);

    rules.set_pin_edge_to_turn_dist(250.0);
    assert_eq!(rules.get_pin_edge_to_turn_dist(), 250.0);

    rules.set_use_slow_autoroute_algorithm(true);
    assert!(rules.get_use_slow_autoroute_algorithm());
}

#[test]
fn test_clearance_matrix_inner_value_and_compensation() {
    let ls = create_test_layer_structure(4);
    let mut cm = ClearanceMatrix::default_instance(ls, 100);

    cm.set_inner_value(1, 1, 180);
    assert_eq!(cm.get_value(1, 1, 0, false), 100);
    assert_eq!(cm.get_value(1, 1, 1, false), 180);
    assert_eq!(cm.get_value(1, 1, 2, false), 180);
    assert_eq!(cm.get_value(1, 1, 3, false), 100);

    // Compensation value is (clearance + 1) / 2
    assert_eq!(cm.clearance_compensation_value(1, 0), 50);
    assert_eq!(cm.clearance_compensation_value(1, 1), 90);
}

#[test]
fn test_net_ordering_and_generated_names() {
    let net_a = Net::new("abc", 1, 1, false, 0);
    let net_b = Net::new("DEF", 1, 2, false, 0);
    let net_c = Net::new("xyz", 1, 3, false, 0);

    assert!(net_a < net_b);
    assert!(net_b < net_c);

    let mut nets = Nets::new();
    let n1 = nets.new_net(None, 0);
    assert_eq!(n1.name, "net#1");

    let n2 = nets.new_net(Some("custom_"), 0);
    assert_eq!(n2.name, "custom_2");
}

#[test]
fn test_net_class_extended_properties() {
    let ls = create_test_layer_structure(5);
    let mut nc = NetClass::new("test_class", &ls, false);

    assert_eq!(nc.layer_count(), 5);
    assert!(nc.is_active_routing_layer(0));
    assert!(nc.is_active_routing_layer(4));

    nc.set_active_routing_layer(1, false);
    assert!(!nc.is_active_routing_layer(1));

    nc.set_all_layers_active(true);
    assert!(nc.is_active_routing_layer(1));

    nc.set_all_inner_layers_active(false);
    assert!(nc.is_active_routing_layer(0));
    assert!(!nc.is_active_routing_layer(1));
    assert!(!nc.is_active_routing_layer(2));
    assert!(!nc.is_active_routing_layer(3));
    assert!(nc.is_active_routing_layer(4));

    nc.set_trace_half_width(100);
    nc.set_trace_half_width_on_inner(200);
    assert_eq!(nc.get_trace_half_width(0), 100);
    assert_eq!(nc.get_trace_half_width(1), 200);
    assert_eq!(nc.get_trace_half_width(2), 200);
    assert_eq!(nc.get_trace_half_width(3), 200);
    assert_eq!(nc.get_trace_half_width(4), 100);
    assert!(nc.trace_width_is_layer_dependent(&ls));

    // Inner layer dependency: change layer 2
    nc.set_trace_half_width_on_layer(2, 250);
    assert!(nc.trace_width_is_inner_layer_dependent(&ls));

    nc.shove_fixed = true;
    assert!(nc.shove_fixed);

    nc.pull_tight = false;
    assert!(!nc.pull_tight);

    nc.ignore_cycles_with_areas = true;
    assert!(nc.ignore_cycles_with_areas);

    nc.minimum_trace_length = 50.0;
    assert_eq!(nc.minimum_trace_length, 50.0);

    nc.maximum_trace_length = 500.0;
    assert_eq!(nc.maximum_trace_length, 500.0);
}

#[test]
fn test_via_rule_swap_indices() {
    let mut vr = ViaRule::new("test_swap");
    let ps1 = Padstack::new(
        "p1",
        1,
        vec![Some(PadShape::Circle { radius: 10 })],
        true,
        false,
    );
    let ps2 = Padstack::new(
        "p2",
        2,
        vec![Some(PadShape::Circle { radius: 20 })],
        true,
        false,
    );
    vr.append_via(ViaInfo::new("v1", ps1, 1, true));
    vr.append_via(ViaInfo::new("v2", ps2, 1, true));

    assert_eq!(vr.get_via(0).unwrap().name(), "v1");
    assert_eq!(vr.get_via(1).unwrap().name(), "v2");

    assert!(vr.swap_indices(0, 1));
    assert_eq!(vr.get_via(0).unwrap().name(), "v2");
    assert_eq!(vr.get_via(1).unwrap().name(), "v1");
}

// =================================================================================================
// Upstream-parity tests for the clearance matrix quirks.
//
// Reference: freerouting 2.3.0 @2d4de019, `app/freerouting/rules/ClearanceMatrix.java`, and
// docs/upstream-study/dsn-board-semantics.md §3.4 / §7.3 / §7.8,
// docs/upstream-study/v2-vs-v19-and-scope.md §14.2.
// =================================================================================================

/// `ClearanceMatrix.set_value(p_i, p_j, …)` writes `row[p_j].column[p_i]` and nothing else
/// (`ClearanceMatrix.java:109-127`). Upstream keeps the matrix symmetric by calling the setter
/// twice from the importer (`Structure.java:692-694`), not inside the setter, so a single
/// directional write legitimately leaves the matrix asymmetric.
#[test]
fn test_clearance_matrix_directional_setter_is_asymmetric() {
    let ls = create_test_layer_structure(2);
    let mut cm = ClearanceMatrix::default_instance(ls, 100);
    assert!(cm.append_class("power"));

    // One directional write: only (i=2, j=1) i.e. row[1].column[2] changes.
    cm.set_value(2, 1, 300);
    assert_eq!(cm.get_value(2, 1, 0, false), 300);
    assert_eq!(cm.get_value(1, 2, 0, false), 100);

    // The layer-scoped setter behaves the same way.
    cm.set_value_on_layer(1, 2, 1, 420);
    assert_eq!(cm.get_value(1, 2, 1, false), 420);
    assert_eq!(cm.get_value(2, 1, 1, false), 300);
}

/// The importer-level operation: two directional writes with swapped indices, on all layers or on
/// one layer (`Structure.java:692-700`, `Network.java:672-679`).
#[test]
fn test_clearance_matrix_symmetric_setters() {
    let ls = create_test_layer_structure(3);
    let mut cm = ClearanceMatrix::default_instance(ls, 100);
    assert!(cm.append_class("power"));

    cm.set_value_symmetric(1, 2, 300);
    for layer in 0..3 {
        assert_eq!(cm.get_value(1, 2, layer, false), 300);
        assert_eq!(cm.get_value(2, 1, layer, false), 300);
    }

    cm.set_value_on_layer_symmetric(1, 2, 1, 500);
    assert_eq!(cm.get_value(1, 2, 1, false), 500);
    assert_eq!(cm.get_value(2, 1, 1, false), 500);
    assert_eq!(cm.get_value(1, 2, 0, false), 300);
}

/// `set_value` only ever raises the two max caches (`ClearanceMatrix.java:122-124`), so lowering a
/// cell leaves them stale-high. Preserved deliberately: over-approximating the clearance halo is
/// the safe direction for the search tree. `recompute_max_values` is our non-upstream opt-in fix.
#[test]
fn test_clearance_matrix_max_value_cache_is_monotonic() {
    let ls = create_test_layer_structure(2);
    let mut cm = ClearanceMatrix::default_instance(ls, 100);

    cm.set_value_symmetric(1, 1, 400);
    assert_eq!(cm.max_value_for_class(1, 0), 400);
    assert_eq!(cm.max_value_on_layer(0), 400);

    // Lower it again: the stored cell drops, the caches do not.
    cm.set_value_symmetric(1, 1, 40);
    assert_eq!(cm.get_value(1, 1, 0, false), 40);
    assert_eq!(
        cm.max_value_for_class(1, 0),
        400,
        "upstream never lowers the row max cache"
    );
    assert_eq!(
        cm.max_value_on_layer(0),
        400,
        "upstream never lowers the layer max cache"
    );

    cm.recompute_max_values();
    assert_eq!(cm.max_value_for_class(1, 0), 40);
    assert_eq!(cm.max_value_on_layer(0), 40);
}

/// `ClearanceMatrix.remove_class` rebuilds every surviving row with `new Row(name)`, which zeroes
/// `max_value` and never refills it, while `max_value_on_layer` is left untouched
/// (`ClearanceMatrix.java:287-310`). The stored cells themselves survive.
#[test]
fn test_clearance_matrix_remove_class_resets_row_max_caches() {
    let ls = create_test_layer_structure(2);
    let mut cm = ClearanceMatrix::default_instance(ls, 100);
    assert!(cm.append_class("power"));
    cm.set_value_symmetric(2, 2, 600);
    assert_eq!(cm.max_value_for_class(2, 0), 600);
    assert_eq!(cm.max_value_on_layer(0), 600);

    cm.remove_class(2);

    assert_eq!(cm.class_count(), 2);
    assert_eq!(
        cm.get_value(1, 1, 0, false),
        100,
        "surviving cells keep their values"
    );
    assert_eq!(
        cm.max_value_for_class(1, 0),
        0,
        "per-row caches are reallocated to zero"
    );
    assert_eq!(
        cm.max_value_on_layer(0),
        600,
        "the global cache keeps the pre-removal maximum"
    );

    cm.recompute_max_values();
    assert_eq!(cm.max_value_for_class(1, 0), 100);
    assert_eq!(cm.max_value_on_layer(0), 100);
}

/// `set_default_value` writes every `(i, j)` with `i, j >= 1`, wiping previously set specific
/// pairs (`ClearanceMatrix.java:70-86`). That ordering constraint is why a PCB-level
/// `(clearance v)` with no `(type …)` must be applied before the typed rules
/// (dsn-board-semantics.md §7.3).
#[test]
fn test_clearance_matrix_default_value_wipes_specific_pairs() {
    let ls = create_test_layer_structure(2);
    let mut cm = ClearanceMatrix::default_instance(ls, 100);
    assert!(cm.append_class("power"));
    cm.set_value_symmetric(1, 2, 800);
    assert_eq!(cm.get_value(1, 2, 0, false), 800);

    cm.set_default_value(200);
    assert_eq!(cm.get_value(1, 2, 0, false), 200);
    assert_eq!(cm.get_value(2, 1, 0, false), 200);
    assert_eq!(cm.get_value(1, 1, 0, false), 200);
    // Class 0 ("null") is never touched by set_default_value.
    assert_eq!(cm.get_value(0, 0, 0, false), 0);
    assert_eq!(cm.get_value(0, 1, 0, false), 0);
}

/// Odd values are rounded *up* to even on store, so the `+ 1` in
/// `(clearance + 1) / 2` never fires for stored values (`ClearanceMatrix.java:113-120, 236-238`).
/// The safety margin is a read-side opt-in and is excluded from the compensation.
#[test]
fn test_clearance_matrix_compensation_uses_rounded_up_storage() {
    let ls = create_test_layer_structure(2);
    let mut cm = ClearanceMatrix::default_instance(ls, 0);

    cm.set_value_symmetric(1, 1, 15);
    assert_eq!(
        cm.get_value(1, 1, 0, false),
        16,
        "odd values round up to the next even integer"
    );
    assert_eq!(cm.clearance_compensation_value(1, 0), 8);
    // The safety margin is never folded into the compensation value.
    assert_eq!(cm.get_value(1, 1, 0, true), 16 + CLEARANCE_SAFETY_MARGIN);

    // Out-of-range compensation falls out of the out-of-range read returning 0.
    assert_eq!(cm.clearance_compensation_value(99, 0), 0);
}

/// `append_class` seeds the new row/column from class 1 as it stands *at that moment*
/// (`ClearanceMatrix.java:243-282`), and refuses a duplicate name case-insensitively.
#[test]
fn test_clearance_matrix_append_class_snapshots_the_default_class() {
    let ls = create_test_layer_structure(2);
    let mut cm = ClearanceMatrix::default_instance(ls, 100);
    cm.set_value_symmetric(1, 1, 240);

    assert!(cm.append_class("signal"));
    assert_eq!(cm.get_no("SIGNAL"), Some(2), "get_no is case-insensitive");
    assert_eq!(
        cm.get_name(2),
        Some("signal"),
        "get_name keeps the stored casing"
    );
    assert!(
        !cm.append_class("SIGNAL"),
        "duplicate names are rejected case-insensitively"
    );

    for layer in 0..2 {
        assert_eq!(cm.get_value(2, 1, layer, false), 240);
        assert_eq!(cm.get_value(1, 2, layer, false), 240);
        assert_eq!(cm.get_value(2, 2, layer, false), 240);
    }
}

// =================================================================================================
// Clearance-class name algebra (Structure.set_clearance_rule / Network.add_clearance_rule).
//
// Reference: docs/upstream-study/dsn-board-semantics.md §7.1-§7.8.
// =================================================================================================

/// The four reserved item classes are appended in the fixed order `via, smd, pin, area`
/// (`Structure.java:718-723`), so a fresh board assigns them indices 2, 3, 4, 5. Index stability
/// matters because keepouts/pins/vias are resolved by index later in the import.
#[test]
fn test_default_clearance_classes_have_stable_indices() {
    let ls = create_test_layer_structure(2);
    let mut cm = ClearanceMatrix::default_instance(ls, 100);

    let created = create_default_clearance_classes(&mut cm);
    let indices: Vec<usize> = created.iter().map(|(i, _)| *i).collect();
    assert_eq!(indices, vec![2, 3, 4, 5]);
    assert_eq!(RESERVED_ITEM_CLASS_NAMES, ["via", "smd", "pin", "area"]);
    assert_eq!(cm.get_name(2), Some("via"));
    assert_eq!(cm.get_name(3), Some("smd"));
    assert_eq!(cm.get_name(4), Some("pin"));
    assert_eq!(cm.get_name(5), Some("area"));

    let bindings: Vec<Option<ItemClass>> = created.iter().map(|(_, c)| *c).collect();
    assert_eq!(
        bindings,
        vec![
            Some(ItemClass::Via),
            Some(ItemClass::Smd),
            Some(ItemClass::Pin),
            Some(ItemClass::Area),
        ]
    );

    // Re-running is a no-op that reports the existing indices (append_class returns false, but
    // Structure.append_clearance_class re-reads get_no regardless).
    let again = create_default_clearance_classes(&mut cm);
    assert_eq!(
        again.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
        vec![2, 3, 4, 5]
    );
    assert_eq!(cm.class_count(), 6);

    // A non-reserved name gets a matrix class but no item-class binding.
    let (idx, binding) = append_clearance_class(&mut cm, "board_edge");
    assert_eq!(idx, 6);
    assert_eq!(binding, None);
}

/// `"wire"` is a magic alias for clearance class 1 and is matched case-sensitively via
/// `String.equals` (`Structure.java:675`), even though `ClearanceMatrix.get_no` is
/// case-insensitive.
#[test]
fn test_wire_alias_is_case_sensitive() {
    assert!(is_wire_alias("wire"));
    assert!(!is_wire_alias("Wire"));
    assert!(!is_wire_alias("WIRE"));
    assert_eq!(WIRE_CLASS_NAME, "wire");
    assert_eq!(
        WIRE_CLEARANCE_CLASS_NO,
        BoardRules::default_clearance_class()
    );

    // The reserved item-class switch is likewise case-sensitive (Structure.java:729-734).
    assert_eq!(reserved_item_class("via"), Some(ItemClass::Via));
    assert_eq!(reserved_item_class("VIA"), None);
    assert_eq!(reserved_item_class("trace"), None);
}

/// `Structure.contains_wire_clearance_pair` looks for the `wire_` prefix / `_wire` suffix on the
/// raw list entries (`Structure.java:709-716`). Because `(type …)` is split on `-`, the
/// `-`-separated encoding of the same rule arrives as two entries and therefore does **not**
/// trigger the default item clearance classes — a real behavioural difference between the two
/// encodings of the same rule.
#[test]
fn test_contains_wire_clearance_pair_only_sees_the_underscore_encoding() {
    assert!(contains_wire_clearance_pair(&["wire_via".to_string()]));
    assert!(contains_wire_clearance_pair(&["smd_wire".to_string()]));
    assert!(contains_wire_clearance_pair(&[
        "smd_via".to_string(),
        "wire_pin".to_string()
    ]));

    // `(type wire-via)` lexes to ["wire", "via"]: no entry starts with "wire_" or ends with "_wire".
    assert!(!contains_wire_clearance_pair(&[
        "wire".to_string(),
        "via".to_string()
    ]));
    assert!(!contains_wire_clearance_pair(&["Wire_via".to_string()]));
    assert!(!contains_wire_clearance_pair::<String>(&[]));
}

/// `Structure.set_clearance_rule`'s `classPairs.size() == 2` fast path — the v2-only branch that
/// handles `(type a-b)` (`Structure.java:643-651`).
#[test]
fn test_structure_class_pair_dash_encoded_form() {
    let pairs = vec!["smd".to_string(), "via".to_string()];
    // The pair comes from the whole list, so both loop iterations yield the same pair and the rule
    // is applied twice (idempotently).
    for entry in &pairs {
        assert_eq!(
            structure_class_pair(&pairs, entry, "\""),
            ClassPairResolution::Pair("smd".to_string(), "via".to_string())
        );
    }

    // Every '"' is stripped from both names, and a leading '_' is stripped from the second one.
    let quoted = vec!["\"My Class\"".to_string(), "_via".to_string()];
    assert_eq!(
        structure_class_pair(&quoted, &quoted[0], "\""),
        ClassPairResolution::Pair("My Class".to_string(), "via".to_string())
    );

    // Upstream's loop body targets curr_pair[1] on BOTH iterations, so two leading underscores are
    // stripped from the second name.
    let double_underscore = vec!["a".to_string(), "__b".to_string()];
    assert_eq!(
        structure_class_pair(&double_underscore, "a", "\""),
        ClassPairResolution::Pair("a".to_string(), "b".to_string())
    );
}

/// The `(type a_b)` single-entry form: `split("_", 2)` splits at the **first** underscore only, so
/// `a_b_c` is accepted as `("a", "b_c")` rather than dropped (`Structure.java:665-671`).
#[test]
fn test_structure_class_pair_underscore_encoded_form() {
    let one = |s: &str| vec![s.to_string()];

    assert_eq!(
        structure_class_pair(&one("wire_via"), "wire_via", "\""),
        ClassPairResolution::Pair("wire".to_string(), "via".to_string())
    );
    assert_eq!(
        structure_class_pair(&one("smd_via_same_net"), "smd_via_same_net", "\""),
        ClassPairResolution::Pair("smd".to_string(), "via_same_net".to_string()),
        "split(\"_\", 2) keeps the tail intact instead of skipping the entry"
    );
    // No underscore at all: nothing to split, upstream `continue`s.
    assert_eq!(
        structure_class_pair(&one("via"), "via", "\""),
        ClassPairResolution::Skip
    );
    // Trailing underscore is kept by a positive split limit.
    assert_eq!(
        structure_class_pair(&one("smd_"), "smd_", "\""),
        ClassPairResolution::Pair("smd".to_string(), String::new())
    );
}

/// The quoted-first-name branch (`Structure.java:653-663`), plus the `smd_to_turn_gap` pseudo pair
/// that sets `pin_edge_to_turn_dist` instead of a matrix cell (`Structure.java:637-641`).
#[test]
fn test_structure_class_pair_quoted_and_pseudo_forms() {
    // A `(type …)` list with a single entry, resolved the way the importer does.
    fn resolve_single(entry: &str, string_quote: &str) -> ClassPairResolution {
        let list = [entry.to_string()];
        structure_class_pair(&list, entry, string_quote)
    }

    assert_eq!(
        resolve_single("\"My Class\"_via", "\""),
        ClassPairResolution::Pair("My Class".to_string(), "via".to_string())
    );

    // Missing the '_' after the closing quote: upstream warns and skips.
    assert_eq!(
        resolve_single("\"My Class\"via", "\""),
        ClassPairResolution::Skip
    );
    // No closing quote at all.
    assert_eq!(
        resolve_single("\"My Class_via", "\""),
        ClassPairResolution::Skip
    );

    // A non-default string quote works the same way.
    assert_eq!(
        resolve_single("'My Class'_pin", "'"),
        ClassPairResolution::Pair("My Class".to_string(), "pin".to_string())
    );

    // smd_to_turn_gap is matched case-insensitively and short-circuits every other branch.
    assert_eq!(
        resolve_single("smd_to_turn_gap", "\""),
        ClassPairResolution::SmdToTurnGap
    );
    assert!(is_smd_to_turn_gap("SMD_TO_TURN_GAP"));
    assert_eq!(
        structure_class_pair(&["SMD_To_Turn_Gap".to_string()], "SMD_To_Turn_Gap", "\""),
        ClassPairResolution::SmdToTurnGap
    );
}

/// `Network.add_clearance_rule` splits on **every** underscore and demands exactly two parts
/// (`Network.java:666-671`), so `a_b_c` is silently dropped — the opposite of `Structure`'s
/// `split("_", 2)`. Java's `String.split` also discards trailing empty fields at limit 0.
#[test]
fn test_network_class_pair_drops_three_part_names() {
    assert_eq!(
        network_class_pair("wire_via"),
        Some(("wire".to_string(), "via".to_string()))
    );
    assert_eq!(network_class_pair("smd_via_same_net"), None);
    assert_eq!(network_class_pair("via"), None);
    // Trailing empties are discarded at limit 0, so "smd_" collapses to a single part and is
    // dropped — whereas Structure's split("_", 2) would have accepted it as ("smd", "").
    assert_eq!(network_class_pair("smd_"), None);
    assert_eq!(
        structure_class_pair(&["smd_".to_string()], "smd_", "\""),
        ClassPairResolution::Pair("smd".to_string(), String::new())
    );
    // Leading empties are kept.
    assert_eq!(
        network_class_pair("_via"),
        Some((String::new(), "via".to_string()))
    );
    // There is no smd_to_turn_gap branch at net-class level: it falls into the generic split and
    // is dropped for having four parts.
    assert_eq!(network_class_pair("smd_to_turn_gap"), None);
}

/// `Network.get_clearance_class` name construction (`Network.java:687-692`) and the derived via
/// info name (`Network.java:338`); both use `DsnFile.CLASS_CLEARANCE_SEPARATOR = '-'`.
///
/// The worked example is the one in dsn-board-semantics.md §7.6:
/// `(class SIGNAL … (clearance 200 (type wire_wire)) (clearance 300 (type wire_via)))` produces
/// the matrix classes `SIGNAL`, `SIGNAL-via`, `SIGNAL-smd`, `SIGNAL-pin`, `SIGNAL-area`.
#[test]
fn test_net_class_derived_clearance_class_names() {
    assert_eq!(CLASS_CLEARANCE_SEPARATOR, '-');
    assert_eq!(
        net_class_item_clearance_class_name("SIGNAL", "wire"),
        "SIGNAL"
    );
    assert_eq!(
        net_class_item_clearance_class_name("SIGNAL", "via"),
        "SIGNAL-via"
    );
    assert_eq!(
        net_class_item_clearance_class_name("SIGNAL", "smd"),
        "SIGNAL-smd"
    );
    assert_eq!(
        net_class_item_clearance_class_name("SIGNAL", "pin"),
        "SIGNAL-pin"
    );
    assert_eq!(
        net_class_item_clearance_class_name("SIGNAL", "area"),
        "SIGNAL-area"
    );
    // Case-sensitive alias: "Wire" is an ordinary item class name.
    assert_eq!(
        net_class_item_clearance_class_name("SIGNAL", "Wire"),
        "SIGNAL-Wire"
    );

    assert_eq!(
        default_via_info_name("Via[0-1]_800:400_um", "SIGNAL"),
        "Via[0-1]_800:400_um-SIGNAL"
    );

    // The §7.6 example, applied through the pure helpers.
    let expected: Vec<String> = ["wire", "via", "smd", "pin", "area"]
        .iter()
        .map(|item| net_class_item_clearance_class_name("SIGNAL", item))
        .collect();
    assert_eq!(
        expected,
        vec![
            "SIGNAL",
            "SIGNAL-via",
            "SIGNAL-smd",
            "SIGNAL-pin",
            "SIGNAL-area"
        ]
    );
}

// =================================================================================================
// RouterSettings.
//
// Reference: settings/RouterSettings.java, settings/sources/DefaultSettings.java, and
// docs/upstream-study/v2-vs-v19-and-scope.md §3 and §5.
// =================================================================================================

fn assert_close(actual: f64, expected: f64, what: &str) {
    assert!(
        (actual - expected).abs() < 1e-12,
        "{what}: expected {expected}, got {actual}"
    );
}

/// The `DefaultSettings.getSettings()` table (`DefaultSettings.java:81-155`),
/// v2-vs-v19-and-scope.md §3.1 / §3.3 / §3.4 / §3.5.
#[test]
fn test_router_settings_defaults_match_default_settings() {
    let s = RouterSettings::default();

    assert_eq!(s.enabled, Some(true));
    assert_eq!(s.algorithm, ALGORITHM_CURRENT);
    assert_eq!(ALGORITHM_CURRENT, "freerouting-router");
    assert_eq!(ALGORITHM_V19, "freerouting-router-v19");
    assert_eq!(s.job_timeout, "12:00:00");
    assert_eq!(s.max_passes, 9999);
    assert_eq!(s.max_items, i32::MAX);
    assert_eq!(s.trace_pull_tight_accuracy, 500);
    assert!(s.vias_allowed);
    assert!(s.automatic_neckdown);
    assert!(!s.save_intermediate_stages);
    assert!(s.ignore_net_classes.is_empty());
    assert_eq!(s.copper_to_edge_clearance_um, 500.0);
    assert_eq!(s.hole_clearance_um, 0.0);
    assert_eq!(s.neck_width_um, 0.0);
    assert!(!s.strict_drc);
    assert_eq!(s.max_threads, default_max_threads());
    // Layer-count-dependent arrays are intentionally empty until the board is known.
    assert!(s.layers.is_empty());
    assert!(s.scoring.preferred_direction_trace_cost.is_empty());

    // Scoring (§3.5) — start_ripup_costs is 100, not the AutorouteControl ctor value of 1000.
    assert_eq!(s.scoring.start_ripup_costs(), 100);
    assert_eq!(s.scoring.via_costs(), 50);
    assert_eq!(s.scoring.plane_via_costs(), 5);
    assert_eq!(s.scoring.unrouted_net_penalty, 5_000_000.0);
    assert_eq!(s.scoring.clearance_violation_penalty, 1_000_000.0);
    assert_eq!(s.scoring.bend_penalty, 10.0);
    assert_eq!(s.scoring.default_preferred_direction_trace_cost, Some(1.0));
    assert_eq!(s.scoring.default_undesired_direction_trace_cost, Some(1.0));
    assert_eq!(s.scoring.default_bend_cost, Some(0.0));
    assert_eq!(MIN_BEND_COST, 0.0);
    assert_eq!(MAX_BEND_COST, 9.9);

    // Fanout (§3.3).
    assert_eq!(s.fanout.enabled, Some(true));
    assert_eq!(s.fanout.max_passes, 20);
    assert_eq!(s.fanout.max_items, i32::MAX);
    assert_eq!(s.fanout.max_milliseconds_per_pin, 10_000);
    assert!(s.fanout.ripup_allowed);
    assert_eq!(s.fanout.min_escape_length_mm, 2.5);
    assert_eq!(s.fanout.max_escape_length_mm, 4.5);
    assert_eq!(s.fanout.start_via_diameter_mm, 0.250);
    assert_eq!(s.fanout.end_via_diameter_mm, 0.250);
    assert_eq!(s.fanout.pin_sorting_order, "outer_first");
    assert!(s.fanout.fallback_to_board_vias);
    assert_eq!(s.fanout.timeout, None);

    // Optimizer (§3.4).
    assert_eq!(s.optimizer.enabled, Some(true));
    assert_eq!(s.optimizer.algorithm, "freerouting-optimizer");
    assert_eq!(s.optimizer.max_passes, 100);
    assert_eq!(s.optimizer.max_items, i32::MAX);
    assert_eq!(s.optimizer.max_threads, default_max_threads());
    assert_eq!(s.optimizer.improvement_threshold, 0.01);
    assert_eq!(s.optimizer.max_consecutive_failures, 50);
    assert_eq!(s.optimizer.additional_ripup_cost_factor_at_start, 10);
    assert_eq!(s.optimizer.trace_ripup_cost_factor, 0.6);
    assert_eq!(s.optimizer.max_autoroute_passes, 6);
    assert_eq!(
        s.optimizer.board_update_strategy,
        BoardUpdateStrategy::Greedy
    );
    assert_eq!(s.optimizer.hybrid_ratio, "1:1");
    assert_eq!(
        s.optimizer.item_selection_strategy,
        ItemSelectionStrategy::Prioritized
    );
}

/// Three getters in one class with three different null policies
/// (`RouterSettings.java:483-512`): `getRunRouter()` ⇒ true, `getRunOptimizer()` ⇒ false,
/// `getRunFanout()` ⇒ true while `isFanoutEnabled()` ⇒ false.
#[test]
fn test_router_settings_null_policies() {
    let mut s = RouterSettings::default();
    s.enabled = None;
    s.optimizer.enabled = None;
    s.fanout.enabled = None;

    assert!(s.run_router(), "getRunRouter() defaults to true when unset");
    assert!(
        !s.run_optimizer(),
        "getRunOptimizer() defaults to false when unset"
    );
    assert!(s.run_fanout(), "getRunFanout() defaults to true when unset");
    assert!(
        !s.is_fanout_enabled(),
        "isFanoutEnabled() defaults to false when unset — the opposite policy in the same class"
    );

    // Scoring getters fall back to 1, not to the DefaultSettings value.
    s.scoring.via_costs = None;
    s.scoring.plane_via_costs = None;
    s.scoring.start_ripup_costs = None;
    assert_eq!(s.scoring.via_costs(), 1);
    assert_eq!(s.scoring.plane_via_costs(), 1);
    assert_eq!(s.scoring.start_ripup_costs(), 1);

    // Setters clamp to >= 1.
    s.scoring.set_via_costs(0);
    s.scoring.set_plane_via_costs(-4);
    s.scoring.set_start_ripup_costs(-1);
    assert_eq!(s.scoring.via_costs(), 1);
    assert_eq!(s.scoring.plane_via_costs(), 1);
    assert_eq!(s.scoring.start_ripup_costs(), 1);
}

/// `applyBoardSpecificOptimizations` on an all-signal 4-layer board wider than it is tall
/// (`RouterSettings.java:224-336`, v2-vs-v19-and-scope.md §5).
#[test]
fn test_apply_board_specific_optimizations_all_signal_layers() {
    let ls = create_test_layer_structure(4);
    let mut s = RouterSettings::default();

    // W = 200, H = 100 → horizAdd = 0.1*round(10*200/100) = 2.0,
    //                    vertAdd  = 0.1*round(10*100/200) = 0.5,
    //                    seed prefIsHorizontal = (200 < 100) = false.
    let aspect = BoardAspectRatio::from_bounding_box(200.0, 100.0);
    assert_close(
        aspect.horizontal_add_costs_against_preferred_dir(),
        2.0,
        "horizAdd",
    );
    assert_close(
        aspect.vertical_add_costs_against_preferred_dir(),
        0.5,
        "vertAdd",
    );
    assert!(!aspect.preferred_direction_is_horizontal_seed());

    s.apply_board_specific_optimizations(aspect, &ls);

    // The flip precedes the assignment, so layer 0 gets !(W < H) = true and the directions
    // alternate from there.
    assert!(s.preferred_direction_is_horizontal(0));
    assert!(!s.preferred_direction_is_horizontal(1));
    assert!(s.preferred_direction_is_horizontal(2));
    assert!(!s.preferred_direction_is_horizontal(3));

    // All four layers are signal, so signalLayerCount = 4 > 2 and outerAdd = 0.2*4 = 0.8 is added
    // to both cost arrays on layer 0 and on the last board layer.
    let outer = 0.2 * 4.0;
    assert_close(s.preferred_direction_trace_costs(0), 1.0 + outer, "pref[0]");
    assert_close(s.preferred_direction_trace_costs(1), 1.0, "pref[1]");
    assert_close(s.preferred_direction_trace_costs(2), 1.0, "pref[2]");
    assert_close(s.preferred_direction_trace_costs(3), 1.0 + outer, "pref[3]");

    assert_close(
        s.against_preferred_direction_trace_costs(0),
        1.0 + 2.0 + outer,
        "und[0]",
    );
    assert_close(
        s.against_preferred_direction_trace_costs(1),
        1.0 + 0.5,
        "und[1]",
    );
    assert_close(
        s.against_preferred_direction_trace_costs(2),
        1.0 + 2.0,
        "und[2]",
    );
    assert_close(
        s.against_preferred_direction_trace_costs(3),
        1.0 + 0.5 + outer,
        "und[3]",
    );

    // Every layer is routable and inherits the default bend cost.
    for i in 0..4 {
        assert!(s.layer_active(i));
        assert_close(s.bend_cost(i), 0.0, "bendCost");
    }
    assert!(s.board_specific_trace_costs_applied());

    // get_horizontal / get_vertical map (pref, undesired) through the preferred direction.
    assert_close(
        s.horizontal_trace_costs(0),
        s.preferred_direction_trace_costs(0),
        "h[0]",
    );
    assert_close(
        s.vertical_trace_costs(0),
        s.against_preferred_direction_trace_costs(0),
        "v[0]",
    );
    assert_close(
        s.horizontal_trace_costs(1),
        s.against_preferred_direction_trace_costs(1),
        "h[1]",
    );
    assert_close(
        s.vertical_trace_costs(1),
        s.preferred_direction_trace_costs(1),
        "v[1]",
    );

    let costs = s.trace_cost_arr();
    assert_eq!(costs.len(), 4);
    assert_close(costs[0].horizontal, s.horizontal_trace_costs(0), "arr h[0]");
    assert_close(costs[3].vertical, s.vertical_trace_costs(3), "arr v[3]");
}

/// Non-signal layers do not flip the alternating preferred direction, are force-marked
/// non-routable, and do not count towards the `signalLayerCount > 2` outer-layer surcharge
/// (`RouterSettings.java:299-334`).
#[test]
fn test_apply_board_specific_optimizations_skips_non_signal_layers() {
    let ls = LayerStructure::new(vec![
        Layer::new("top", true),
        Layer::new("gnd", false),
        Layer::new("pwr", false),
        Layer::new("bottom", true),
    ]);
    let mut s = RouterSettings::default();

    // W = 100, H = 200 → horizAdd = 0.5, vertAdd = 2.0, seed = (100 < 200) = true.
    let aspect = BoardAspectRatio::from_bounding_box(100.0, 200.0);
    s.apply_board_specific_optimizations(aspect, &ls);

    assert!(
        !s.preferred_direction_is_horizontal(0),
        "signal layer 0 flips the seed to false"
    );
    assert!(
        !s.preferred_direction_is_horizontal(1),
        "a plane layer does not flip"
    );
    assert!(
        !s.preferred_direction_is_horizontal(2),
        "a plane layer does not flip"
    );
    assert!(
        s.preferred_direction_is_horizontal(3),
        "the next signal layer flips back"
    );

    assert!(s.layer_active(0));
    assert!(
        !s.layer_active(1),
        "non-signal layers are forced non-routable"
    );
    assert!(!s.layer_active(2));
    assert!(s.layer_active(3));

    // Only 2 signal layers, so the `> 2` gate is closed even though the board has 4 layers.
    for i in 0..4 {
        assert_close(s.preferred_direction_trace_costs(i), 1.0, "pref");
    }
    assert_close(
        s.against_preferred_direction_trace_costs(0),
        1.0 + 2.0,
        "und[0]",
    );
    assert_close(
        s.against_preferred_direction_trace_costs(1),
        1.0 + 2.0,
        "und[1]",
    );
    assert_close(
        s.against_preferred_direction_trace_costs(2),
        1.0 + 2.0,
        "und[2]",
    );
    assert_close(
        s.against_preferred_direction_trace_costs(3),
        1.0 + 0.5,
        "und[3]",
    );
}

/// A user-supplied `preferred_direction_horizontal` is preserved, but the internal alternating
/// walk still advances — so the *following* layers keep the schedule they would otherwise have
/// (`RouterSettings.java:308-313`).
#[test]
fn test_apply_board_specific_optimizations_preserves_explicit_direction() {
    let ls = create_test_layer_structure(4);
    let mut s = RouterSettings::default();
    s.set_layer_count(4);
    s.set_preferred_direction_is_horizontal(0, false);

    // Seed = (200 < 100) = false, so without the override layer 0 would be `true`.
    s.apply_board_specific_optimizations(BoardAspectRatio::from_bounding_box(200.0, 100.0), &ls);

    assert!(
        !s.preferred_direction_is_horizontal(0),
        "the explicit value survives"
    );
    assert!(!s.preferred_direction_is_horizontal(1));
    assert!(s.preferred_direction_is_horizontal(2));
    assert!(!s.preferred_direction_is_horizontal(3));

    // The cost arrays still follow the *walk*, not the stored per-layer flag: layer 0's walk value
    // was `true`, so it took the horizontal add-on.
    assert_close(
        s.against_preferred_direction_trace_costs(0),
        1.0 + 2.0 + 0.2 * 4.0,
        "und[0] uses the walk value, not the overridden flag",
    );
}

/// `boardSpecificTraceCostsApplied` makes the derivation idempotent, and an explicit
/// `set_*_trace_costs` call latches it so a later invocation cannot overwrite the user's value
/// (`RouterSettings.java:297, 335, 630, 681`).
#[test]
fn test_apply_board_specific_optimizations_is_latched() {
    let ls = create_test_layer_structure(4);
    let aspect = BoardAspectRatio::from_bounding_box(200.0, 100.0);

    let mut s = RouterSettings::default();
    s.apply_board_specific_optimizations(aspect, &ls);
    let first = s.preferred_direction_trace_costs(0);

    // Re-running must not add the outer-layer surcharge a second time.
    s.apply_board_specific_optimizations(aspect, &ls);
    assert_close(
        s.preferred_direction_trace_costs(0),
        first,
        "pref[0] after a second run",
    );

    // An explicit cost keeps its value across a further run and is clamped to >= 0.1.
    s.set_preferred_direction_trace_costs(1, 0.01);
    assert_close(s.preferred_direction_trace_costs(1), 0.1, "clamped to 0.1");
    s.apply_board_specific_optimizations(aspect, &ls);
    assert_close(
        s.preferred_direction_trace_costs(1),
        0.1,
        "explicit cost survives",
    );

    // Changing the layer count clears the latch and re-derives.
    let ls6 = create_test_layer_structure(6);
    s.apply_board_specific_optimizations_if_needed(aspect, &ls6);
    assert_eq!(s.layer_count(), 6);
    assert_close(
        s.preferred_direction_trace_costs(1),
        1.0,
        "re-derived for the new stackup",
    );
}

/// `setLayerCount` resets the per-layer flags and refills both cost arrays with 1.0
/// (`RouterSettings.java:389-418`); the unset preferred direction then reads as `layer % 2 == 1`
/// (`RouterSettings.java:609-618`).
#[test]
fn test_router_settings_layer_defaults_and_bend_cost_clamp() {
    let mut s = RouterSettings::default();
    // No layers yet: every per-layer getter reports its out-of-range fallback.
    assert!(!s.layer_active(0));
    assert!(!s.preferred_direction_is_horizontal(0));
    assert_close(s.bend_cost(0), 0.0, "bend cost out of range");
    assert_close(
        s.preferred_direction_trace_costs(0),
        0.0,
        "pref out of range",
    );

    s.set_layer_count(4);
    assert_eq!(s.layer_count(), 4);
    for i in 0..4 {
        assert!(s.layer_active(i));
        assert_eq!(s.preferred_direction_is_horizontal(i), i % 2 == 1);
        assert_close(s.preferred_direction_trace_costs(i), 1.0, "pref seed");
        assert_close(
            s.against_preferred_direction_trace_costs(i),
            1.0,
            "und seed",
        );
    }

    s.set_bend_cost(0, 42.0);
    assert_close(s.bend_cost(0), MAX_BEND_COST, "clamped to MAX_BEND_COST");
    s.set_bend_cost(1, -3.0);
    assert_close(s.bend_cost(1), MIN_BEND_COST, "clamped to MIN_BEND_COST");

    // An unset per-layer bend cost falls back to the (clamped) scoring default.
    s.scoring.default_bend_cost = Some(50.0);
    assert_close(
        s.bend_cost(2),
        MAX_BEND_COST,
        "default bend cost is clamped on read",
    );
    s.scoring.default_bend_cost = None;
    assert_close(s.bend_cost(2), 0.0, "absent default bend cost reads as 0.0");

    s.set_layer_active(3, false);
    assert!(!s.layer_active(3));
}

/// `RouterSettings.validate()` — the only normalization step upstream performs
/// (`RouterSettings.java:932-965`).
#[test]
fn test_router_settings_validate() {
    let mut s = RouterSettings::default();
    s.max_passes = 0;
    s.trace_pull_tight_accuracy = 0;
    s.validate();
    assert_eq!(s.max_passes, i32::MAX, "0 means no limit");
    assert_eq!(
        s.trace_pull_tight_accuracy,
        DEFAULT_TRACE_PULL_TIGHT_ACCURACY
    );

    let mut s = RouterSettings::default();
    s.max_passes = -1;
    s.validate();
    assert_eq!(s.max_passes, MAX_PASSES_LIMIT);

    let mut s = RouterSettings::default();
    s.max_passes = 10_000;
    s.validate();
    assert_eq!(s.max_passes, MAX_PASSES_LIMIT);

    // normalizeMaxThreads: null/negative ⇒ cores-1, 0 ⇒ all cores, otherwise capped at the count.
    assert_eq!(normalize_max_threads(None), default_max_threads());
    assert_eq!(normalize_max_threads(Some(-1)), default_max_threads());
    assert!(normalize_max_threads(Some(0)) >= default_max_threads());
    assert_eq!(normalize_max_threads(Some(1)), 1);
    assert_eq!(
        normalize_max_threads(Some(i32::MAX)),
        normalize_max_threads(Some(0)),
        "capped at the available core count"
    );

    // The setter mirrors into the optimizer's copy.
    let mut s = RouterSettings::default();
    s.set_max_threads(Some(1));
    assert_eq!(s.max_threads, 1);
    assert_eq!(s.optimizer.max_threads, 1);
}
