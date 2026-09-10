//! Rendering model and contract types for fr-gui.
//!
//! Matches `crates/fr-core/src/snapshot.rs` from `GUI-CONTRACT.md`.

#![allow(dead_code)]

#[derive(Clone, Debug)]
pub struct RenderModel {
    /// Layer table; index corresponds to layer index.
    pub layers: Vec<LayerInfo>,
    /// Bounding box: [xmin, ymin, xmax, ymax] in board units.
    pub bbox: [f64; 4],
    /// Multiplier to convert board units to millimetres: `mm = board_unit * resolution`.
    pub resolution: f64,
    /// Routed wire traces.
    pub traces: Vec<TraceShape>,
    /// Vias between layers.
    pub vias: Vec<ViaShape>,
    /// Component pads / pins.
    pub pads: Vec<PadShape>,
    /// Copper planes, keepouts, and board outlines.
    pub areas: Vec<AreaShape>,
    /// Unrouted connection flylines (ratsnest).
    pub ratsnest: Vec<RatLine>,
    /// Routing progress and statistics.
    pub stats: Stats,
}

#[derive(Clone, Debug)]
pub struct LayerInfo {
    pub name: String,
    pub is_signal: bool,
    pub color: [f32; 4],
}

#[derive(Clone, Debug)]
pub struct TraceShape {
    pub layer: u16,
    pub net: i32,
    pub half_width: f64,
    pub points: Vec<[f64; 2]>,
    pub fixed: FixState,
}

#[derive(Clone, Debug)]
pub struct ViaShape {
    pub layer_from: u16,
    pub layer_to: u16,
    pub net: i32,
    pub center: [f64; 2],
    pub radius: f64,
    pub fixed: FixState,
}

#[derive(Clone, Debug)]
pub struct PadShape {
    pub layer: u16,
    pub net: i32,
    pub polygon: Vec<[f64; 2]>,
    pub is_smd: bool,
    pub component: String,
    pub pin: String,
}

#[derive(Clone, Debug)]
pub struct AreaShape {
    pub layer: u16,
    pub net: i32,
    pub polygon: Vec<[f64; 2]>,
    pub kind: AreaKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AreaKind {
    Plane,
    Keepout,
    Outline,
    Other,
}

#[derive(Clone, Debug)]
pub struct RatLine {
    pub net: i32,
    pub from: [f64; 2],
    pub to: [f64; 2],
    pub layer_from: u16,
    pub layer_to: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixState {
    Unfixed,
    ShoveFixed,
    UserFixed,
    SystemFixed,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub incomplete: u32,
    pub violations: u32,
    pub pass: u32,
    pub elapsed_s: f64,
    pub score: f64,
    pub items: u32,
}
