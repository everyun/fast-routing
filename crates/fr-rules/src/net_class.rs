//! Describes routing rules for individual nets.
//! Ported from `app.freerouting.rules.NetClass`.

use crate::default_item_clearance_classes::DefaultItemClearanceClasses;
use crate::layer::LayerStructure;
use crate::via_rule::ViaRule;

/// Describes routing rules for individual nets.
#[derive(Debug, Clone, PartialEq)]
pub struct NetClass {
    /// Name of this net class.
    pub name: String,
    trace_half_width_arr: Vec<i32>,
    active_routing_layer_arr: Vec<bool>,
    /// The default clearance classes of item types for Specctra DSN net classes.
    pub default_item_clearance_classes: DefaultItemClearanceClasses,
    /// If true, this net class is skipped by the autorouter.
    pub is_ignored_by_autorouter: bool,
    via_rule: Option<ViaRule>,
    trace_clearance_class: usize,
    /// If true, traces and vias of this net class are fixed (cannot be shoved).
    pub shove_fixed: bool,
    /// If true, traces of this net class are pulled tight.
    pub pull_tight: bool,
    /// If true, cycle removal ignores cycles with conduction areas.
    pub ignore_cycles_with_areas: bool,
    /// Minimum trace length (<= 0 means unrestricted).
    pub minimum_trace_length: f64,
    /// Maximum trace length (<= 0 means unrestricted).
    pub maximum_trace_length: f64,
}

impl NetClass {
    /// Creates a new `NetClass`.
    pub fn new(
        name: impl Into<String>,
        layer_structure: &LayerStructure,
        ignored_by_autorouter: bool,
    ) -> Self {
        let layer_count = layer_structure.len();
        let active_layers = layer_structure.arr.iter().map(|l| l.is_signal).collect();
        NetClass {
            name: name.into(),
            trace_half_width_arr: vec![0; layer_count],
            active_routing_layer_arr: active_layers,
            default_item_clearance_classes: DefaultItemClearanceClasses::new(),
            is_ignored_by_autorouter: ignored_by_autorouter,
            via_rule: None,
            trace_clearance_class: 1,
            shove_fixed: false,
            pull_tight: true,
            ignore_cycles_with_areas: false,
            minimum_trace_length: 0.0,
            maximum_trace_length: 0.0,
        }
    }

    /// Sets the trace half-width used for routing on all layers.
    pub fn set_trace_half_width(&mut self, value: i32) {
        self.trace_half_width_arr.fill(value);
    }

    /// Sets the trace half-width used for routing on a specific layer.
    pub fn set_trace_half_width_on_layer(&mut self, layer: usize, value: i32) {
        if layer < self.trace_half_width_arr.len() {
            self.trace_half_width_arr[layer] = value;
        }
    }

    /// Sets the trace half-width used for routing on all inner layers.
    pub fn set_trace_half_width_on_inner(&mut self, value: i32) {
        let len = self.trace_half_width_arr.len();
        if len > 2 {
            for i in 1..(len - 1) {
                self.trace_half_width_arr[i] = value;
            }
        }
    }

    /// Returns the number of layers in this net class.
    pub fn layer_count(&self) -> usize {
        self.trace_half_width_arr.len()
    }

    /// Gets the trace half-width used for routing on the input layer.
    pub fn get_trace_half_width(&self, layer: usize) -> i32 {
        self.trace_half_width_arr.get(layer).copied().unwrap_or(0)
    }

    /// Gets the clearance class used for routing traces with this net class.
    pub fn get_trace_clearance_class(&self) -> usize {
        self.trace_clearance_class
    }

    /// Sets the clearance class used for routing traces with this net class.
    pub fn set_trace_clearance_class(&mut self, clearance_class: usize) {
        self.trace_clearance_class = clearance_class;
    }

    /// Gets the via rule of this net class.
    pub fn get_via_rule(&self) -> Option<&ViaRule> {
        self.via_rule.as_ref()
    }

    /// Sets the via rule of this net class.
    pub fn set_via_rule(&mut self, via_rule: Option<ViaRule>) {
        self.via_rule = via_rule;
    }

    /// Returns whether the layer with the given index is active for routing.
    pub fn is_active_routing_layer(&self, layer: usize) -> bool {
        self.active_routing_layer_arr
            .get(layer)
            .copied()
            .unwrap_or(false)
    }

    /// Sets whether the layer with the given index is active for routing.
    pub fn set_active_routing_layer(&mut self, layer: usize, active: bool) {
        if layer < self.active_routing_layer_arr.len() {
            self.active_routing_layer_arr[layer] = active;
        }
    }

    /// Activates or deactivates all layers for routing.
    pub fn set_all_layers_active(&mut self, value: bool) {
        self.active_routing_layer_arr.fill(value);
    }

    /// Activates or deactivates all inner layers for routing.
    pub fn set_all_inner_layers_active(&mut self, value: bool) {
        let len = self.active_routing_layer_arr.len();
        if len > 2 {
            for i in 1..(len - 1) {
                self.active_routing_layer_arr[i] = value;
            }
        }
    }

    /// Returns true if the trace width of this class is not equal on all signal layers.
    pub fn trace_width_is_layer_dependent(&self, layer_structure: &LayerStructure) -> bool {
        if self.trace_half_width_arr.is_empty() {
            return false;
        }
        let compare_value = self.trace_half_width_arr[0];
        for i in 1..self.trace_half_width_arr.len() {
            if i < layer_structure.arr.len() && layer_structure.arr[i].is_signal {
                if self.trace_half_width_arr[i] != compare_value {
                    return true;
                }
            }
        }
        false
    }

    /// Returns true if the trace width of this class is not equal on all inner signal layers.
    pub fn trace_width_is_inner_layer_dependent(&self, layer_structure: &LayerStructure) -> bool {
        let len = self.trace_half_width_arr.len();
        if len <= 3 {
            return false;
        }

        let mut first_inner = 1;
        while first_inner < len && !layer_structure.arr.get(first_inner).map_or(false, |l| l.is_signal) {
            first_inner += 1;
        }
        if first_inner >= len.saturating_sub(1) {
            return false;
        }

        let compare_width = self.trace_half_width_arr[first_inner];
        for i in (first_inner + 1)..(len - 1) {
            if layer_structure.arr.get(i).map_or(false, |l| l.is_signal) {
                if self.trace_half_width_arr[i] != compare_width {
                    return true;
                }
            }
        }
        false
    }
}
