//! Structured AST representation of a Specctra DSN file.

/// A 2D point in user coordinates (e.g. mm or mil).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DsnPoint {
    pub x: f64,
    pub y: f64,
}

/// A placed component instance on the board.
#[derive(Debug, Clone, PartialEq)]
pub struct DsnComponent {
    pub name: String,
    pub package_name: String,
    pub x: f64,
    pub y: f64,
    pub side: String, // "front" or "back"
    pub rotation: f64,
}

/// A pin in a package footprint.
#[derive(Debug, Clone, PartialEq)]
pub struct DsnPin {
    pub pin_id: String,
    pub padstack_name: String,
    pub x: f64,
    pub y: f64,
    pub rotation: f64,
}

/// A package footprint definition.
#[derive(Debug, Clone, PartialEq)]
pub struct DsnPackage {
    pub name: String,
    pub pins: Vec<DsnPin>,
    pub outlines: Vec<Vec<DsnPoint>>,
}

/// A padstack shape definition per layer.
#[derive(Debug, Clone, PartialEq)]
pub struct DsnPadstackShape {
    pub layer: String,
    pub shape_type: String, // "circle", "rect", "polygon"
    pub dimensions: Vec<f64>,
    pub points: Vec<DsnPoint>,
}

/// A padstack definition.
#[derive(Debug, Clone, PartialEq)]
pub struct DsnPadstack {
    pub name: String,
    pub shapes: Vec<DsnPadstackShape>,
}

/// A pin connection in a net.
#[derive(Debug, Clone, PartialEq)]
pub struct DsnNetPin {
    pub component_name: String,
    pub pin_id: String,
}

/// A net definition.
#[derive(Debug, Clone, PartialEq)]
pub struct DsnNet {
    pub name: String,
    pub pins: Vec<DsnNetPin>,
    pub class_name: Option<String>,
    pub is_plane: bool,
}

/// A net class definition.
#[derive(Debug, Clone, PartialEq)]
pub struct DsnClass {
    pub name: String,
    pub net_names: Vec<String>,
    pub width: Option<f64>,
    pub clearance: Option<f64>,
    pub via_rule: Option<String>,
}

/// A PCB layer definition.
#[derive(Debug, Clone, PartialEq)]
pub struct DsnLayer {
    pub name: String,
    pub layer_type: String, // "signal", "power", "mixed"
    pub preferred_direction: Option<String>,
}

/// A wire / trace route in the session or pre-routed DSN.
#[derive(Debug, Clone, PartialEq)]
pub struct DsnWire {
    pub net_name: String,
    pub layer: String,
    pub width: f64,
    pub points: Vec<DsnPoint>,
    pub fixed_type: Option<String>,
}

/// A via instance.
#[derive(Debug, Clone, PartialEq)]
pub struct DsnVia {
    pub net_name: String,
    pub padstack_name: String,
    pub x: f64,
    pub y: f64,
    pub fixed_type: Option<String>,
}

/// Complete parsed Specctra DSN document.
#[derive(Debug, Clone, PartialEq)]
pub struct DsnDocument {
    pub pcb_name: String,
    pub unit: String,
    pub resolution: f64,
    pub layers: Vec<DsnLayer>,
    pub boundary_points: Vec<DsnPoint>,
    pub components: Vec<DsnComponent>,
    pub packages: Vec<DsnPackage>,
    pub padstacks: Vec<DsnPadstack>,
    pub nets: Vec<DsnNet>,
    pub classes: Vec<DsnClass>,
    pub wires: Vec<DsnWire>,
    pub vias: Vec<DsnVia>,
}

impl DsnDocument {
    pub fn new(pcb_name: &str) -> Self {
        DsnDocument {
            pcb_name: pcb_name.to_string(),
            unit: "mm".to_string(),
            resolution: 1000.0,
            layers: Vec::new(),
            boundary_points: Vec::new(),
            components: Vec::new(),
            packages: Vec::new(),
            padstacks: Vec::new(),
            nets: Vec::new(),
            classes: Vec::new(),
            wires: Vec::new(),
            vias: Vec::new(),
        }
    }
}
