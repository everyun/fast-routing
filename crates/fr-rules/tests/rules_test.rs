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
    assert_eq!(ls.get_signal_layer(1).map(|l| l.name.as_str()), Some("inner_1"));
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
        assert_eq!(cm.get_value(1, 1, layer, true), 120 + CLEARANCE_SAFETY_MARGIN);
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

    let ps1 = Padstack::new("ps1", 1, vec![Some(PadShape::Circle { radius: 50 })], true, false);
    let via1 = ViaInfo::new("via1", ps1, 1, true);

    let ps2 = Padstack::new("ps2", 2, vec![Some(PadShape::Circle { radius: 80 })], true, false);
    let via2 = ViaInfo::new("via2", ps2, 1, false);

    assert!(via_infos.add(via1));
    assert!(via_infos.add(via2));
    assert!(!via_infos.add(ViaInfo::new("via1", Padstack::new("ps_dup", 3, vec![], true, false), 1, true)));

    assert_eq!(via_infos.count(), 2);
    assert!(via_infos.name_exists("via1"));
    assert!(via_infos.name_exists("via2"));
    assert!(!via_infos.name_exists("via3"));

    assert_eq!(via_infos.get(0).map(|v| v.name()), Some("via1"));
    assert_eq!(via_infos.get_by_name("via2").map(|v| v.attach_smd_allowed()), Some(false));

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

    assert_eq!(nets.get_by_name_and_subnet("gnd", 1).map(|n| n.net_number), Some(1));
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
    assert_eq!(rules.get_trace_angle_restriction(), AngleRestriction::NinetyDegree);

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
    let ps1 = Padstack::new("p1", 1, vec![Some(PadShape::Circle { radius: 10 })], true, false);
    let ps2 = Padstack::new("p2", 2, vec![Some(PadShape::Circle { radius: 20 })], true, false);
    vr.append_via(ViaInfo::new("v1", ps1, 1, true));
    vr.append_via(ViaInfo::new("v2", ps2, 1, true));

    assert_eq!(vr.get_via(0).unwrap().name(), "v1");
    assert_eq!(vr.get_via(1).unwrap().name(), "v2");

    assert!(vr.swap_indices(0, 1));
    assert_eq!(vr.get_via(0).unwrap().name(), "v2");
    assert_eq!(vr.get_via(1).unwrap().name(), "v1");
}

