//! Master container aggregating clearance matrix, net classes, nets, and via rules.
//! Ported from `app.freerouting.rules.BoardRules`.

use crate::clearance_matrix::ClearanceMatrix;
use crate::default_item_clearance_classes::ItemClass;
use crate::layer::{AngleRestriction, LayerStructure};
use crate::net_class::NetClass;
use crate::net_classes::NetClasses;
use crate::nets::Nets;
use crate::via_infos::ViaInfos;
use crate::via_rule::ViaRule;

/// Contains the rules and constraints required for items to be inserted into a routing board.
#[derive(Debug, Clone, PartialEq)]
pub struct BoardRules {
    /// The matrix describing the spacing restrictions between item clearance classes.
    pub clearance_matrix: ClearanceMatrix,
    /// Describes the electrical nets on the board.
    pub nets: Nets,
    /// Collection of via padstack definitions.
    pub via_infos: ViaInfos,
    /// Available via rules for routing.
    pub via_rules: Vec<ViaRule>,
    /// Available net classes.
    pub net_classes: NetClasses,
    /// Structure of board layers.
    pub layer_structure: LayerStructure,

    trace_angle_restriction: AngleRestriction,
    ignore_conduction: bool,
    min_trace_half_width: i32,
    max_trace_half_width: i32,
    pin_edge_to_turn_dist: f64,
    use_slow_autoroute_algorithm: bool,
    hole_clearance: i32,
}

impl BoardRules {
    /// Creates a new `BoardRules` container.
    pub fn new(layer_structure: LayerStructure, clearance_matrix: ClearanceMatrix) -> Self {
        let mut rules = BoardRules {
            clearance_matrix,
            nets: Nets::new(),
            via_infos: ViaInfos::new(),
            via_rules: Vec::new(),
            net_classes: NetClasses::new(),
            layer_structure,
            trace_angle_restriction: AngleRestriction::FortyFiveDegree,
            ignore_conduction: true,
            min_trace_half_width: 100000,
            max_trace_half_width: 100,
            pin_edge_to_turn_dist: 0.0,
            use_slow_autoroute_algorithm: false,
            hole_clearance: 0,
        };
        rules.create_default_net_class();
        rules
    }

    /// Gets the default item clearance class index (1).
    pub const fn default_clearance_class() -> usize {
        1
    }

    /// Returns the clearance class used for items with no clearances (0).
    pub const fn clearance_class_none() -> usize {
        0
    }

    /// Returns the trace half-width used for routing with the given net on `layer`.
    pub fn get_trace_half_width(&self, net_number: i32, layer: usize) -> i32 {
        if let Some(current_net) = self.nets.get(net_number) {
            if let Some(nc) = self.net_classes.get(current_net.net_class_index) {
                return nc.get_trace_half_width(layer);
            }
        }
        self.get_default_trace_half_width(layer)
    }

    /// Returns true if the trace widths used for routing for `net_number` are equal on all layers.
    pub fn trace_widths_are_layer_dependent(&self, net_number: i32) -> bool {
        let compare = self.get_trace_half_width(net_number, 0);
        for i in 1..self.layer_structure.len() {
            if self.get_trace_half_width(net_number, i) != compare {
                return true;
            }
        }
        false
    }

    /// Returns the smallest of all default trace half-widths.
    pub fn get_min_trace_half_width(&self) -> i32 {
        self.min_trace_half_width
    }

    /// Returns the biggest of all default trace half-widths.
    pub fn get_max_trace_half_width(&self) -> i32 {
        self.max_trace_half_width
    }

    /// Returns the clearance around drilled holes.
    pub fn get_hole_clearance(&self) -> i32 {
        self.hole_clearance
    }

    /// Sets the clearance around drilled holes.
    pub fn set_hole_clearance(&mut self, value: i32) {
        self.hole_clearance = value.max(0);
    }

    /// Changes the default trace half-width used for routing on `layer`.
    pub fn set_default_trace_half_width(&mut self, layer: usize, value: i32) {
        if let Some(default_nc) = self.net_classes.get_mut(0) {
            default_nc.set_trace_half_width_on_layer(layer, value);
        }
        self.min_trace_half_width = self.min_trace_half_width.min(value);
        self.max_trace_half_width = self.max_trace_half_width.max(value);
    }

    /// Returns the default trace half-width used on `layer`.
    pub fn get_default_trace_half_width(&self, layer: usize) -> i32 {
        self.get_default_net_class().get_trace_half_width(layer)
    }

    /// Changes the default trace half-width on all layers to `value`.
    pub fn set_default_trace_half_widths(&mut self, value: i32) {
        if value <= 0 {
            return;
        }
        if let Some(default_nc) = self.net_classes.get_mut(0) {
            default_nc.set_trace_half_width(value);
        }
        self.min_trace_half_width = self.min_trace_half_width.min(value);
        self.max_trace_half_width = self.max_trace_half_width.max(value);
    }

