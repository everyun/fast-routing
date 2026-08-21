//! BasicBoard master PCB data model.

use crate::area::{ConductionArea, ObstacleArea};
use crate::pin::Pin;
use crate::trace::PolylineTrace;
use crate::via::Via;
use fr_geometry::planar::{IntBox, IntPoint};

/// Master board representation containing all components, nets, and placed geometry.
#[derive(Debug, Clone, PartialEq)]
pub struct BasicBoard {
    pub name: String,
    pub bounding_box: IntBox,
    pub layer_count: usize,
    pub outline_points: Vec<IntPoint>,
    pub pins: Vec<Pin>,
    pub vias: Vec<Via>,
    pub traces: Vec<PolylineTrace>,
    pub obstacle_areas: Vec<ObstacleArea>,
    pub conduction_areas: Vec<ConductionArea>,
}

impl BasicBoard {
    pub fn new(name: &str, layer_count: usize, bounding_box: IntBox) -> Self {
        BasicBoard {
            name: name.to_string(),
            bounding_box,
            layer_count,
            outline_points: Vec::new(),
            pins: Vec::new(),
            vias: Vec::new(),
            traces: Vec::new(),
            obstacle_areas: Vec::new(),
            conduction_areas: Vec::new(),
        }
    }

    /// Total count of connectable pins.
    pub fn pin_count(&self) -> usize {
        self.pins.len()
    }

    /// Total count of placed vias.
    pub fn via_count(&self) -> usize {
        self.vias.len()
    }

    /// Total count of routed trace segments.
    pub fn trace_count(&self) -> usize {
        self.traces.len()
    }

    /// Total routed trace length across all layers.
    pub fn total_trace_length(&self) -> f64 {
        self.traces.iter().map(|t| t.length()).sum()
    }

    /// Finds all pins belonging to a net.
    pub fn get_pins_for_net(&self, net_no: i32) -> Vec<&Pin> {
        self.pins.iter().filter(|p| p.header.contains_net(net_no)).collect()
    }

    /// Finds all traces belonging to a net.
    pub fn get_traces_for_net(&self, net_no: i32) -> Vec<&PolylineTrace> {
        self.traces.iter().filter(|t| t.header.contains_net(net_no)).collect()
    }

    /// Finds all vias belonging to a net.
    pub fn get_vias_for_net(&self, net_no: i32) -> Vec<&Via> {
        self.vias.iter().filter(|v| v.header.contains_net(net_no)).collect()
    }

    /// Adds a trace to the board.
    pub fn insert_trace(&mut self, trace: PolylineTrace) {
        self.traces.push(trace);
    }

    /// Adds a via to the board.
    pub fn insert_via(&mut self, via: Via) {
        self.vias.push(via);
    }

    /// Removes all un-fixed traces and vias for a given net (rip-up).
    pub fn ripup_net(&mut self, net_no: i32) {
        self.traces.retain(|t| !t.header.contains_net(net_no) || t.header.is_fixed());
        self.vias.retain(|v| !v.header.contains_net(net_no) || v.header.is_fixed());
    }
}
