//! Snapshot and render model types for GUI visualization.
//!
//! Defined per `docs/tasks/GUI-CONTRACT.md`.

/// The complete board render model at one snapshot point.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RenderModel {
    /// Information for each board layer; index matches layer index.
    pub layers: Vec<LayerInfo>,
    /// Bounding box: `[xmin, ymin, xmax, ymax]` in board units.
    pub bbox: [f64; 4],
    /// Board units to millimeters conversion scale factor.
    pub resolution: f64,
    /// All routed trace segments.
    pub traces: Vec<TraceShape>,
    /// All vias.
    pub vias: Vec<ViaShape>,
    /// All component pins and pads.
    pub pads: Vec<PadShape>,
    /// Keepouts, copper pours, power planes, and board outlines.
    pub areas: Vec<AreaShape>,
    /// Unconnected ratsnest airlines.
    pub ratsnest: Vec<RatLine>,
    /// Router progress and board health metrics.
    pub stats: Stats,
}

/// Metadata and rendering color for a single layer.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerInfo {
    /// Layer name.
    pub name: String,
    /// Whether this is an electrical signal layer.
    pub is_signal: bool,
    /// RGBA color `[r, g, b, a]` normalized to `0.0..=1.0`.
    pub color: [f32; 4],
}

/// A routed trace polyline.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceShape {
    /// Layer index.
    pub layer: u16,
    /// Net number.
    pub net: i32,
    /// Half of the trace width in board units.
    pub half_width: f64,
    /// Centerline vertices `[x, y]` in board units.
    pub points: Vec<[f64; 2]>,
    /// Fix state of the trace.
    pub fixed: FixState,
}

/// A through or blind/buried via.
#[derive(Debug, Clone, PartialEq)]
pub struct ViaShape {
    /// Starting layer index.
    pub layer_from: u16,
    /// Ending layer index.
    pub layer_to: u16,
    /// Net number.
    pub net: i32,
    /// Via center `[x, y]` in board units.
    pub center: [f64; 2],
    /// Via outer radius in board units.
    pub radius: f64,
    /// Fix state of the via.
    pub fixed: FixState,
}

/// A component pad or pin shape.
#[derive(Debug, Clone, PartialEq)]
pub struct PadShape {
    /// Layer index.
    pub layer: u16,
    /// Net number.
    pub net: i32,
    /// Boundary polygon vertices `[x, y]` in board units.
    pub polygon: Vec<[f64; 2]>,
    /// Whether this is a surface mount device pad.
    pub is_smd: bool,
    /// Owning component identifier.
    pub component: String,
    /// Pin name or number within the component.
    pub pin: String,
}

/// A polygonal area (plane, keepout, outline).
#[derive(Debug, Clone, PartialEq)]
pub struct AreaShape {
    /// Layer index.
    pub layer: u16,
    /// Net number.
    pub net: i32,
    /// Boundary polygon vertices `[x, y]` in board units.
    pub polygon: Vec<[f64; 2]>,
    /// Kind of area geometry.
    pub kind: AreaKind,
}

/// What kind of area geometry an [`AreaShape`] represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AreaKind {
    /// Power or ground copper plane / pour.
    Plane,
    /// Routing keepout area.
    Keepout,
    /// Board edge outline.
    Outline,
    /// Other area geometry.
    Other,
}

/// A ratsnest airline connecting two unconnected items on a net.
#[derive(Debug, Clone, PartialEq)]
pub struct RatLine {
    /// Net number.
    pub net: i32,
    /// Starting point `[x, y]` in board units.
    pub from: [f64; 2],
    /// Ending point `[x, y]` in board units.
    pub to: [f64; 2],
    /// Layer at starting end.
    pub layer_from: u16,
    /// Layer at ending end.
    pub layer_to: u16,
}

/// Fix state of a trace or via.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FixState {
    /// Freely movable by the autorouter.
    #[default]
    Unfixed,
    /// Fixed during shove operations.
    ShoveFixed,
    /// Fixed by the user.
    UserFixed,
    /// Fixed by the system.
    SystemFixed,
}

/// Routing statistics attached to each snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Stats {
    /// Number of incomplete connections.
    pub incomplete: u32,
    /// Number of clearance violations.
    pub violations: u32,
    /// Current pass number (0 for pre-routing / fanout).
    pub pass: u32,
    /// Elapsed wall time in seconds.
    pub elapsed_s: f64,
    /// Current normalized routing score.
    pub score: f64,
    /// Total items currently on the board.
    pub items: u32,
}