    /// Returns the default net class used for all nets without a specialized rule.
    pub fn get_default_net_class(&self) -> &NetClass {
        self.net_classes.get(0).expect("default net class must exist")
    }

    /// Returns a mutable reference to the default net class.
    pub fn get_default_net_class_mut(&mut self) -> &mut NetClass {
        self.net_classes.get_mut(0).expect("default net class must exist")
    }

    /// Creates the initial default net class if net classes are empty.
    pub fn create_default_net_class(&mut self) {
        if self.net_classes.is_empty() {
            let idx = self.net_classes.append("default", &self.layer_structure, false);
            let default_nc = self.net_classes.get_mut(idx).unwrap();
            let default_trace_half_width = 1500;
            default_nc.set_trace_half_width(default_trace_half_width);
            default_nc.set_trace_clearance_class(1);
            self.min_trace_half_width = default_trace_half_width;
            self.max_trace_half_width = default_trace_half_width;
        }
    }

    /// Creates an empty new net rule with an internally created name.
    pub fn get_new_net_class(&mut self) -> usize {
        let idx = self.net_classes.append_with_generated_name(&self.layer_structure);
        let default_cl = self.get_default_net_class().get_trace_clearance_class();
        let default_via_rule = self.get_default_via_rule().cloned();
        let default_width = self.get_default_net_class().get_trace_half_width(0);
        let nc = self.net_classes.get_mut(idx).unwrap();
        nc.set_trace_clearance_class(default_cl);
        nc.set_via_rule(default_via_rule);
        nc.set_trace_half_width(default_width);
        idx
    }

    /// Creates an empty new net rule with the specified name.
    pub fn get_new_net_class_with_name(&mut self, name: &str) -> usize {
        let idx = self.net_classes.append(name, &self.layer_structure, false);
        let default_cl = self.get_default_net_class().get_trace_clearance_class();
        let default_via_rule = self.get_default_via_rule().cloned();
        let default_width = self.get_default_net_class().get_trace_half_width(0);
        let nc = self.net_classes.get_mut(idx).unwrap();
        nc.set_trace_clearance_class(default_cl);
        nc.set_via_rule(default_via_rule);
        nc.set_trace_half_width(default_width);
        idx
    }

    /// Appends a new net class initialized with default properties.
    pub fn append_net_class(&mut self) -> usize {
        let idx = self.net_classes.append_with_generated_name(&self.layer_structure);
        let default_cl = self.get_default_net_class().get_trace_clearance_class();
        let default_via_rule = self.get_default_net_class().get_via_rule().cloned();
        let default_width = self.get_default_net_class().get_trace_half_width(0);
        let nc = self.net_classes.get_mut(idx).unwrap();
        nc.set_via_rule(default_via_rule);
        nc.set_trace_half_width(default_width);
        nc.set_trace_clearance_class(default_cl);
        idx
    }

    /// Appends a new net class with `name` or returns the existing class index.
    pub fn append_net_class_with_name(&mut self, name: &str) -> usize {
        if let Some(pos) = self.net_classes.iter().position(|c| c.name == name) {
            return pos;
        }
        let default_item_cl = self.get_default_net_class().default_item_clearance_classes.clone();
        let default_via_rule = self.get_default_net_class().get_via_rule().cloned();
        let default_width = self.get_default_net_class().get_trace_half_width(0);
        let default_cl = self.get_default_net_class().get_trace_clearance_class();

        let idx = self.net_classes.append(name, &self.layer_structure, false);
        let nc = self.net_classes.get_mut(idx).unwrap();
        nc.default_item_clearance_classes = default_item_cl;
        nc.set_via_rule(default_via_rule);
        nc.set_trace_half_width(default_width);
        nc.set_trace_clearance_class(default_cl);
        idx
    }

    /// Returns the default via rule for routing, or `None` if no via rule exists.
    pub fn get_default_via_rule(&self) -> Option<&ViaRule> {
        self.via_rules.first()
    }

    /// Returns the via rule with the given name, or `None` if not found.
    pub fn get_via_rule(&self, name: &str) -> Option<&ViaRule> {
        self.via_rules.iter().find(|vr| vr.name == name)
    }

    /// Creates a default via rule for `net_class_index` with name `name`.
    /// If more than one via info with the same layer range is found, only the via info
    /// with the smallest pad size is inserted.
    pub fn create_default_via_rule(&mut self, net_class_index: usize, name: &str) {
        if self.via_infos.is_empty() {
            return;
        }

        let default_via_cl_class = if let Some(nc) = self.net_classes.get(net_class_index) {
            nc.default_item_clearance_classes.get(ItemClass::Via) as usize
        } else {
            1
        };

        let mut default_rule = ViaRule::new(name);

        for i in 0..self.via_infos.count() {
            let curr_via_info = self.via_infos.get(i).unwrap();
            if curr_via_info.clearance_class() == default_via_cl_class {
                let curr_padstack = curr_via_info.padstack();
                let curr_from_layer = curr_padstack.from_layer();
                let curr_to_layer = curr_padstack.to_layer();

                let existing_via = default_rule.get_layer_range(curr_from_layer, curr_to_layer).cloned();
                if let Some(existing) = existing_via {
                    let new_shape = curr_padstack.get_shape(curr_from_layer);
                    let existing_shape = existing.padstack().get_shape(curr_from_layer);
                    let new_width = new_shape.map_or(0.0, |s| s.max_width());
                    let existing_width = existing_shape.map_or(0.0, |s| s.max_width());

                    if new_width < existing_width {
                        default_rule.remove_via(existing.name());
                        default_rule.append_via(curr_via_info.clone());
                    }
                } else {
                    default_rule.append_via(curr_via_info.clone());
                }
            }
        }

        if let Some(nc) = self.net_classes.get_mut(net_class_index) {
            nc.set_via_rule(Some(default_rule.clone()));
        }
        self.via_rules.push(default_rule);
    }

    /// Returns the maximum diameter of the default via on its first and last layer.
    pub fn get_default_via_diameter(&self) -> f64 {
        let default_via_rule = match self.get_default_via_rule() {
            Some(r) => r,
            None => return 0.0,
        };
        if default_via_rule.is_empty() {
            return 0.0;
        }

        let via_padstack = default_via_rule.get_via(0).unwrap().padstack();
        let from_shape = via_padstack.get_shape(via_padstack.from_layer());
        let to_shape = via_padstack.get_shape(via_padstack.to_layer());
        let width_from = from_shape.map_or(0.0, |s| s.max_width());
        let width_to = to_shape.map_or(0.0, |s| s.max_width());
        width_from.max(width_to)
    }

    /// Changes the clearance class index of all objects from `from_no` to `to_no`.
    pub fn change_clearance_class_no(&mut self, from_no: usize, to_no: usize) {
        for nc in self.net_classes.iter_mut() {
            if nc.get_trace_clearance_class() == from_no {
                nc.set_trace_clearance_class(to_no);
            }
            for item_class in ItemClass::ALL {
                if nc.default_item_clearance_classes.get(item_class) as usize == from_no {
                    nc.default_item_clearance_classes.set(item_class, to_no as i32);
                }
            }
        }

        for via in self.via_infos.iter_mut() {
            if via.clearance_class() == from_no {
                via.set_clearance_class(to_no);
            }
        }
    }

    /// Removes the clearance class with index `index`.
    /// Returns false if it is still in use by any net class or via info.
    pub fn remove_clearance_class(&mut self, index: usize) -> bool {
        for nc in self.net_classes.iter() {
            if nc.get_trace_clearance_class() == index {
                return false;
            }
            for item_class in ItemClass::ALL {
                if nc.default_item_clearance_classes.get(item_class) as usize == index {
                    return false;
                }
            }
        }

        for via in self.via_infos.iter() {
            if via.clearance_class() == index {
                return false;
            }
        }

        // Shift down higher indices
        for nc in self.net_classes.iter_mut() {
            if nc.get_trace_clearance_class() > index {
                nc.set_trace_clearance_class(nc.get_trace_clearance_class() - 1);
            }
            for item_class in ItemClass::ALL {
                let curr_no = nc.default_item_clearance_classes.get(item_class) as usize;
                if curr_no > index {
                    nc.default_item_clearance_classes.set(item_class, (curr_no - 1) as i32);
                }
            }
        }

        for via in self.via_infos.iter_mut() {
            if via.clearance_class() > index {
                via.set_clearance_class(via.clearance_class() - 1);
            }
        }

        self.clearance_matrix.remove_class(index);
        true
    }

    /// Returns the minimum distance between pin border and first turn of a trace.
    pub fn get_pin_edge_to_turn_dist(&self) -> f64 {
        self.pin_edge_to_turn_dist
    }

    /// Sets the minimum distance between pin border and first turn of a trace.
    pub fn set_pin_edge_to_turn_dist(&mut self, value: f64) {
        self.pin_edge_to_turn_dist = value;
    }

    /// Returns whether the router ignores conduction areas.
    pub fn get_ignore_conduction(&self) -> bool {
        self.ignore_conduction
    }

    /// Sets whether the router ignores conduction areas.
    pub fn set_ignore_conduction(&mut self, value: bool) {
        self.ignore_conduction = value;
    }

    /// Gets the angle restriction for traces.
    pub fn get_trace_angle_restriction(&self) -> AngleRestriction {
        self.trace_angle_restriction
    }

    /// Sets the angle restriction for traces.
    pub fn set_trace_angle_restriction(&mut self, angle_restriction: AngleRestriction) {
        self.trace_angle_restriction = angle_restriction;
    }

    /// Returns whether the slow autoroute algorithm is used.
    pub fn get_use_slow_autoroute_algorithm(&self) -> bool {
        self.use_slow_autoroute_algorithm
    }

    /// Sets whether the slow autoroute algorithm is used.
    pub fn set_use_slow_autoroute_algorithm(&mut self, value: bool) {
        self.use_slow_autoroute_algorithm = value;
    }
}
