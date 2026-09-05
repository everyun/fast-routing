//! [`DsnAst`] → [`fr_board::Board`] + [`fr_rules::BoardRules`].
//!
//! This is the half of the importer that decides what the router sees: the scale factor every
//! coordinate is expressed in, the clearance matrix every item indexes into, the placement math
//! that puts a back-side pad where it belongs, and the fixed states that decide what may be
//! ripped up. `dsn-board-semantics.md` §3, §6-§11 is the contract; the section numbers are cited
//! at each step.
//!
//! # Order is the specification
//!
//! Clearance classes are stored on items **by index**, so the order in which classes are appended
//! is part of the output. The build therefore runs the upstream phases in the upstream order —
//! structure rules, board-edge override, library, nets, via infos, via rules, net classes, class
//! pairs, components, wiring — and appends classes exactly where upstream appends them. The same
//! goes for net numbers (creation order) and item ids (insertion order).
//!
//! Board items are accumulated in [`Builder::items`] and inserted at the very end, once the
//! clearance matrix has stopped growing and its final class count is known. The insertion order
//! is the accumulation order, so item ids match upstream's.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use fr_board::item::{
    ComponentOutline, ConductionArea, FixedState, Item, ItemKind, LayerRange, ObstacleArea,
    ObstacleKind, Pin, PlacedArea, Via,
};
use fr_board::{AreaShape, Board, NetNos};
use fr_geometry::planar::{
    split_to_convex, Circle, FloatPoint, IntBox, IntOctagon, IntPoint, IntVector, Line, Point,
    Polyline, Simplex, Vector,
};
use fr_rules::clearance_class_names::{
    contains_wire_clearance_pair, net_class_item_clearance_class_name, network_class_pair,
    structure_class_pair, ClassPairResolution, RESERVED_ITEM_CLASS_NAMES,
};
use fr_rules::{BoardRules, ClearanceMatrix, ItemClass, Nets, PadShape, ViaInfo, ViaRule};

use super::ast::*;
use super::jround::JavaRound;
use super::transform::{derive_scale_factor, CoordinateTransform, ScaleFactor};

/// Post-load overrides `HeadlessBoardManager` applies right after the board is constructed.
///
/// They are part of the import as the released jar performs it — `createBoard` calls both before
/// the network scope is read, so the `board_edge` clearance class lands *before* every net class
/// in the matrix (`HeadlessBoardManager.java:334-343`). The defaults are the shipped ones.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImportOptions {
    /// `router.copper_to_edge_clearance_um`, default 500 µm.
    pub copper_to_edge_clearance_um: Option<f64>,
    /// `router.hole_clearance_um`, default 0 µm.
    pub hole_clearance_um: Option<f64>,
    /// Override clearance for inner board cutouts (Edge.Cuts holes) in µm.
    /// When `None` (default), inner cutouts map to the `board_edge` clearance
    /// class (inheriting the same clearance rule as the outer board boundary).
    pub cutout_clearance_um: Option<f64>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        ImportOptions {
            copper_to_edge_clearance_um: Some(500.0),
            hole_clearance_um: Some(0.0),
            cutout_clearance_um: None,
        }
    }
}

/// A convex pad shape, in board units, relative to the pin or via centre.
///
/// Upstream's `ConvexShape` covers `IntBox`, `IntOctagon`, `Simplex` **and** `Circle`; neither
/// `fr_rules::PadShape` (no simplex) nor `fr_board::TileShape` (no circle) covers all four, so the
/// importer keeps its own union as the authoritative library geometry and lowers it into whichever
/// of the two a consumer needs.
#[derive(Debug, Clone, PartialEq)]
pub enum PadGeometry {
    /// Axis-parallel rectangle.
    Box(IntBox),
    /// 45°-bounded octagon.
    Octagon(IntOctagon),
    /// The general convex case.
    Simplex(Simplex),
    /// A round pad; the DSN number is a diameter and is halved on import.
    Circle(Circle),
}

impl PadGeometry {
    /// 2 for a real area, less for degenerate geometry.
    pub fn dimension(&self) -> i32 {
        match self {
            PadGeometry::Box(b) => b.dimension(),
            PadGeometry::Octagon(o) => o.dimension(),
            PadGeometry::Simplex(s) => s.dimension(),
            PadGeometry::Circle(c) => c.dimension(),
        }
    }

    /// Upstream `ConvexShape.offset(width)` — grow by `width` in every direction.
    pub fn offset(&self, width: f64) -> PadGeometry {
        match self {
            PadGeometry::Box(b) => PadGeometry::Box(b.offset(width)),
            PadGeometry::Octagon(o) => PadGeometry::Octagon(o.offset(width)),
            PadGeometry::Simplex(s) => PadGeometry::Simplex(s.offset(width)),
            PadGeometry::Circle(c) => {
                PadGeometry::Circle(Circle::new(c.center, c.radius + width.jround() as i32))
            }
        }
    }

    /// Exact 90°-multiple rotation about `pole` — upstream `TileShape.turn90Degree`, which turns
    /// the border *lines* with exact integer arithmetic.
    pub fn turn_90_degree(&self, factor: i32, pole: &IntPoint) -> PadGeometry {
        match self {
            PadGeometry::Box(b) => PadGeometry::Box(b.turn_90_degree(factor, pole)),
            PadGeometry::Circle(c) => {
                PadGeometry::Circle(Circle::new(c.center.turn_90_degree(factor, pole), c.radius))
            }
            other => simplify_pad(PadGeometry::Simplex(
                other.to_simplex().turn_90_degree(factor, pole),
            )),
        }
    }

    /// Approximate rotation by an arbitrary angle — upstream `TileShape.rotateApprox`
    /// (`TileShape.java:677-700`).
    ///
    /// It rotates the shape's **corners**, not its border lines, and rebuilds the tile from the
    /// resulting corner polygon. Rotating the lines instead is wrong for any shape whose border
    /// lines are defined by two points a single unit apart — which is exactly how an `IntBox`
    /// defines its four edges — because rounding then destroys the line's direction and the
    /// "shape" degenerates into an unbounded half-plane intersection.
    pub fn rotate_approx(&self, angle_rad: f64, pole: &FloatPoint) -> PadGeometry {
        if angle_rad == 0.0 {
            return self.clone();
        }
        match self {
            PadGeometry::Circle(c) => {
                let center = c.center.to_float().rotate(angle_rad, pole).jround();
                PadGeometry::Circle(Circle::new(center, c.radius))
            }
            other => {
                let simplex = other.to_simplex();
                let corners: Vec<IntPoint> = simplex
                    .corner_approx_arr()
                    .iter()
                    .map(|corner| corner.rotate(angle_rad, pole).jround())
                    .collect();
                let normalized = normalize_polygon_corners(&corners);
                if normalized.len() < 3 {
                    // Upstream degrades a 2-corner result to a line segment's simplex and a
                    // 1-corner result to a point; both have dimension < 2 and hit the
                    // one-unit-enlargement rescue at the call site, so an empty simplex is
                    // equivalent there.
                    return PadGeometry::Simplex(Simplex::get_instance(&[]));
                }
                simplify_pad(PadGeometry::Simplex(bounding_tile(&normalized)))
            }
        }
    }

    /// Mirror at the vertical axis through `pole` — the `x → -x` both flip styles apply.
    pub fn mirror_vertical(&self, pole: &IntPoint) -> PadGeometry {
        match self {
            PadGeometry::Circle(c) => {
                PadGeometry::Circle(Circle::new(c.center.mirror_vertical(pole), c.radius))
            }
            other => simplify_pad(PadGeometry::Simplex(
                other.to_simplex().mirror_vertical(pole),
            )),
        }
    }

    /// Translation.
    pub fn translate_by(&self, vector: &Vector) -> PadGeometry {
        match self {
            PadGeometry::Box(b) => PadGeometry::Box(b.translate_by(vector)),
            PadGeometry::Octagon(o) => PadGeometry::Octagon(o.translate_by(vector)),
            PadGeometry::Simplex(s) => PadGeometry::Simplex(s.translate_by(vector)),
            PadGeometry::Circle(c) => {
                let center = match Point::Int(c.center).translate_by(vector) {
                    Point::Int(p) => p,
                    Point::Rational(r) => r.to_float().jround(),
                };
                PadGeometry::Circle(Circle::new(center, c.radius))
            }
        }
    }

    /// Smallest surrounding box.
    pub fn bounding_box(&self) -> IntBox {
        match self {
            PadGeometry::Box(b) => *b,
            PadGeometry::Octagon(o) => o.bounding_box(),
            PadGeometry::Simplex(s) => s.bounding_box(),
            PadGeometry::Circle(c) => IntBox::new(
                c.center.x - c.radius,
                c.center.y - c.radius,
                c.center.x + c.radius,
                c.center.y + c.radius,
            ),
        }
    }

    fn to_simplex(&self) -> Simplex {
        match self {
            PadGeometry::Box(b) => b.to_simplex(),
            PadGeometry::Octagon(o) => o.to_simplex(),
            PadGeometry::Simplex(s) => s.clone(),
            PadGeometry::Circle(c) => c.bounding_octagon().to_simplex(),
        }
    }

    /// Upstream `Shape.containsInside`, used by `Pin.getCenter` to validate a computed centre.
    pub fn contains_inside(&self, point: &IntPoint) -> bool {
        match self {
            PadGeometry::Box(b) => b.contains_inside(point),
            PadGeometry::Circle(c) => {
                let dx = f64::from(point.x - c.center.x);
                let dy = f64::from(point.y - c.center.y);
                dx * dx + dy * dy < f64::from(c.radius) * f64::from(c.radius)
            }
            other => other.to_simplex().contains_inside(&Point::Int(*point)),
        }
    }

    /// Upstream `Shape.centreOfGravity`.
    pub fn centre_of_gravity(&self) -> FloatPoint {
        match self {
            PadGeometry::Circle(c) => c.center.to_float(),
            PadGeometry::Box(b) => FloatPoint::new(
                f64::from(b.ll.x + b.ur.x) / 2.0,
                f64::from(b.ll.y + b.ur.y) / 2.0,
            ),
            other => other.to_simplex().centre_of_gravity(),
        }
    }

    /// Lowers into the board's tile-shape union. A circle becomes its bounding octagon, which is
    /// the approximation `fr_board::Padstack::round` already makes for vias.
    pub fn to_tile_shape(&self) -> fr_board::TileShape {
        match self {
            PadGeometry::Box(b) => fr_board::TileShape::Box(*b),
            PadGeometry::Octagon(o) => fr_board::TileShape::Oct(*o),
            PadGeometry::Simplex(s) => fr_board::TileShape::Simplex(s.clone()),
            PadGeometry::Circle(c) => fr_board::TileShape::Oct(c.bounding_octagon()),
        }
    }

    /// Lowers into `fr_rules::PadShape`, which the via-rule dedup compares widths on. A simplex
    /// has no representation there and degrades to its bounding octagon.
    fn to_pad_shape(&self) -> PadShape {
        match self {
            PadGeometry::Box(b) => PadShape::Box(*b),
            PadGeometry::Octagon(o) => PadShape::Octagon(*o),
            PadGeometry::Circle(c) => PadShape::Circle { radius: c.radius },
            PadGeometry::Simplex(s) => PadShape::Octagon(
                s.bounding_octagon()
                    .unwrap_or_else(|| s.bounding_box().to_int_octagon()),
            ),
        }
    }
}

/// A padstack as the DSN defined it: one convex shape per board layer, `None` where the padstack
/// has no copper.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedPadstack {
    /// Name with the `.<digits>` suffix stripped.
    pub name: String,
    /// One slot per board layer.
    pub shapes: Vec<Option<PadGeometry>>,
    /// `(attach off)` clears this.
    pub attach_allowed: bool,
    /// `(absolute on)` sets this; it disables the back-side layer flip.
    pub placed_absolute: bool,
}

impl ImportedPadstack {
    /// Lowest layer carrying copper, or 0 when the padstack is empty.
    pub fn from_layer(&self) -> usize {
        self.shapes.iter().position(Option::is_some).unwrap_or(0)
    }

    /// Highest layer carrying copper, or 0 when the padstack is empty.
    pub fn to_layer(&self) -> usize {
        self.shapes.iter().rposition(Option::is_some).unwrap_or(0)
    }

    /// True when the padstack occupies a single copper layer — upstream's SMD test, which is what
    /// decides between the `SMD` and `PIN` default clearance classes and whether a via may be
    /// dropped on the pad.
    pub fn is_smd(&self) -> bool {
        self.from_layer() == self.to_layer()
    }

    fn to_rules_padstack(&self, no: usize) -> fr_rules::Padstack {
        let shapes = self
            .shapes
            .iter()
            .map(|slot| slot.as_ref().map(PadGeometry::to_pad_shape))
            .collect();
        fr_rules::Padstack::new(
            self.name.clone(),
            no,
            shapes,
            self.attach_allowed,
            self.placed_absolute,
        )
    }
}

/// A library package after geometry conversion and `::N` deduplication.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedPackage {
    /// The name it was finally registered under.
    pub name: String,
    /// Which side the footprint was defined for.
    pub is_front: bool,
    /// Pins in file order; the index is the board-side pin number.
    pub pins: Vec<ImportedPin>,
    /// Outline shapes, relative to the component origin.
    pub outlines: Vec<Option<BoardShape>>,
    /// The DSN path width of each outline, verbatim — the courtyard/fabrication classification
    /// thresholds compare against these raw DSN numbers, which makes them resolution-dependent
    /// upstream as well.
    pub outline_widths: Vec<f64>,
    /// Whether each outline path's first and last coordinate pair coincide.
    pub outline_is_closed: Vec<bool>,
    /// Wire keepouts, relative to the component origin.
    pub keepouts: Vec<ImportedKeepout>,
    /// Via keepouts.
    pub via_keepouts: Vec<ImportedKeepout>,
    /// Placement keepouts.
    pub place_keepouts: Vec<ImportedKeepout>,
}

/// One package pin.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedPin {
    /// Pin name as spelled in the file — numeric names included.
    pub name: String,
    /// Index into [`ImportedDesign::padstacks`].
    pub padstack_index: usize,
    /// Offset from the component origin, in board units.
    pub relative_location: IntVector,
    /// Pin rotation in degrees.
    pub rotation: f64,
}

/// One package keepout.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedKeepout {
    /// Name, generated as `keepout_1`, `via_keepout_1`, … when any keepout of the list was
    /// unnamed. Names matter because `(place … (keepout <name> (clearance_class …)))` binds by
    /// name.
    pub name: Option<String>,
    /// The border shape, relative to the component origin.
    pub area: BoardShape,
    /// The number of `(window …)` holes that were dropped; see [`ImportedDesign::warnings`].
    pub hole_count: usize,
    /// Layer index, or `-1` for "all signal layers".
    pub layer: i32,
}

/// A placed component.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedComponent {
    /// Reference designator.
    pub name: String,
    /// Board location, `None` for an unplaced component.
    pub location: Option<IntPoint>,
    /// Rotation in degrees, normalized into `[0, 360)`.
    pub rotation: f64,
    /// Placement side.
    pub is_front: bool,
    /// Index into [`ImportedDesign::packages`].
    pub package_index: usize,
    /// `(lock_type … position …)`.
    pub position_fixed: bool,
    /// `(PN "…")`.
    pub part_number: Option<String>,
    /// The resolved centre of every pin, in board units, in package pin order.
    pub pin_centers: Vec<IntPoint>,
    /// The clearance class index resolved for every pin, in package pin order.
    pub pin_clearance_classes: Vec<i32>,
    /// The clearance class index resolved for every package keepout, in
    /// [`ImportedPackage::keepouts`] order, then [`ImportedPackage::via_keepouts`], then
    /// [`ImportedPackage::place_keepouts`] — the order `Component.writeKeepoutInfos` walks.
    ///
    /// Upstream keeps this only on the inserted `ObstacleArea` and recovers it by scanning the
    /// whole item list for the area whose `(component_no, name)` matches
    /// (`Component.java:110-163`). Our board items carry no name, so the resolution is recorded
    /// here instead, in the same shape as `pin_clearance_classes`. Empty for an unplaced
    /// component, which is where upstream returns early.
    pub keepout_clearance_classes: KeepoutClearanceClasses,
}

/// The resolved clearance class of every component keepout, by keepout kind.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KeepoutClearanceClasses {
    /// Parallel to [`ImportedPackage::keepouts`].
    pub keepouts: Vec<i32>,
    /// Parallel to [`ImportedPackage::via_keepouts`].
    pub via_keepouts: Vec<i32>,
    /// Parallel to [`ImportedPackage::place_keepouts`].
    pub place_keepouts: Vec<i32>,
}

impl KeepoutClearanceClasses {
    /// The list for one of the three kinds, in `Component.writeKeepoutInfos`'s `j` order.
    pub fn of_kind(&self, kind_index: usize) -> &[i32] {
        match kind_index {
            0 => &self.keepouts,
            1 => &self.via_keepouts,
            _ => &self.place_keepouts,
        }
    }

    fn push(&mut self, kind_index: usize, class: i32) {
        match kind_index {
            0 => self.keepouts.push(class),
            1 => self.via_keepouts.push(class),
            _ => self.place_keepouts.push(class),
        }
    }
}

/// Everything the importer produces.
pub struct ImportedDesign {
    /// The `(pcb <name>)` token.
    pub design_name: String,
    /// `(resolution <unit> …)`.
    pub unit: Unit,
    /// `(resolution … <n>)`.
    pub resolution: i32,
    /// The derived scale factor (§3.2).
    pub scale_factor: f64,
    /// The transform built from it.
    pub transform: CoordinateTransform,
    /// The board bounding box, already inflated by 1000 board units.
    pub bounds: IntBox,
    /// The board, with every item inserted.
    pub board: Board,
    /// The rules: clearance matrix, nets, net classes, via infos, via rules.
    pub rules: BoardRules,
    /// The padstack library, in definition order.
    pub padstacks: Vec<ImportedPadstack>,
    /// The package library, in definition order.
    pub packages: Vec<ImportedPackage>,
    /// The placed components, in file order.
    pub components: Vec<ImportedComponent>,
    /// Indices into [`ImportedDesign::padstacks`] of the via padstacks, in resolution order.
    pub via_padstack_indices: Vec<usize>,
    /// The board outline contours after hole separation.
    pub outline_shapes: Vec<BoardShape>,
    /// The contours that were classified as holes and inserted as keepouts on every layer.
    pub outline_holes: Vec<BoardShape>,
    /// `(place_control (flip_style rotate_first))`, or the Electra spelling in the structure
    /// scope.
    pub flip_style_rotate_first: bool,
    /// `(control (via_at_smd on))`.
    pub via_at_smd_allowed: bool,
    /// `(autoroute_settings …)`, when the guard let it be parsed.
    pub autoroute_settings: Option<DsnAutorouteSettings>,
    /// `(string_quote …)`.
    pub string_quote: String,
    /// `(host_cad …)`.
    pub host_cad: Option<String>,
    /// `(host_version …)`.
    pub host_version: Option<String>,
    /// Everything the read and the build wanted to complain about.
    pub warnings: Vec<String>,
    /// True when `adjustPlaneAutorouteSettings` reclassified at least one layer.
    pub plane_autoroute_settings_adjusted: bool,
}

/// An item waiting for the board to exist.
enum Pending {
    Trace {
        polyline: Polyline,
        layer: i32,
        half_width: i32,
        nets: NetNos,
        clearance_class: i32,
        fixed: FixedState,
    },
    Via {
        via: Via,
        nets: NetNos,
        clearance_class: i32,
        fixed: FixedState,
    },
    Plain(Item),
}

struct Builder<'a> {
    ast: &'a DsnAst,
    options: ImportOptions,
    warnings: Vec<String>,
    transform: CoordinateTransform,
    bounds: IntBox,
    layer_count: usize,
    dsn_layers: DsnLayerStructure,
    rules: BoardRules,
    padstacks: Vec<ImportedPadstack>,
    packages: Vec<ImportedPackage>,
    components: Vec<ImportedComponent>,
    via_padstack_indices: Vec<usize>,
    outline_shapes: Vec<BoardShape>,
    outline_holes: Vec<BoardShape>,
    outline_clearance_class: i32,
    cutout_clearance_class: i32,
    items: Vec<Pending>,
    board_padstacks: fr_board::PadstackTable,
    /// Interned board padstacks, keyed by the placement that produced them.
    padstack_cache: HashMap<PadstackKey, u32>,
    /// `NetList`: pin sets per `(name, subnet)`, ordered the way `Net.Id.compareTo` orders them.
    netlist: BTreeMap<(String, i32), BTreeSet<NetPin>>,
    /// The inverted netlist, built once after every net exists.
    pin_net_index: HashMap<NetPin, Vec<i32>>,
    next_component_no: i32,
}

#[derive(PartialEq, Eq, Hash)]
struct PadstackKey {
    padstack_index: usize,
    pin_rotation_bits: u64,
    component_rotation_bits: u64,
    mirror_on_y_axis: bool,
    flip_after_rotate: bool,
    on_front: bool,
    /// The pin's package-relative location, and `None` whenever the placed pad provably does not
    /// depend on it — see `placed_pad_shapes`. Keeping it out of the key in the right-angle case
    /// is what lets one entry serve a whole footprint; keeping it *in* at an oblique rotation is
    /// what stops one pin's rounding from being handed to its neighbours.
    pin_offset: Option<IntVector>,
}

/// Imports a parsed DSN file.
pub fn build(
    ast: &DsnAst,
    read_warnings: Vec<String>,
    options: ImportOptions,
) -> Result<ImportedDesign, String> {
    let mut builder = Builder::new(ast, options, read_warnings)?;
    builder.build_structure_tail();
    builder.build_library();
    builder.build_network();
    builder.build_wiring();
    Ok(builder.finish())
}

impl<'a> Builder<'a> {
    // -----------------------------------------------------------------------------------------
    // §6.2 / §3.2 — the board, the scale factor, the rules
    // -----------------------------------------------------------------------------------------

    fn new(
        ast: &'a DsnAst,
        options: ImportOptions,
        warnings: Vec<String>,
    ) -> Result<Builder<'a>, String> {
        let layer_count = ast.structure.layers.len();
        if layer_count == 0 {
            return Err("Structure.create_board: layers missing in structure scope".to_string());
        }
        let dsn_layers = DsnLayerStructure {
            layers: ast.structure.layers.clone(),
        };

        // The bounding shape, or the union of every outline shape's bounding box when the
        // `(boundary (… pcb …))` record is missing.
        let bounding_box_dsn = match &ast.structure.bounding_shape {
            Some(shape) => shape.bounding_box(),
            None => {
                if ast.structure.outline_shapes.is_empty() {
                    return Err("Structure.create_board: outline missing".to_string());
                }
                let mut bounds = ast.structure.outline_shapes[0].bounding_box();
                for shape in &ast.structure.outline_shapes[1..] {
                    let other = shape.bounding_box();
                    bounds = [
                        bounds[0].min(other[0]),
                        bounds[1].min(other[1]),
                        bounds[2].max(other[2]),
                        bounds[3].max(other[3]),
                    ];
                }
                bounds
            }
        };

        let scale_factor = match derive_scale_factor(bounding_box_dsn, ast.resolution) {
            ScaleFactor::Ok(value) => value,
            ScaleFactor::Degenerate => {
                return Err("Structure.create_board: degenerate board outline".to_string())
            }
        };
        let transform = CoordinateTransform::new(scale_factor, 0.0, 0.0);

        let lower_left = transform
            .dsn_to_board_point(bounding_box_dsn[0], bounding_box_dsn[1])
            .jround();
        let upper_right = transform
            .dsn_to_board_point(bounding_box_dsn[2], bounding_box_dsn[3])
            .jround();
        // §6.2 step 5: the board box is inflated by 1000 board units on every side, which is the
        // margin that lets the router shove traces slightly outside the nominal outline.
        let bounds = IntBox::new(
            lower_left.x.min(upper_right.x),
            lower_left.y.min(upper_right.y),
            lower_left.x.max(upper_right.x),
            lower_left.y.max(upper_right.y),
        )
        .offset(1000.0);

        let rules_layers = fr_rules::LayerStructure::new(
            ast.structure
                .layers
                .iter()
                .map(|layer| fr_rules::Layer::new(layer.name.clone(), layer.is_signal))
                .collect(),
        );
        let clearance_matrix = ClearanceMatrix::default_instance(rules_layers.clone(), 0);
        let rules = BoardRules::new(rules_layers, clearance_matrix);

        let mut builder = Builder {
            ast,
            options,
            warnings,
            transform,
            bounds,
            layer_count,
            dsn_layers,
            rules,
            padstacks: Vec::new(),
            packages: Vec::new(),
            components: Vec::new(),
            via_padstack_indices: Vec::new(),
            outline_shapes: Vec::new(),
            outline_holes: Vec::new(),
            outline_clearance_class: 0,
            cutout_clearance_class: 0,
            items: Vec::new(),
            board_padstacks: fr_board::PadstackTable::new(),
            padstack_cache: HashMap::new(),
            netlist: BTreeMap::new(),
            pin_net_index: HashMap::new(),
            next_component_no: 1,
        };

        builder.build_outline(&bounding_box_dsn);
        builder.update_board_rules();
        if let Some(angle) = ast.structure.snap_angle {
            builder.rules.set_trace_angle_restriction(
                fr_rules::AngleRestriction::from_i32(angle)
                    .unwrap_or(fr_rules::AngleRestriction::FortyFiveDegree),
            );
        }
        builder.resolve_outline_clearance_class();
        builder.insert_outline_item();
        builder.apply_copper_to_edge_clearance_override();
        builder.apply_cutout_clearance_override();
        builder.apply_hole_clearance_override();
        builder.insert_outline_holes();
        Ok(builder)
    }

    fn warn(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }

    /// §6.2 steps 6-8: transform the outline shapes, drop the degenerate ones, fall back to the
    /// bounding shape when everything was dropped, then separate the holes.
    fn build_outline(&mut self, bounding_box_dsn: &[f64; 4]) {
        let mut shapes = Vec::new();
        for shape in &self.ast.structure.outline_shapes {
            // A path with a non-zero width is rewritten to width 0 first, because the offset
            // function `transformToBoard` would use is only implemented for convex shapes.
            let shape = match shape {
                DsnShape::PolygonPath { layer, coor, .. } => DsnShape::PolygonPath {
                    layer: layer.clone(),
                    width: 0.0,
                    coor: coor.clone(),
                },
                other => other.clone(),
            };
            if let Some(board_shape) = transform_to_board(&shape, &self.transform) {
                if board_shape.dimension() > 0 {
                    shapes.push(board_shape);
                }
            }
        }
        if shapes.is_empty() {
            let fallback = match &self.ast.structure.bounding_shape {
                Some(shape) => transform_to_board(shape, &self.transform),
                None => {
                    let lower_left = self
                        .transform
                        .dsn_to_board_point(bounding_box_dsn[0], bounding_box_dsn[1])
                        .jround();
                    let upper_right = self
                        .transform
                        .dsn_to_board_point(bounding_box_dsn[2], bounding_box_dsn[3])
                        .jround();
                    Some(BoardShape::Box(IntBox::new(
                        lower_left.x.min(upper_right.x),
                        lower_left.y.min(upper_right.y),
                        lower_left.x.max(upper_right.x),
                        lower_left.y.max(upper_right.y),
                    )))
                }
            };
            if let Some(shape) = fallback {
                shapes.push(shape);
            }
        }
        let holes = separate_holes(&mut shapes);
        self.outline_shapes = shapes;
        self.outline_holes = holes;
    }

    /// §7.2 `Structure.updateBoardRules` — clearance rules first, then widths, then the
    /// per-layer rules, then the `smd_to_turn_gap` fallback.
    fn update_board_rules(&mut self) {
        let mut smd_to_turn_gap_found = false;
        let default_rules = self.ast.structure.default_rules.clone();
        for rule in &default_rules {
            if let DsnRule::Clearance { value, class_pairs } = rule {
                if self.set_clearance_rule(*value, class_pairs, -1) {
                    smd_to_turn_gap_found = true;
                }
            }
        }
        for rule in &default_rules {
            if let DsnRule::Width(width) = rule {
                // §3.4: the PCB-level rounding is round(dsnToBoard(w) / 2) — halve in *board*
                // units. The net-class-level rule halves in DSN units first; the two differ by
                // one board unit for odd widths at low resolution and both ship.
                let half_width = (self.transform.dsn_to_board(*width) / 2.0).jround() as i32;
                self.rules.set_default_trace_half_widths(half_width);
            }
        }
        let layer_rules = self.ast.structure.layer_dependent_rules.clone();
        for layer_rule in &layer_rules {
            for layer_name in &layer_rule.layer_names {
                let layer_index = self.dsn_layers.get_no(layer_name);
                if layer_index < 0 {
                    continue;
                }
                for rule in &layer_rule.rules {
                    match rule {
                        DsnRule::Width(width) => {
                            let half_width =
                                (self.transform.dsn_to_board(*width) / 2.0).jround() as i32;
                            self.rules
                                .set_default_trace_half_width(layer_index as usize, half_width);
                        }
                        DsnRule::Clearance { value, class_pairs } => {
                            self.set_clearance_rule(*value, class_pairs, layer_index);
                        }
                    }
                }
            }
        }
        if !smd_to_turn_gap_found {
            let min_half_width = self.rules.get_min_trace_half_width();
            self.rules
                .set_pin_edge_to_turn_dist(f64::from(min_half_width));
        }
    }

    /// §7.3 `Structure.setClearanceRule`. Returns true when `smd_to_turn_gap` was seen.
    fn set_clearance_rule(&mut self, value: f64, class_pairs: &[String], layer_index: i32) -> bool {
        let mut found = false;
        let clearance = self.transform.dsn_to_board(value).jround() as i32;
        if class_pairs.is_empty() {
            if layer_index < 0 {
                self.rules.clearance_matrix.set_default_value(clearance);
            } else {
                self.rules
                    .clearance_matrix
                    .set_default_value_on_layer(layer_index as usize, clearance);
            }
            return false;
        }
        if contains_wire_clearance_pair(class_pairs) {
            self.create_default_clearance_classes();
        }
        for current in class_pairs {
            match structure_class_pair(class_pairs, current, &self.ast.string_quote) {
                ClassPairResolution::SmdToTurnGap => {
                    self.rules.set_pin_edge_to_turn_dist(f64::from(clearance));
                    found = true;
                }
                ClassPairResolution::Skip => {}
                ClassPairResolution::Pair(first, second) => {
                    let first_no = self.resolve_structure_class(&first);
                    let second_no = self.resolve_structure_class(&second);
                    self.write_clearance(first_no, second_no, layer_index, clearance);
                }
            }
        }
        found
    }

    /// `"wire"` aliases clearance class 1 and is never appended as a class of its own.
    fn resolve_structure_class(&mut self, name: &str) -> usize {
        if name == fr_rules::WIRE_CLASS_NAME {
            return fr_rules::WIRE_CLEARANCE_CLASS_NO;
        }
        match self.rules.clearance_matrix.get_no(name) {
            Some(index) => index,
            None => self.append_clearance_class(name),
        }
    }

    /// `Structure.appendClearanceClass`: append to the matrix and, for the four reserved names,
    /// bind the new index into the **default** net class's item clearance classes.
    fn append_clearance_class(&mut self, name: &str) -> usize {
        let (index, binding) =
            fr_rules::append_clearance_class(&mut self.rules.clearance_matrix, name);
        if let Some(item_class) = binding {
            self.rules
                .get_default_net_class_mut()
                .default_item_clearance_classes
                .set(item_class, index as i32);
        }
        index
    }

    /// `via`, `smd`, `pin`, `area` — in that order, so on a fresh board they take indices 2..5.
    /// The order is load-bearing because items store class indices.
    fn create_default_clearance_classes(&mut self) {
        for name in RESERVED_ITEM_CLASS_NAMES {
            self.append_clearance_class(name);
        }
    }

    fn write_clearance(&mut self, first: usize, second: usize, layer_index: i32, value: i32) {
        if layer_index < 0 {
            self.rules.clearance_matrix.set_value(first, second, value);
            self.rules.clearance_matrix.set_value(second, first, value);
        } else {
            let layer = layer_index as usize;
            self.rules
                .clearance_matrix
                .set_value_on_layer(first, second, layer, value);
            self.rules
                .clearance_matrix
                .set_value_on_layer(second, first, layer, value);
        }
    }

    /// `HeadlessBoardManager.createBoard`: the outline's clearance class comes from the
    /// `(boundary … (clearance_class …))` name, clamped at 0, or from the default net class's
    /// `AREA` class.
    fn resolve_outline_clearance_class(&mut self) {
        self.outline_clearance_class = match &self.ast.structure.outline_clearance_class_name {
            Some(name) => self
                .rules
                .clearance_matrix
                .get_no(name)
                .map_or(0, |index| index as i32),
            None => self
                .rules
                .get_default_net_class()
                .default_item_clearance_classes
                .get(ItemClass::Area),
        };
    }

    fn insert_outline_item(&mut self) {
        let borders: Vec<Polyline> = self
            .outline_shapes
            .iter()
            .filter_map(board_shape_to_polyline)
            .collect();
        self.items.push(Pending::Plain(Item::new(
            NetNos::default(),
            self.outline_clearance_class,
            FixedState::SystemFixed,
            ItemKind::BoardOutline(fr_board::item::BoardOutline {
                borders,
                keepout_outside_outline: false,
            }),
        )));
    }

    /// §6.2 step 15: every hole in the outline becomes an `ObstacleArea` on **every** layer, with
    /// `cutout_clearance_class` and `SYSTEM_FIXED`.
    fn insert_outline_holes(&mut self) {
        let holes = self.outline_holes.clone();
        for hole in &holes {
            for layer in 0..self.layer_count as i32 {
                self.push_obstacle(
                    hole,
                    layer,
                    ObstacleKind::Keepout,
                    self.cutout_clearance_class,
                    FixedState::SystemFixed,
                    0,
                );
            }
        }
    }

    /// `HeadlessBoardManager.applyCopperToEdgeClearanceOverride` (`:466-552`).
    ///
    /// Skipped when the configured value is the default *and* the DSN gave the outline an
    /// explicit clearance class, which is how a `(boundary … (clearance_class boundary))` record
    /// survives. Otherwise a `board_edge` class is appended and written symmetrically against
    /// every class on every layer, and the outline is reassigned to it.
    ///
    /// Parity note: upstream's variable is called `configuredClearanceBoardUnits`, but the
    /// conversion `Unit.scale(um * resolution, UM, unit)` produces **DSN** units — the board
    /// scale factor is never applied. That is transcribed as-is.
    fn apply_copper_to_edge_clearance_override(&mut self) {
        let Some(configured_um) = self.options.copper_to_edge_clearance_um else {
            return;
        };
        if configured_um < 0.0 {
            self.warn(format!(
                "Ignoring router.copper_to_edge_clearance_um because it is negative: {configured_um}"
            ));
            return;
        }
        let default_area_class = self
            .rules
            .get_default_net_class()
            .default_item_clearance_classes
            .get(ItemClass::Area);
        let uses_fallback_outline_class = self.outline_clearance_class == default_area_class;
        let uses_default_value = (configured_um - 500.0).abs() < 1e-9;
        if uses_default_value && !uses_fallback_outline_class {
            return;
        }
        let board_resolution = self.ast.resolution.max(1);
        let configured_units = Unit::scale(
            configured_um * f64::from(board_resolution),
            Unit::Um,
            self.ast.unit,
        )
        .jround() as i32;
        let board_edge_class = match self.rules.clearance_matrix.get_no("board_edge") {
            Some(index) => index,
            None => {
                self.rules.clearance_matrix.append_class("board_edge");
                match self.rules.clearance_matrix.get_no("board_edge") {
                    Some(index) => index,
                    None => {
                        self.warn("Unable to create the board_edge clearance class");
                        return;
                    }
                }
            }
        };
        let class_count = self.rules.clearance_matrix.class_count();
        for layer in 0..self.layer_count {
            for class_no in 1..class_count {
                self.rules.clearance_matrix.set_value_on_layer(
                    board_edge_class,
                    class_no,
                    layer,
                    configured_units,
                );
                self.rules.clearance_matrix.set_value_on_layer(
                    class_no,
                    board_edge_class,
                    layer,
                    configured_units,
                );
            }
        }
        self.outline_clearance_class = board_edge_class as i32;
        if let Some(Pending::Plain(item)) = self.items.iter_mut().find(
            |item| matches!(item, Pending::Plain(i) if matches!(i.kind, ItemKind::BoardOutline(_))),
        ) {
            item.clearance_class = board_edge_class as i32;
        }
    }

    /// Clearance class for inner cutouts (Edge.Cuts cutouts/holes).
    ///
    /// When `cutout_clearance_um` is None (the default), cutouts inherit the
    /// `board_edge` clearance class (the same rule as the outer board boundary).
    ///
    /// When `cutout_clearance_um` is Some(um), a dedicated `cutout_edge` clearance
    /// class is appended and written symmetrically against every class on every layer.
    fn apply_cutout_clearance_override(&mut self) {
        let Some(configured_um) = self.options.cutout_clearance_um else {
            self.cutout_clearance_class = self.outline_clearance_class;
            return;
        };
        if configured_um < 0.0 {
            self.warn(format!(
                "Ignoring router.cutout_clearance_um because it is negative: {configured_um}"
            ));
            self.cutout_clearance_class = self.outline_clearance_class;
            return;
        }
        let board_resolution = self.ast.resolution.max(1);
        let configured_units = Unit::scale(
            configured_um * f64::from(board_resolution),
            Unit::Um,
            self.ast.unit,
        )
        .jround() as i32;
        let cutout_edge_class = match self.rules.clearance_matrix.get_no("cutout_edge") {
            Some(index) => index,
            None => {
                self.rules.clearance_matrix.append_class("cutout_edge");
                match self.rules.clearance_matrix.get_no("cutout_edge") {
                    Some(index) => index,
                    None => {
                        self.warn("Unable to create the cutout_edge clearance class");
                        self.cutout_clearance_class = self.outline_clearance_class;
                        return;
                    }
                }
            }
        };
        let class_count = self.rules.clearance_matrix.class_count();
        for layer in 0..self.layer_count {
            for class_no in 1..class_count {
                self.rules.clearance_matrix.set_value_on_layer(
                    cutout_edge_class,
                    class_no,
                    layer,
                    configured_units,
                );
                self.rules.clearance_matrix.set_value_on_layer(
                    class_no,
                    cutout_edge_class,
                    layer,
                    configured_units,
                );
            }
        }
        self.cutout_clearance_class = cutout_edge_class as i32;
    }

    /// `HeadlessBoardManager.applyHoleClearanceOverride` (`:346-412`).
    ///
    /// With the shipped default of 0 µm this only records the value; the `hole_edge` clearance
    /// class and the circle-keepout reclassification are reached only when a caller raises it.
    fn apply_hole_clearance_override(&mut self) {
        let Some(configured_um) = self.options.hole_clearance_um else {
            return;
        };
        if configured_um < 0.0 {
            self.warn(format!(
                "Ignoring router.hole_clearance_um because it is negative: {configured_um}"
            ));
            return;
        }
        let board_resolution = self.ast.resolution.max(1);
        let configured_units = Unit::scale(
            configured_um * f64::from(board_resolution),
            Unit::Um,
            self.ast.unit,
        )
        .jround() as i32;
        self.rules.set_hole_clearance(configured_units);
    }

    // -----------------------------------------------------------------------------------------
    // §11 — keepouts, planes, missing power planes
    // -----------------------------------------------------------------------------------------

    fn build_structure_tail(&mut self) {
        let keepouts = self.ast.structure.keepouts.clone();
        for area in &keepouts {
            self.insert_structure_keepout(area, ObstacleKind::Keepout);
        }
        let via_keepouts = self.ast.structure.via_keepouts.clone();
        for area in &via_keepouts {
            self.insert_structure_keepout(area, ObstacleKind::ViaKeepout);
        }
        let place_keepouts = self.ast.structure.place_keepouts.clone();
        for area in &place_keepouts {
            self.insert_structure_keepout(area, ObstacleKind::ComponentKeepout);
        }
        self.insert_planes();
        self.insert_missing_power_planes();
    }

    /// Determines whether a structure keepout is an inner board cutout
    /// (e.g. KiCad Edge.Cuts hole exported via `SPECCTRA_DB::fillBOUNDARY`
    /// as `(keepout "" (polygon signal 0 ...))`).
    fn is_inner_cutout(&self, area: &DsnArea, kind: ObstacleKind) -> bool {
        if kind != ObstacleKind::Keepout {
            return false;
        }
        if area.clearance_class_name.is_some() {
            return false;
        }
        if area.shapes.is_empty() || !area.shapes.iter().all(|s| s.layer().is_signal_pseudo()) {
            return false;
        }
        if let Some(name) = &area.name {
            if !name.is_empty() {
                return false;
            }
        }
        true
    }

    /// §11.2 `Structure.insertKeepout`.
    ///
    /// A degenerate keepout is skipped with a warning rather than aborting the read — a v2
    /// addition without which KiCad 4.0.7 exports do not load at all.
    fn insert_structure_keepout(&mut self, area: &DsnArea, kind: ObstacleKind) {
        let Some(shape) = self.area_to_board(area) else {
            return;
        };
        if shape.dimension() < 2 {
            let name = area.name.clone().unwrap_or_default();
            self.warn(format!(
                "Keepout zone '{name}' was skipped because its geometry is degenerate"
            ));
            return;
        }
        let clearance_class = if self.is_inner_cutout(area, kind) {
            self.cutout_clearance_class
        } else {
            self.keepout_clearance_class(area.clearance_class_name.as_deref())
        };
        let layer = area.shapes[0].layer().clone();
        if layer.is_signal_pseudo() {
            for index in 0..self.layer_count {
                if self.dsn_layers.layers[index].is_signal {
                    self.push_obstacle(
                        &shape,
                        index as i32,
                        kind,
                        clearance_class,
                        FixedState::SystemFixed,
                        0,
                    );
                }
            }
        } else if layer.no >= 0 {
            self.push_obstacle(
                &shape,
                layer.no,
                kind,
                clearance_class,
                FixedState::SystemFixed,
                0,
            );
        } else {
            self.warn("Structure.insert_keepout: unknown layer name");
        }
    }

    fn keepout_clearance_class(&mut self, name: Option<&str>) -> i32 {
        match name {
            None => self
                .rules
                .get_default_net_class()
                .default_item_clearance_classes
                .get(ItemClass::Area),
            Some(name) => match self.rules.clearance_matrix.get_no(name) {
                Some(index) => index as i32,
                None => {
                    self.warn(format!(
                        "Keepout.insert_keepout: clearance class not found at '{name}'"
                    ));
                    0
                }
            },
        }
    }

    /// §11.4: planes are always subnet 1, the net is created if missing with
    /// `contains_plane = true` hardcoded, and the pour is inserted as a **non-obstacle**
    /// conduction area.
    fn insert_planes(&mut self) {
        let planes = self.ast.structure.planes.clone();
        for plane in &planes {
            let key = (plane.net_name.clone(), 1);
            if !self.netlist.contains_key(&key) {
                self.netlist.insert(key.clone(), BTreeSet::new());
                let default_class = 0usize;
                self.rules
                    .nets
                    .add(plane.net_name.clone(), 1, true, default_class);
            }
            let Some(net) = self.rules.nets.get_by_name_and_subnet(&plane.net_name, 1) else {
                self.warn(format!(
                    "Plane.read_scope: net not found at '{}'",
                    plane.net_name
                ));
                continue;
            };
            let net_number = net.net_number;
            let net_class_index = net.net_class_index;
            let Some(shape) = self.area_to_board(&plane.area) else {
                continue;
            };
            let layer = plane.area.shapes[0].layer().clone();
            if layer.no < 0 {
                self.warn("Plane.read_scope: unexpected layer name");
                continue;
            }
            let clearance_class = match &plane.area.clearance_class_name {
                Some(name) => match self.rules.clearance_matrix.get_no(name) {
                    Some(index) => index as i32,
                    None => {
                        self.warn(format!(
                            "Structure.read_scope: clearance class not found at '{name}'"
                        ));
                        0
                    }
                },
                None => self
                    .rules
                    .net_classes
                    .get(net_class_index)
                    .map_or(1, |class| {
                        class.default_item_clearance_classes.get(ItemClass::Area)
                    }),
            };
            self.push_conduction_area(
                &shape,
                layer.no,
                NetNos::from_slice(&[net_number]),
                clearance_class,
                FixedState::SystemFixed,
            );
        }
    }

    /// §11.5 `Structure.insertMissingPowerPlanes`: a non-signal layer with a `(use_net …)` but no
    /// conduction area gets one covering the whole board bounding box, clearance class 0.
    fn insert_missing_power_planes(&mut self) {
        for index in 0..self.layer_count {
            let layer = self.dsn_layers.layers[index].clone();
            if layer.is_signal || layer.net_names.is_empty() {
                continue;
            }
            let has_conduction_area = self.items.iter().any(|pending| match pending {
                Pending::Plain(item) => matches!(
                    &item.kind,
                    ItemKind::ConductionArea(area) if area.layer == layer.no
                ),
                _ => false,
            });
            if has_conduction_area {
                continue;
            }
            let net_name = layer.net_names[0].clone();
            let key = (net_name.clone(), 1);
            if let std::collections::btree_map::Entry::Vacant(entry) = self.netlist.entry(key) {
                entry.insert(BTreeSet::new());
                self.rules.nets.add(net_name.clone(), 1, true, 0);
            }
            let Some(net) = self.rules.nets.get_by_name_and_subnet(&net_name, 1) else {
                self.warn(format!(
                    "Structure.insert_missing_power_planes: net not found at '{net_name}'"
                ));
                continue;
            };
            let net_number = net.net_number;
            let bounds = self.bounds;
            self.push_conduction_area(
                &BoardShape::Box(bounds),
                layer.no,
                NetNos::from_slice(&[net_number]),
                0,
                FixedState::SystemFixed,
            );
        }
    }

    /// `Shape.transformAreaToBoard`. Areas with holes are not representable in `fr_board`'s
    /// [`AreaShape`] yet, so the holes are dropped with a warning and the border is kept — the
    /// permissive direction, and the same one `fr_board`'s search tree already takes for a raw
    /// polygon area.
    fn area_to_board(&mut self, area: &DsnArea) -> Option<BoardShape> {
        let border = area.shapes.first()?;
        let shape = transform_to_board(border, &self.transform)?;
        if area.shapes.len() > 1 {
            self.warn(format!(
                "Area '{}' has {} window(s); holes are not yet modelled and were dropped",
                area.name.clone().unwrap_or_default(),
                area.shapes.len() - 1
            ));
        }
        Some(shape)
    }

    // -----------------------------------------------------------------------------------------
    // §8 — library
    // -----------------------------------------------------------------------------------------

    fn build_library(&mut self) {
        for padstack in &self.ast.padstacks {
            let mut shapes: Vec<Option<PadGeometry>> = vec![None; self.layer_count];
            let mut warnings = Vec::new();
            for pad_shape in &padstack.shapes {
                let Some(board_shape) = transform_to_board_rel(pad_shape, &self.transform) else {
                    continue;
                };
                let mut convex = match board_shape {
                    BoardShape::Box(b) => Some(PadGeometry::Box(b)),
                    BoardShape::Octagon(o) => Some(PadGeometry::Octagon(o)),
                    BoardShape::Circle(c) => Some(PadGeometry::Circle(c)),
                    BoardShape::Simplex(s) => Some(simplify_pad(PadGeometry::Simplex(s))),
                    // §8.1: a non-convex pad is replaced by its **convex hull**, then by the
                    // single convex piece of that hull.
                    BoardShape::Polygon(corners) => {
                        let hull = convex_hull(&corners);
                        if hull.len() < 3 {
                            None
                        } else {
                            Some(simplify_pad(PadGeometry::Simplex(bounding_tile(&hull))))
                        }
                    }
                };
                if let Some(shape) = &convex {
                    if shape.dimension() < 2 {
                        warnings.push(format!(
                            "Library.read_padstack_scope: the shape of padstack '{}' is not an area; enlarging it by one unit as a workaround",
                            padstack.name
                        ));
                        let enlarged = shape.offset(1.0);
                        convex = if enlarged.dimension() < 2 {
                            None
                        } else {
                            Some(enlarged)
                        };
                    }
                }
                let layer = pad_shape.layer();
                if layer.no < 0 {
                    // `pcb` and `signal` both fill *every* layer, non-signal ones included.
                    for slot in shapes.iter_mut() {
                        *slot = convex.clone();
                    }
                } else {
                    let index = self.dsn_layers.get_no(&layer.name);
                    if index < 0 || index as usize >= self.layer_count {
                        warnings.push(format!(
                            "Library.read_padstack_scope: bad layer for padstack '{}'",
                            padstack.name
                        ));
                        continue;
                    }
                    shapes[index as usize] = convex.clone();
                }
            }
            for warning in warnings {
                self.warn(warning);
            }
            self.padstacks.push(ImportedPadstack {
                name: padstack.name.clone(),
                shapes,
                attach_allowed: padstack.attach_allowed,
                placed_absolute: padstack.placed_absolute,
            });
        }

        let packages = self.ast.packages.clone();
        for package in &packages {
            self.build_package(package);
        }
    }

    fn padstack_index(&self, name: &str) -> Option<usize> {
        let cleaned = super::strip_numeric_suffix(name);
        self.padstacks.iter().position(|p| p.name == cleaned)
    }

    fn build_package(&mut self, package: &DsnPackage) {
        let mut pins = Vec::with_capacity(package.pins.len());
        for pin in &package.pins {
            let Some(padstack_index) = self.padstack_index(&pin.padstack_name) else {
                // Upstream aborts the whole library read here (`Library.java:333`), which loses
                // every later package; a warning plus a dropped pin keeps the rest of the board.
                self.warn(format!(
                    "Library.read_scope: board padstack '{}' not found",
                    pin.padstack_name
                ));
                continue;
            };
            pins.push(ImportedPin {
                name: pin.pin_name.clone(),
                padstack_index,
                relative_location: IntVector::new(
                    self.transform.dsn_to_board(pin.rel_coor[0]).jround() as i32,
                    self.transform.dsn_to_board(pin.rel_coor[1]).jround() as i32,
                ),
                rotation: pin.rotation,
            });
        }

        let mut outlines = Vec::with_capacity(package.outlines.len());
        let mut outline_widths = Vec::with_capacity(package.outlines.len());
        let mut outline_is_closed = Vec::with_capacity(package.outlines.len());
        for shape in &package.outlines {
            outlines.push(transform_to_board_rel(shape, &self.transform));
            outline_widths.push(shape.path_width());
            let closed = match shape {
                DsnShape::PolygonPath { coor, .. } | DsnShape::PolylinePath { coor, .. } => {
                    coor.len() >= 4
                        && coor[0] == coor[coor.len() - 2]
                        && coor[1] == coor[coor.len() - 1]
                }
                _ => true,
            };
            outline_is_closed.push(closed);
        }

        let keepouts = self.build_package_keepouts(&package.keepouts, "keepout_");
        let via_keepouts = self.build_package_keepouts(&package.via_keepouts, "via_keepout_");
        let place_keepouts = self.build_package_keepouts(&package.place_keepouts, "place_keepout_");

        // §8.3 the `::N` dedup probe. KiCad exports the same footprint name several times when
        // the pad stacks differ; probing `base`, `base::1`, `base::2`, … and reusing an existing
        // entry with identical pins is what keeps those from becoming distinct packages.
        let base_name = strip_package_suffix(&package.name);
        let mut suffix = 0usize;
        let name = loop {
            let candidate = if suffix == 0 {
                base_name.clone()
            } else {
                format!("{base_name}::{suffix}")
            };
            match self.packages_get(&candidate, package.is_front) {
                Some(existing)
                    if self.packages[existing]
                        .name
                        .eq_ignore_ascii_case(&candidate) =>
                {
                    if package_pins_identical(&self.packages[existing].pins, &pins) {
                        return;
                    }
                    suffix += 1;
                }
                _ => break candidate,
            }
        };

        self.packages.push(ImportedPackage {
            name,
            is_front: package.is_front,
            pins,
            outlines,
            outline_widths,
            outline_is_closed,
            keepouts,
            via_keepouts,
            place_keepouts,
        });
    }

    /// `Library.generateMissingKeepoutNames`: if **any** keepout of a list is unnamed, *all* of
    /// them are renamed `<prefix>1`, `<prefix>2`, …
    fn build_package_keepouts(&mut self, areas: &[DsnArea], prefix: &str) -> Vec<ImportedKeepout> {
        let all_named = areas.iter().all(|area| area.name.is_some());
        let mut result = Vec::with_capacity(areas.len());
        for (index, area) in areas.iter().enumerate() {
            let Some(border) = area.shapes.first() else {
                continue;
            };
            let Some(shape) = transform_to_board_rel(border, &self.transform) else {
                continue;
            };
            let name = if all_named {
                area.name.clone()
            } else {
                Some(format!("{prefix}{}", index + 1))
            };
            result.push(ImportedKeepout {
                name,
                area: shape,
                hole_count: area.shapes.len() - 1,
                // The layer is taken from the **first** shape of the area, and stays `-1` for
                // `signal`/`pcb`, meaning "all signal layers".
                layer: border.layer().no,
            });
        }
        result
    }

    /// `Packages.get(name, isFront)`: case-insensitive, prefers the matching side, falls back to
    /// the other side, and retries after stripping a `::<digits>` suffix.
    fn packages_get(&self, name: &str, is_front: bool) -> Option<usize> {
        let mut other_side = None;
        for (index, package) in self.packages.iter().enumerate() {
            if package.name.eq_ignore_ascii_case(name) {
                if package.is_front == is_front {
                    return Some(index);
                }
                other_side = Some(index);
            }
        }
        let base = strip_package_suffix(name);
        if !base.eq_ignore_ascii_case(name) {
            for (index, package) in self.packages.iter().enumerate() {
                if package.name.eq_ignore_ascii_case(&base) {
                    if package.is_front == is_front {
                        return Some(index);
                    }
                    other_side = Some(index);
                }
            }
        }
        other_side
    }

    // -----------------------------------------------------------------------------------------
    // §9 — network
    // -----------------------------------------------------------------------------------------

    fn build_network(&mut self) {
        self.build_nets();
        self.resolve_via_padstacks();
        self.insert_via_infos();
        self.insert_via_rules();
        self.insert_net_classes();
        self.insert_class_pairs();
        self.insert_components();
    }

    /// §9.5 `Network.readNetScope`, hoisted out of the parse: nets are created in file order,
    /// which is what fixes their net numbers.
    fn build_nets(&mut self) {
        let nets = self.ast.nets.clone();
        for net in &nets {
            let subnet_pin_lists: Vec<Vec<NetPin>> = if !net.fromto.is_empty() {
                net.fromto.clone()
            } else if net.pin_order_found {
                // `(order A B C D)` becomes the sequential 2-pin subnets {A,B}, {B,C}, {C,D}.
                net.pins
                    .windows(2)
                    .map(|pair| {
                        let mut set: Vec<NetPin> = pair.to_vec();
                        set.sort();
                        set.dedup();
                        set
                    })
                    .collect()
            } else {
                vec![net.pins.clone()]
            };
            let mut subnet_number = net.subnet_number;
            for pin_list in subnet_pin_lists {
                let key = (net.name.clone(), subnet_number);
                if !self.netlist.contains_key(&key) {
                    let contains_plane = self.dsn_layers.contains_plane(&net.name);
                    self.netlist.insert(key.clone(), BTreeSet::new());
                    self.rules
                        .nets
                        .add(net.name.clone(), subnet_number, contains_plane, 0);
                }
                if let Some(entry) = self.netlist.get_mut(&key) {
                    entry.clear();
                    entry.extend(pin_list.into_iter());
                }
                if !net.rules.is_empty() {
                    self.apply_net_rules(&net.name, subnet_number, &net.rules);
                }
                subnet_number += 1;
            }
        }
    }

    /// `(net X (rule (width w)))`: find an existing net class with that width, the default trace
    /// clearance class and the default via rule, or make a new one.
    fn apply_net_rules(&mut self, name: &str, subnet: i32, rules: &[DsnRule]) {
        for rule in rules {
            let DsnRule::Width(width) = rule else {
                self.warn("Network.read_net_scope: rule not yet implemented");
                continue;
            };
            let half_width = (self.transform.dsn_to_board(*width) / 2.0).jround() as i32;
            let default_clearance = self
                .rules
                .get_default_net_class()
                .get_trace_clearance_class();
            let default_via_rule = self
                .rules
                .get_default_net_class()
                .get_via_rule()
                .map(|rule| rule.name.clone());
            let existing = self
                .rules
                .net_classes
                .find(half_width, default_clearance, default_via_rule.as_deref())
                .map(|class| class.name.clone())
                .and_then(|class_name| {
                    self.rules
                        .net_classes
                        .iter()
                        .position(|class| class.name == class_name)
                });
            let class_index = match existing {
                Some(index) => index,
                None => self.rules.get_new_net_class(),
            };
            if let Some(class) = self.rules.net_classes.get_mut(class_index) {
                class.set_trace_half_width(half_width);
            }
            if let Some(net) = self.rules.nets.get_by_name_and_subnet(name, subnet) {
                let net_number = net.net_number;
                if let Some(net) = self.rules.nets.get_mut(net_number) {
                    net.net_class_index = class_index;
                }
            }
        }
    }

    /// §6.4: the structure's `(via …)` names plus every net class's `use_via` names, resolved
    /// through the padstack library after `.N` stripping.
    fn resolve_via_padstacks(&mut self) {
        let mut names = self.ast.structure.via_padstack_names.clone();
        for class in &self.ast.net_classes {
            names.extend(class.use_via.iter().cloned());
        }
        for name in &names {
            match self.padstack_index(name) {
                Some(index) => self.via_padstack_indices.push(index),
                None => self.warn(format!(
                    "Library.read_scope: via padstack with name '{name}' not found"
                )),
            }
        }
    }

    /// §9.4 `Network.insertViaInfos`.
    fn insert_via_infos(&mut self) {
        if !self.ast.via_infos.is_empty() {
            let infos = self.ast.via_infos.clone();
            for info in &infos {
                let Some(padstack_index) = self
                    .padstacks
                    .iter()
                    .position(|p| p.name == info.padstack_name)
                else {
                    self.warn(format!(
                        "Network.read_via_info: padstack not found at '{}'",
                        info.padstack_name
                    ));
                    continue;
                };
                // Upstream promotes an unlisted padstack into the via-padstack array here
                // (`Network.java:302`), but `setViaPadstacks` at the end of the network scope
                // then **replaces** that array wholesale with the resolved `(via …)` name list,
                // so the promotion never survives. Reproducing the promotion would leave an
                // extra via padstack behind.
                let clearance_class = self
                    .rules
                    .clearance_matrix
                    .get_no(&info.clearance_class_name)
                    .unwrap_or(BoardRules::default_clearance_class());
                let padstack = self.padstacks[padstack_index].to_rules_padstack(padstack_index);
                self.rules.via_infos.add(ViaInfo::new(
                    info.name.clone(),
                    padstack,
                    clearance_class,
                    info.attach_allowed,
                ));
            }
        } else {
            let default_class = 0usize;
            self.create_default_via_infos(default_class);
        }
    }

    /// `Network.createDefaultViaInfos`: one `ViaInfo` per via padstack. The default net class
    /// names them after the padstack, every other class appends `-<netClassName>`.
    fn create_default_via_infos(&mut self, net_class_index: usize) {
        let is_default_class = net_class_index == 0;
        let clearance_class = self
            .rules
            .net_classes
            .get(net_class_index)
            .map_or(1, |class| {
                class.default_item_clearance_classes.get(ItemClass::Via)
            }) as usize;
        let class_name = self
            .rules
            .net_classes
            .get(net_class_index)
            .map_or_else(String::new, |class| class.name.clone());
        let attach_allowed = self.ast.structure.via_at_smd_allowed;
        let indices = self.via_padstack_indices.clone();
        for padstack_index in indices {
            let padstack = &self.padstacks[padstack_index];
            let via_attach_allowed = attach_allowed && padstack.attach_allowed;
            let via_name = if is_default_class {
                padstack.name.clone()
            } else {
                fr_rules::default_via_info_name(&padstack.name, &class_name)
            };
            let rules_padstack = padstack.to_rules_padstack(padstack_index);
            self.rules.via_infos.add(ViaInfo::new(
                via_name,
                rules_padstack,
                clearance_class,
                via_attach_allowed,
            ));
        }
    }

    /// §9.4 `Network.insertViaRules`: every named rule, then a synthesized default when none
    /// was added, then **every** net class is pointed at the default rule (net-class-specific
    /// rules are re-set afterwards by `insert_net_classes`).
    fn insert_via_rules(&mut self) {
        let mut rule_found = false;
        let via_rules = self.ast.via_rules.clone();
        for list in &via_rules {
            if list.len() < 2 {
                continue;
            }
            if self.add_via_rule(list) {
                rule_found = true;
            }
        }
        if !rule_found {
            self.rules.create_default_via_rule(0, "default");
        }
        let default_rule = self.rules.get_default_via_rule().cloned();
        for index in 0..self.rules.net_classes.count() {
            if let Some(class) = self.rules.net_classes.get_mut(index) {
                class.set_via_rule(default_rule.clone());
            }
        }
    }

    fn add_via_rule(&mut self, name_list: &[String]) -> bool {
        let rule_name = &name_list[0];
        let mut rule = ViaRule::new(rule_name.clone());
        let mut rule_ok = true;
        for via_name in &name_list[1..] {
            match self.rules.via_infos.get_by_name(via_name).cloned() {
                Some(info) => rule.append_via(info),
                None => {
                    self.warn("Network.insert_via_rules: viaInfo not found");
                    rule_ok = false;
                }
            }
        }
        if rule_ok {
            self.rules
                .via_rules
                .retain(|existing| existing.name != *rule_name);
            self.rules.via_rules.push(rule);
        }
        rule_ok
    }

    /// §7.4 `Network.insertNetClass`.
    fn insert_net_classes(&mut self) {
        let classes = self.ast.net_classes.clone();
        for class in &classes {
            self.insert_net_class(class);
        }
    }

    fn insert_net_class(&mut self, class: &DsnNetClass) {
        // §14.5: KiCad renames its `Default` class to `kicad_default` in DSN; folding it onto the
        // built-in default class is the only reason its width and clearance rules apply at all.
        let class_index = if is_kicad_default_net_class_name(&class.name) {
            0
        } else {
            self.rules.append_net_class_with_name(&class.name)
        };
        if let Some(name) = &class.trace_clearance_class {
            match self.rules.clearance_matrix.get_no(name) {
                Some(index) => {
                    if let Some(net_class) = self.rules.net_classes.get_mut(class_index) {
                        net_class.set_trace_clearance_class(index);
                    }
                }
                None => self.warn(format!(
                    "Network.insert_net_class: clearance class not found at '{}'",
                    class.name
                )),
            }
        }
        if let Some(name) = &class.via_rule {
            match self.rules.get_via_rule(name).cloned() {
                Some(rule) => {
                    if let Some(net_class) = self.rules.net_classes.get_mut(class_index) {
                        net_class.set_via_rule(Some(rule));
                    }
                }
                None => self.warn(format!(
                    "Network.insert_net_class: via rule not found at '{}'",
                    class.name
                )),
            }
        }
        if class.max_trace_length > 0.0 {
            let value = self.transform.dsn_to_board(class.max_trace_length);
            if let Some(net_class) = self.rules.net_classes.get_mut(class_index) {
                net_class.maximum_trace_length = value;
            }
        }
        if class.min_trace_length > 0.0 {
            let value = self.transform.dsn_to_board(class.min_trace_length);
            if let Some(net_class) = self.rules.net_classes.get_mut(class_index) {
                net_class.minimum_trace_length = value;
            }
        }
        for net_name in &class.net_list {
            let numbers: Vec<i32> = self
                .rules
                .nets
                .get_by_name(net_name)
                .iter()
                .map(|net| net.net_number)
                .collect();
            for net_number in numbers {
                if let Some(net) = self.rules.nets.get_mut(net_number) {
                    net.net_class_index = class_index;
                }
            }
        }

        let mut clearance_rule_found = false;
        for rule in &class.rules {
            match rule {
                DsnRule::Width(width) => {
                    // §3.4: the net-class rounding halves in **DSN** units first.
                    let half_width = self.transform.dsn_to_board(width / 2.0).jround() as i32;
                    if let Some(net_class) = self.rules.net_classes.get_mut(class_index) {
                        net_class.set_trace_half_width(half_width);
                    }
                }
                DsnRule::Clearance { value, class_pairs } => {
                    self.add_net_class_clearance_rule(class_index, *value, class_pairs, -1);
                    clearance_rule_found = true;
                }
            }
        }
        for layer_rule in &class.layer_rules {
            for layer_name in &layer_rule.layer_names {
                let Some(layer_index) = self.rules.layer_structure.get_no(layer_name) else {
                    self.warn(format!(
                        "Network.insert_net_class: layer not found at '{}'",
                        class.name
                    ));
                    continue;
                };
                for rule in &layer_rule.rules {
                    match rule {
                        DsnRule::Width(width) => {
                            let half_width =
                                self.transform.dsn_to_board(width / 2.0).jround() as i32;
                            if let Some(net_class) = self.rules.net_classes.get_mut(class_index) {
                                net_class.set_trace_half_width_on_layer(layer_index, half_width);
                            }
                        }
                        DsnRule::Clearance { value, class_pairs } => {
                            self.add_net_class_clearance_rule(
                                class_index,
                                *value,
                                class_pairs,
                                layer_index as i32,
                            );
                            clearance_rule_found = true;
                        }
                    }
                }
            }
        }
        if let Some(net_class) = self.rules.net_classes.get_mut(class_index) {
            net_class.pull_tight = class.pull_tight;
            net_class.shove_fixed = class.shove_fixed;
        }

        let mut via_infos_created = false;
        if clearance_rule_found && class_index != 0 {
            self.create_default_via_infos(class_index);
            via_infos_created = true;
        }
        if !class.use_via.is_empty() {
            self.create_via_rule(&class.use_via, class_index);
        } else if via_infos_created {
            let name = self
                .rules
                .net_classes
                .get(class_index)
                .map_or_else(String::new, |c| c.name.clone());
            self.rules.create_default_via_rule(class_index, &name);
        }
        if !class.use_layer.is_empty() {
            self.create_active_trace_layers(&class.use_layer, class_index);
        }
    }

    /// §7.5 `Network.addClearanceRule` — different from `Structure.setClearanceRule` in three
    /// ways: it splits on **every** underscore and requires exactly two parts, it has no
    /// `smd_to_turn_gap` or quoting branch, and its class names are namespaced per net class.
    fn add_net_class_clearance_rule(
        &mut self,
        class_index: usize,
        value: f64,
        class_pairs: &[String],
        layer_index: i32,
    ) {
        let clearance = self.transform.dsn_to_board(value).jround() as i32;
        let class_name = self
            .rules
            .net_classes
            .get(class_index)
            .map_or_else(String::new, |class| class.name.clone());
        let mut class_no = self.rules.clearance_matrix.get_no(&class_name);
        if class_no.is_none() {
            self.rules.clearance_matrix.append_class(&class_name);
            class_no = self.rules.clearance_matrix.get_no(&class_name);
            if let Some(index) = class_no {
                // Seed the new row and column with the maximum of the new value and whatever
                // `appendClass` inherited from class 1.
                let class_count = self.rules.clearance_matrix.class_count();
                for other in 1..class_count {
                    for layer in 0..self.layer_count {
                        let current = self
                            .rules
                            .clearance_matrix
                            .get_value(index, other, layer, false)
                            .max(clearance);
                        self.rules
                            .clearance_matrix
                            .set_value_on_layer(index, other, layer, current);
                        self.rules
                            .clearance_matrix
                            .set_value_on_layer(other, index, layer, current);
                    }
                }
                if let Some(net_class) = self.rules.net_classes.get_mut(class_index) {
                    net_class
                        .default_item_clearance_classes
                        .set_all(index as i32);
                }
            }
        }
        let Some(class_no) = class_no else { return };
        if let Some(net_class) = self.rules.net_classes.get_mut(class_index) {
            net_class.set_trace_clearance_class(class_no);
        }
        if class_pairs.is_empty() {
            if layer_index < 0 {
                self.rules
                    .clearance_matrix
                    .set_value(class_no, class_no, clearance);
            } else {
                self.rules.clearance_matrix.set_value_on_layer(
                    class_no,
                    class_no,
                    layer_index as usize,
                    clearance,
                );
            }
            return;
        }
        if contains_wire_clearance_pair(class_pairs) {
            for item_class_name in RESERVED_ITEM_CLASS_NAMES {
                self.net_class_clearance_class(class_index, item_class_name);
            }
        }
        for current in class_pairs {
            let Some((first, second)) = network_class_pair(current) else {
                continue;
            };
            let first_no = self.net_class_clearance_class(class_index, &first);
            let second_no = self.net_class_clearance_class(class_index, &second);
            self.write_clearance(first_no, second_no, layer_index, clearance);
        }
    }

    /// §7.6 `Network.getClearanceClass`: `"wire"` maps onto the net class itself, every other
    /// item class name becomes `<netClass>-<itemClass>` and inherits the parent's whole row.
    fn net_class_clearance_class(&mut self, class_index: usize, item_class_name: &str) -> usize {
        let net_class_name = self
            .rules
            .net_classes
            .get(class_index)
            .map_or_else(String::new, |class| class.name.clone());
        let new_class_name = net_class_item_clearance_class_name(&net_class_name, item_class_name);
        if let Some(found) = self.rules.clearance_matrix.get_no(&new_class_name) {
            return found;
        }
        self.rules.clearance_matrix.append_class(&new_class_name);
        let Some(result) = self.rules.clearance_matrix.get_no(&new_class_name) else {
            return 0;
        };
        let Some(parent) = self.rules.clearance_matrix.get_no(&net_class_name) else {
            self.warn(format!(
                "Network.get_clearance_class: clearance class not found at '{net_class_name}'"
            ));
            return result;
        };
        let class_count = self.rules.clearance_matrix.class_count();
        for other in 1..class_count {
            for layer in 0..self.layer_count {
                let value = self
                    .rules
                    .clearance_matrix
                    .get_value(parent, other, layer, false);
                self.rules
                    .clearance_matrix
                    .set_value_on_layer(result, other, layer, value);
                self.rules
                    .clearance_matrix
                    .set_value_on_layer(other, result, layer, value);
            }
        }
        if let Some(item_class) = fr_rules::reserved_item_class(item_class_name) {
            if let Some(net_class) = self.rules.net_classes.get_mut(class_index) {
                net_class
                    .default_item_clearance_classes
                    .set(item_class, result as i32);
            }
        }
        result
    }

    /// `Network.createViaRule`: a rule named after the net class, holding every `ViaInfo` whose
    /// clearance class equals the net class's `VIA` class **and** whose padstack name is listed.
    fn create_via_rule(&mut self, use_via: &[String], class_index: usize) {
        let name = self
            .rules
            .net_classes
            .get(class_index)
            .map_or_else(String::new, |class| class.name.clone());
        let default_via_class = self.rules.net_classes.get(class_index).map_or(1, |class| {
            class.default_item_clearance_classes.get(ItemClass::Via)
        }) as usize;
        let mut rule = ViaRule::new(name);
        for via_name in use_via {
            for index in 0..self.rules.via_infos.count() {
                let Some(info) = self.rules.via_infos.get(index) else {
                    continue;
                };
                if info.clearance_class() == default_via_class && info.padstack().name == *via_name
                {
                    let info = info.clone();
                    rule.append_via(info);
                }
            }
        }
        self.rules.via_rules.push(rule.clone());
        if let Some(net_class) = self.rules.net_classes.get_mut(class_index) {
            net_class.set_via_rule(Some(rule));
        }
    }

    /// §9.3 `Network.createActiveTraceLayers`: deactivate everything, activate the listed layers,
    /// then set every inactive layer's trace half width to 0.
    fn create_active_trace_layers(&mut self, use_layer: &[String], class_index: usize) {
        let indices: Vec<Option<usize>> = use_layer
            .iter()
            .map(|name| self.rules.layer_structure.get_no(name))
            .collect();
        let layer_count = self.layer_count;
        let Some(net_class) = self.rules.net_classes.get_mut(class_index) else {
            return;
        };
        for layer in 0..layer_count {
            net_class.set_active_routing_layer(layer, false);
        }
        for index in indices.into_iter().flatten() {
            net_class.set_active_routing_layer(index, true);
        }
        for layer in 0..layer_count {
            if !net_class.is_active_routing_layer(layer) {
                net_class.set_trace_half_width_on_layer(layer, 0);
            }
        }
    }

    /// §7.7 `Network.insertClassPairs`.
    ///
    /// **The shared-iterator pairing bug is reproduced.** Upstream writes `Iterator<String> it2 =
    /// it1;`, so the inner loop consumes the outer one: `(classes A B C)` produces exactly the
    /// pairs `(A,B)` and `(A,C)`, never `(B,C)`, and the outer loop ends after one turn. The
    /// obvious nested loop would produce a *different clearance matrix*; see
    /// `docs/PARITY-DIVERGENCES.md`.
    fn insert_class_pairs(&mut self) {
        let class_classes = self.ast.class_classes.clone();
        for class_class in &class_classes {
            let mut index = 0usize;
            while index < class_class.class_names.len() {
                let first_name = class_class.class_names[index].clone();
                index += 1;
                let Some(first_class) = self.resolve_net_class(&first_name) else {
                    self.warn("Network.insert_class_pairs: first class not found");
                    continue;
                };
                while index < class_class.class_names.len() {
                    let second_name = class_class.class_names[index].clone();
                    index += 1;
                    let Some(second_class) = self.resolve_net_class(&second_name) else {
                        self.warn("Network.insert_class_pairs: second class not found");
                        continue;
                    };
                    self.insert_class_pair_info(class_class, first_class, second_class);
                }
            }
        }
    }

    fn resolve_net_class(&self, name: &str) -> Option<usize> {
        if is_kicad_default_net_class_name(name) {
            return Some(0);
        }
        self.rules.net_classes.iter().position(|c| c.name == name)
    }

    fn insert_class_pair_info(&mut self, class_class: &DsnClassClass, first: usize, second: usize) {
        for rule in &class_class.rules {
            if let DsnRule::Clearance { value, class_pairs } = rule {
                self.add_mixed_clearance_rule(first, second, *value, class_pairs, -1);
            } else {
                self.warn("Network.insert_class_pair_info: unexpected rule");
            }
        }
        for layer_rule in &class_class.layer_rules {
            for layer_name in &layer_rule.layer_names {
                let Some(layer_index) = self.rules.layer_structure.get_no(layer_name) else {
                    self.warn(format!(
                        "Network.insert_class_pair_info: layer not found at '{layer_name}'"
                    ));
                    continue;
                };
                for rule in &layer_rule.rules {
                    if let DsnRule::Clearance { value, class_pairs } = rule {
                        self.add_mixed_clearance_rule(
                            first,
                            second,
                            *value,
                            class_pairs,
                            layer_index as i32,
                        );
                    } else {
                        self.warn("Network.insert_class_pair_info: unexpected layer rule type");
                    }
                }
            }
        }
    }

    /// `Network.addMixedClearanceRule`: with a `(type a_b)` the rule is applied **twice**, once
    /// as `(first.a, second.b)` and once as `(second.a, first.b)`.
    fn add_mixed_clearance_rule(
        &mut self,
        first: usize,
        second: usize,
        value: f64,
        class_pairs: &[String],
        layer_index: i32,
    ) {
        let clearance = self.transform.dsn_to_board(value).jround() as i32;
        let first_name = self
            .rules
            .net_classes
            .get(first)
            .map_or_else(String::new, |c| c.name.clone());
        let second_name = self
            .rules
            .net_classes
            .get(second)
            .map_or_else(String::new, |c| c.name.clone());
        let first_no = match self.rules.clearance_matrix.get_no(&first_name) {
            Some(index) => index,
            None => {
                self.rules.clearance_matrix.append_class(&first_name);
                self.rules.clearance_matrix.get_no(&first_name).unwrap_or(0)
            }
        };
        let second_no = match self.rules.clearance_matrix.get_no(&second_name) {
            Some(index) => index,
            None => {
                self.rules.clearance_matrix.append_class(&second_name);
                self.rules
                    .clearance_matrix
                    .get_no(&second_name)
                    .unwrap_or(0)
            }
        };
        if class_pairs.is_empty() {
            self.write_clearance(first_no, second_no, layer_index, clearance);
            return;
        }
        for current in class_pairs {
            let Some((left, right)) = network_class_pair(current) else {
                continue;
            };
            for direction in 0..2 {
                let (a_class, b_class) = if direction == 0 {
                    (first, second)
                } else {
                    (second, first)
                };
                let a = self.net_class_clearance_class(a_class, &left);
                let b = self.net_class_clearance_class(b_class, &right);
                self.write_clearance(a, b, layer_index, clearance);
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // §9.2 — components, the mirroring math
    // -----------------------------------------------------------------------------------------

    fn insert_components(&mut self) {
        self.build_pin_net_index();
        let placements = self.ast.placements.clone();
        for placement in &placements {
            for location in &placement.locations {
                self.insert_component(&placement.lib_name, location);
            }
        }
    }

    fn insert_component(&mut self, lib_key: &str, location: &ComponentLocation) {
        // `Network.insertComponent` (`Network.java:804-822`) resolves the image name **twice**,
        // once per side, requires both to hit, and hands both to `Component`, which then answers
        // `getPackage()` with the one matching the placement side (`board/Component.java:240`).
        //
        // The two lookups are not interchangeable, and that is the whole point: `Packages.get`
        // returns a same-side hit immediately but keeps overwriting `other_side_package` as it
        // scans, first over the literal name and then over the name with a trailing `::<digits>`
        // stripped. So `get("Foo::1", false)` on a library whose `Foo` and `Foo::1` are both
        // front-side packages answers **`Foo`** — the `::1` hit is recorded as the other-side
        // candidate and then replaced by the base-name one. A back-placed component therefore
        // silently lands on the base variant, which is exactly what the release dump shows
        // (Issue066: `C3`, `C10`, `C12` are placed against `…::1` and written under `…`).
        let front = self.packages_get(lib_key, true);
        let back = self.packages_get(lib_key, false);
        let (Some(front_index), Some(back_index)) = (front, back) else {
            self.warn(format!(
                "Network.insert_component: component package not found at '{lib_key}'"
            ));
            return;
        };
        let package_index = if location.is_front {
            front_index
        } else {
            back_index
        };
        let component_no = self.next_component_no;
        self.next_component_no += 1;
        // `Component`'s constructor normalizes the rotation into [0, 360) by repeated ±360.
        let mut rotation = location.rotation;
        while rotation >= 360.0 {
            rotation -= 360.0;
        }
        while rotation < 0.0 {
            rotation += 360.0;
        }
        let board_location = location
            .coor
            .map(|coor| self.transform.dsn_to_board_point(coor[0], coor[1]).jround());
        let mut component = ImportedComponent {
            name: location.name.clone(),
            location: board_location,
            rotation,
            is_front: location.is_front,
            package_index,
            position_fixed: location.position_fixed,
            part_number: location.part_number.clone(),
            pin_centers: Vec::new(),
            pin_clearance_classes: Vec::new(),
            keepout_clearance_classes: KeepoutClearanceClasses::default(),
        };
        let Some(board_location) = board_location else {
            // An unplaced component exists but contributes no board items.
            self.components.push(component);
            return;
        };
        let translation = board_location.difference_by_int(&IntPoint::new(0, 0));
        let fixed = if location.position_fixed {
            FixedState::SystemFixed
        } else {
            FixedState::Unfixed
        };
        let flip_first = self.ast.structure.flip_style_rotate_first;
        let on_front = location.is_front;
        let mirror_on_y_axis = !on_front && !flip_first;
        let flip_after_rotate = !on_front && flip_first;

        let pin_count = self.packages[package_index].pins.len();
        for pin_index in 0..pin_count {
            let pin = self.packages[package_index].pins[pin_index].clone();
            let padstack_index = pin.padstack_index;

            // §9.2.1 the pin centre.
            let mut rel_location = pin.relative_location;
            if mirror_on_y_axis {
                rel_location = rel_location.mirror_at_y_axis_int();
            }
            rel_location = rotate_vector(rel_location, rotation);
            if flip_after_rotate {
                rel_location = rel_location.mirror_at_y_axis_int();
            }
            let mut center = board_location.translate_by_int(&rel_location);

            // §9.2.2 the pad shapes, all layers at once, and §9.2.3 the layer flip.
            let placed = self.placed_pad_shapes(
                padstack_index,
                pin.rotation,
                rotation,
                mirror_on_y_axis,
                flip_after_rotate,
                on_front,
                pin.relative_location,
                rel_location,
            );
            // `Pin.get_center` (`Pin.java:86-118`): a pad that does not cover the nominal centre
            // moves the centre onto its own centre of gravity. The shapes are pin-relative now, so
            // the test runs against the placed copy — `first_shape + centre` is the absolute shape
            // `Pin.get_shape` would have returned.
            if let Some(first_shape) = placed.shapes.iter().flatten().next() {
                let absolute = first_shape
                    .translate_by(&Vector::Int(center.difference_by_int(&IntPoint::ZERO)));
                if !absolute.contains_inside(&center) {
                    center = absolute.centre_of_gravity().jround();
                }
            }

            // §9.2.4 nets and the clearance class.
            let net_numbers = self.pin_nets(&location.name, &pin.name);
            let net_class_index = net_numbers
                .first()
                .and_then(|net_no| self.rules.nets.get(*net_no))
                .map_or(0, |net| net.net_class_index);
            let mut clearance_class = -1i32;
            if let Some(info) = location.pin_infos.iter().find(|info| info.name == pin.name) {
                clearance_class = self
                    .rules
                    .clearance_matrix
                    .get_no(&info.clearance_class)
                    .map_or(-1, |index| index as i32);
            }
            if clearance_class < 0 {
                // SMD versus through-hole is decided purely by `fromLayer() == toLayer()`.
                let item_class = if self.padstacks[padstack_index].is_smd() {
                    ItemClass::Smd
                } else {
                    ItemClass::Pin
                };
                clearance_class = self
                    .rules
                    .net_classes
                    .get(net_class_index)
                    .map_or(1, |class| {
                        class.default_item_clearance_classes.get(item_class)
                    });
            }

            let board_padstack_no = self.intern_padstack(placed);
            let layers = self
                .board_padstacks
                .get(board_padstack_no)
                .map_or(LayerRange::single(0), |p| {
                    LayerRange::new(p.from_layer, p.to_layer)
                });
            component.pin_centers.push(center);
            component.pin_clearance_classes.push(clearance_class);
            let item = Item::new(
                NetNos::from_slice(&net_numbers),
                clearance_class,
                fixed,
                ItemKind::Pin(Pin {
                    center,
                    padstack_no: board_padstack_no,
                    pin_no: pin_index as u32,
                    layers,
                }),
            )
            .with_component(component_no);
            self.items.push(Pending::Plain(item));
        }

        // §9.2.5 package keepouts.
        for kind_index in 0..3usize {
            let (keepouts, infos) = match kind_index {
                0 => (
                    self.packages[package_index].keepouts.clone(),
                    &location.keepout_infos,
                ),
                1 => (
                    self.packages[package_index].via_keepouts.clone(),
                    &location.via_keepout_infos,
                ),
                _ => (
                    self.packages[package_index].place_keepouts.clone(),
                    &location.place_keepout_infos,
                ),
            };
            let kind = match kind_index {
                0 => ObstacleKind::Keepout,
                1 => ObstacleKind::ViaKeepout,
                _ => ObstacleKind::ComponentKeepout,
            };
            for keepout in &keepouts {
                let mut layer = keepout.layer;
                if layer >= self.layer_count as i32 {
                    self.warn("Network.insert_component: keepout layer is too big");
                    // No area reaches the board, so `Component.writeKeepoutInfos`'s lookup finds
                    // nothing; class 0 is its "skip" sentinel.
                    component.keepout_clearance_classes.push(kind_index, 0);
                    continue;
                }
                if layer >= 0 && !on_front {
                    layer = self.layer_count as i32 - keepout.layer - 1;
                }
                let mut clearance_class = self
                    .rules
                    .get_default_net_class()
                    .default_item_clearance_classes
                    .get(ItemClass::Area);
                if let Some(name) = &keepout.name {
                    if let Some(info) = infos.iter().find(|info| info.name == *name) {
                        if let Some(index) =
                            self.rules.clearance_matrix.get_no(&info.clearance_class)
                        {
                            if index > 0 {
                                clearance_class = index as i32;
                            }
                        }
                    }
                }
                component
                    .keepout_clearance_classes
                    .push(kind_index, clearance_class);
                let placed_area = placed_area(&keepout.area, translation, rotation, !on_front);
                if layer >= 0 {
                    self.push_placed_obstacle(
                        placed_area,
                        layer,
                        kind,
                        clearance_class,
                        fixed,
                        component_no,
                    );
                } else {
                    for index in 0..self.layer_count {
                        if self.dsn_layers.layers[index].is_signal {
                            self.push_placed_obstacle(
                                placed_area.clone(),
                                index as i32,
                                kind,
                                clearance_class,
                                fixed,
                                component_no,
                            );
                        }
                    }
                }
            }
        }

        // §9.2.6 the package outline; classification is rendering-only.
        let package = self.packages[package_index].clone();
        for (index, outline) in package.outlines.iter().enumerate() {
            let Some(shape) = outline else { continue };
            let placed_area = placed_area(shape, translation, rotation, !on_front);
            let _ = index;
            self.items.push(Pending::Plain(
                Item::new(
                    NetNos::default(),
                    0,
                    fixed,
                    ItemKind::ComponentOutline(ComponentOutline {
                        area: placed_area,
                        is_front: on_front,
                    }),
                )
                .with_component(component_no),
            ));
        }
        self.components.push(component);
    }

    /// `NetList.getNets(componentName, pinName)` — every net whose pin set contains the pin, in
    /// `Net.Id` order (name, then subnet).
    ///
    /// Upstream walks **every** net and does a `TreeSet.contains` per net, once per pin of every
    /// component: `O(pins x nets x log pins)`, and the quadratic hot spot of DSN import
    /// (`dsn-board-semantics.md` §17.2). Inverting the netlist once keeps the order — the index
    /// is built by walking the netlist in `Net.Id` order, so each pin's net list comes out sorted
    /// the same way — and makes the lookup `O(1)`.
    fn build_pin_net_index(&mut self) {
        let mut index: HashMap<NetPin, Vec<i32>> = HashMap::new();
        for ((name, subnet), pins) in &self.netlist {
            let Some(net) = self.rules.nets.get_by_name_and_subnet(name, *subnet) else {
                continue;
            };
            let net_number = net.net_number;
            for pin in pins {
                index.entry(pin.clone()).or_default().push(net_number);
            }
        }
        self.pin_net_index = index;
    }

    fn pin_nets(&self, component_name: &str, pin_name: &str) -> Vec<i32> {
        let probe = NetPin {
            component_name: component_name.to_string(),
            pin_name: pin_name.to_string(),
        };
        self.pin_net_index.get(&probe).cloned().unwrap_or_default()
    }

    /// §9.2.2 and §9.2.3: the pad shapes of one placed pin, relative to the **pin centre**, plus
    /// the resolved board layer span.
    ///
    /// # The frame, and the wave-5 pin-offset defect
    ///
    /// `Pin.get_shape` (`Pin.java:163-239`) builds an *absolute* shape: rotate the pad by the pin
    /// rotation, mirror it, translate it by the package-relative pin location, rotate by the
    /// component rotation, flip, and finally translate by the component location. `fr_board::item::Pin`
    /// stores no placement — `search_tree::drill_shapes` translates the padstack by `Pin::center`
    /// — so what belongs in the table is upstream's absolute shape minus the pin centre:
    ///
    /// ```text
    ///   centre = location + F(R(M(rel)))                     `Pin.relative_location`
    ///   shape  = location + F(R(M(pad) + rel))               `Pin.get_shape`
    ///   stored = shape - centre = F(R(M(pad) + rel)) - F(R(M(rel)))
    /// ```
    ///
    /// which is what `placed_relative_location` — the caller's already-computed `F(R(M(rel)))` —
    /// is for. Leaving that subtraction out was the defect: [`PadstackKey`] does not carry the pin
    /// offset, so the **first** pin interned under a key donated its own package offset to every
    /// later pin sharing the key, and most pins' copper landed a *foreign* pin's offset away from
    /// their own centre. Issue027's `R2` is the clean witness — two pins at opposite
    /// `rel_location`s, one baked offset.
    ///
    /// # Why the offset is in the key when the rotation is oblique
    ///
    /// At a right-angle component rotation `F ∘ R` is exact integer arithmetic and therefore
    /// linear, so `F(R(M(pad) + rel)) - F(R(M(rel))) = F(R(M(pad)))`: the result does not depend on
    /// `rel` at all and one interned entry serves every pin of the footprint. At any other angle
    /// `rotate_approx` rounds each corner, and rounding does not distribute over the sum — a pad
    /// translated to two different package positions comes back with corners up to one board unit
    /// apart. Upstream recomputes per pin and so must we, so `PadstackKey::pin_offset` carries
    /// `rel` in exactly that case, which is the only case where two pins of a footprint can
    /// legitimately want different table entries.
    #[allow(clippy::too_many_arguments)]
    fn placed_pad_shapes(
        &self,
        padstack_index: usize,
        pin_rotation: f64,
        component_rotation: f64,
        mirror_on_y_axis: bool,
        flip_after_rotate: bool,
        on_front: bool,
        relative_location: IntVector,
        placed_relative_location: IntVector,
    ) -> PlacedPadstack {
        let padstack = &self.padstacks[padstack_index];
        let from_layer = padstack.from_layer();
        let to_layer = padstack.to_layer();
        let placed_absolute = padstack.placed_absolute;
        let layer_count = self.layer_count;
        let (first_layer, last_layer) = if on_front || placed_absolute {
            (from_layer, to_layer)
        } else {
            (layer_count - to_layer - 1, layer_count - from_layer - 1)
        };
        let rel_location = if mirror_on_y_axis {
            relative_location.mirror_at_y_axis_int()
        } else {
            relative_location
        };
        let mut shapes = Vec::with_capacity(to_layer - from_layer + 1);
        for shape_index in 0..=(to_layer - from_layer) {
            let padstack_layer = if on_front || placed_absolute {
                shape_index + from_layer
            } else {
                // `Pin.getPadstackLayer`: `boardLayerCount - index - firstLayer() - 1`, where
                // `firstLayer()` is the *drill item's* first layer, not the padstack's. Expanding
                // it gives `toLayer - index`, which is what makes the shape at board layer
                // `first_layer + index` come from the mirrored padstack layer.
                to_layer.saturating_sub(shape_index)
            };
            let Some(shape) = padstack.shapes.get(padstack_layer).and_then(Option::as_ref) else {
                shapes.push(None);
                continue;
            };
            let mut shape = shape.clone();
            // 1. the pin's own rotation about the origin
            if pin_rotation % 90.0 == 0.0 {
                let factor = (pin_rotation as i32) / 90;
                if factor != 0 {
                    shape = shape.turn_90_degree(factor, &IntPoint::new(0, 0));
                }
            } else {
                shape = shape.rotate_approx(pin_rotation.to_radians(), &FloatPoint::new(0.0, 0.0));
            }
            // 2. mirror-first flip of the pad itself
            if mirror_on_y_axis {
                shape = shape.mirror_vertical(&IntPoint::new(0, 0));
            }
            // 3. translate into package coordinates
            shape = shape.translate_by(&Vector::Int(rel_location));
            // 4. the component rotation about the origin
            if component_rotation % 90.0 == 0.0 {
                let factor = (component_rotation as i32) / 90;
                if factor != 0 {
                    shape = shape.turn_90_degree(factor, &IntPoint::new(0, 0));
                }
            } else {
                shape = shape
                    .rotate_approx(component_rotation.to_radians(), &FloatPoint::new(0.0, 0.0));
            }
            // 5. the rotate-first flip
            if flip_after_rotate {
                shape = shape.mirror_vertical(&IntPoint::new(0, 0));
            }
            // 6. out of the component frame and into the pin frame, which is where a padstack the
            // search tree translates by `Pin::center` has to live. Steps 3 and 6 cancel exactly at
            // a right-angle rotation and do not at any other, which is the whole reason step 3
            // survives at all.
            if placed_relative_location.x != 0 || placed_relative_location.y != 0 {
                shape = shape.translate_by(&Vector::Int(IntVector::new(
                    -placed_relative_location.x,
                    -placed_relative_location.y,
                )));
            }
            // Belt and braces: a shape that escaped the coordinate envelope would overflow the
            // search tree's `to_flavor`. Upstream's `int` arithmetic wraps silently instead;
            // dropping the pad is the direction that keeps the board loadable.
            let pad_bounds = shape.bounding_box();
            let limit = fr_geometry::planar::Limits::CRIT_INT;
            if pad_bounds.ll.x < -limit
                || pad_bounds.ll.y < -limit
                || pad_bounds.ur.x > limit
                || pad_bounds.ur.y > limit
            {
                shapes.push(None);
                continue;
            }
            shapes.push(Some(shape));
        }
        PlacedPadstack {
            key: PadstackKey {
                padstack_index,
                pin_rotation_bits: pin_rotation.to_bits(),
                component_rotation_bits: component_rotation.to_bits(),
                mirror_on_y_axis,
                flip_after_rotate,
                on_front,
                pin_offset: (component_rotation % 90.0 != 0.0).then_some(relative_location),
            },
            first_layer: first_layer as i32,
            last_layer: last_layer as i32,
            shapes,
            drill_radius: self.padstacks[padstack_index]
                .to_rules_padstack(padstack_index)
                .get_drill_radius()
                .jround() as i32,
        }
    }

    /// Interns a placed padstack in the board's padstack table.
    ///
    /// `fr_board::item::Pin` carries a padstack index and a centre but no placement, so the rotation and
    /// the mirroring have to be baked into the stored geometry. Interning by placement is the
    /// `PadShapeCache` `dsn-board-semantics.md` §17.2 asks for: hundreds of instances of the same
    /// footprint at the same rotation share one entry, where upstream recomputes per instance.
    fn intern_padstack(&mut self, placed: PlacedPadstack) -> u32 {
        if let Some(index) = self.padstack_cache.get(&placed.key) {
            return *index;
        }
        let board_padstack = fr_board::Padstack {
            from_layer: placed.first_layer,
            to_layer: placed.last_layer,
            shapes: placed
                .shapes
                .iter()
                .map(|slot| slot.as_ref().map(PadGeometry::to_tile_shape))
                .collect(),
            drill_radius: placed.drill_radius,
        };
        let index = self.board_padstacks.push(board_padstack);
        self.padstack_cache.insert(placed.key, index);
        index
    }

    // -----------------------------------------------------------------------------------------
    // §10 — wiring
    // -----------------------------------------------------------------------------------------

    fn build_wiring(&mut self) {
        let wires = self.ast.wires.clone();
        for wire in &wires {
            self.insert_wire(wire);
        }
        let vias = self.ast.wiring_vias.clone();
        for via in &vias {
            self.insert_wiring_via(via);
        }
    }

    /// `Wiring.getSubnets`: subnet > 0 resolves exactly, subnet 0 resolves to **all** subnets of
    /// that name.
    fn get_subnets(&self, net: Option<&NetRef>) -> Vec<i32> {
        let Some(net) = net else { return Vec::new() };
        if net.subnet_number > 0 {
            self.rules
                .nets
                .get_by_name_and_subnet(&net.name, net.subnet_number)
                .map(|n| vec![n.net_number])
                .unwrap_or_default()
        } else {
            self.rules
                .nets
                .get_by_name(&net.name)
                .iter()
                .map(|n| n.net_number)
                .collect()
        }
    }

    fn insert_wire(&mut self, wire: &DsnWire) {
        let net_numbers = self.get_subnets(wire.net.as_ref());
        let net_class_index = net_numbers
            .last()
            .and_then(|net_no| self.rules.nets.get(*net_no))
            .map_or(0, |net| net.net_class_index);
        let mut clearance_class = match &wire.clearance_class_name {
            Some(name) => self
                .rules
                .clearance_matrix
                .get_no(name)
                .map_or(-1, |index| index as i32),
            None => -1,
        };
        let fixed = fixed_state(wire.fixed);

        let (layer_index, half_width) = match (&wire.path, &wire.border_shape) {
            (Some(path), _) => (
                path.layer().no,
                (self.transform.dsn_to_board(path.path_width() / 2.0)).jround() as i32,
            ),
            (None, Some(border)) => (border.layer().no, 0),
            (None, None) => return,
        };
        if layer_index < 0 || layer_index >= self.layer_count as i32 {
            self.warn("Wiring: wire ignored — unknown layer");
            return;
        }

        // Not implemented: `Wiring.tryCorrectNet` (`Wiring.java:583-599`), which adopts the net
        // of a single-net contact when the inserted item ended up with none. It needs the
        // board's contact query, and the only files it helps are Mentor "turret" exports.
        if let Some(border) = &wire.border_shape {
            if clearance_class < 0 {
                clearance_class = self
                    .rules
                    .net_classes
                    .get(net_class_index)
                    .map_or(1, |class| {
                        class.default_item_clearance_classes.get(ItemClass::Area)
                    });
            }
            let Some(shape) = transform_to_board(border, &self.transform) else {
                return;
            };
            if !wire.windows.is_empty() {
                self.warn(format!(
                    "Wiring: conduction area with {} window(s); holes are not yet modelled",
                    wire.windows.len()
                ));
            }
            self.push_conduction_area(
                &shape,
                layer_index,
                NetNos::from_slice(&net_numbers),
                clearance_class,
                fixed,
            );
            return;
        }

        if clearance_class < 0 {
            clearance_class = self
                .rules
                .net_classes
                .get(net_class_index)
                .map_or(1, |class| {
                    class.default_item_clearance_classes.get(ItemClass::Trace)
                });
        }
        match wire.path.as_ref() {
            Some(DsnShape::PolygonPath { coor, .. }) => {
                let mut corners = Vec::with_capacity(coor.len() / 2);
                for pair in coor.chunks_exact(2) {
                    let point = self.transform.dsn_to_board_point(pair[0], pair[1]);
                    // KiCad occasionally exports courtyard-derived geometry outside the outline;
                    // upstream drops the whole wire. The box was already inflated by 1000 units.
                    if !float_point_in_box(&point, &self.bounds) {
                        self.warn(format!(
                            "Wiring: wire corner ({}, {}) is outside board bounds",
                            pair[0] as i64, pair[1] as i64
                        ));
                        return;
                    }
                    corners.push(point.jround());
                }
                let polygon = fr_geometry::planar::Polygon::new(
                    &corners.iter().map(|p| Point::Int(*p)).collect::<Vec<_>>(),
                );
                let polygon_corners = polygon.corner_array();
                let has_distinct = polygon_corners
                    .iter()
                    .skip(1)
                    .any(|corner| *corner != polygon_corners[0]);
                if polygon_corners.len() < 2 || !has_distinct {
                    self.warn(
                        "Wiring: degenerate wire trace skipped (all corners identical)".to_string(),
                    );
                    return;
                }
                let polyline = Polyline::new_from_polygon(&polygon);
                self.items.push(Pending::Trace {
                    polyline,
                    layer: layer_index,
                    half_width,
                    nets: NetNos::from_slice(&net_numbers),
                    clearance_class,
                    fixed,
                });
            }
            Some(DsnShape::PolylinePath { coor, .. }) => {
                // No bounds check and no degeneracy check on this path, exactly as upstream.
                let mut lines = Vec::with_capacity(coor.len() / 4);
                for group in coor.chunks_exact(4) {
                    let a = self
                        .transform
                        .dsn_to_board_point(group[0], group[1])
                        .jround();
                    let b = self
                        .transform
                        .dsn_to_board_point(group[2], group[3])
                        .jround();
                    lines.push(Line { a, b });
                }
                if lines.len() < 3 {
                    self.warn("Wiring: polyline_path with fewer than three lines skipped");
                    return;
                }
                let polyline = Polyline::new_from_lines(&lines);
                self.items.push(Pending::Trace {
                    polyline,
                    layer: layer_index,
                    half_width,
                    nets: NetNos::from_slice(&net_numbers),
                    clearance_class,
                    fixed,
                });
            }
            _ => {}
        }
    }

    fn insert_wiring_via(&mut self, via: &DsnWiringVia) {
        let Some(padstack_index) = self.padstack_index(&via.padstack_name) else {
            self.warn(format!(
                "Wiring: via padstack '{}' not found",
                via.padstack_name
            ));
            return;
        };
        // DIVERGENCE (see docs/PARITY-DIVERGENCES.md, "Wiring.readViaScope multi-subnet net
        // array"): v2 fills `netNumbers[currentIndex]` without ever incrementing `currentIndex`,
        // so a via on a multi-subnet net gets `[lastSubnet, 0, 0, …]` and distinct vias silently
        // share net 0. v1.9 increments; that is the intent and what we implement.
        let net_numbers = self.get_subnets(via.net.as_ref());
        if via.net.is_some() && net_numbers.is_empty() {
            self.warn(format!(
                "Wiring: via net '{}' not found",
                via.net
                    .as_ref()
                    .map(|n| n.name.as_str())
                    .unwrap_or_default()
            ));
        }
        let net_class_index = net_numbers
            .last()
            .and_then(|net_no| self.rules.nets.get(*net_no))
            .map_or(0, |net| net.net_class_index);
        let mut clearance_class = match &via.clearance_class_name {
            Some(name) => self
                .rules
                .clearance_matrix
                .get_no(name)
                .map_or(-1, |index| index as i32),
            None => -1,
        };
        if clearance_class < 0 {
            clearance_class = self
                .rules
                .net_classes
                .get(net_class_index)
                .map_or(1, |class| {
                    class.default_item_clearance_classes.get(ItemClass::Via)
                });
        }
        let center = self
            .transform
            .dsn_to_board_point(via.location[0], via.location[1])
            .jround();
        let padstack = &self.padstacks[padstack_index];
        let first_layer = padstack.from_layer() as i32;
        let last_layer = padstack.to_layer() as i32;
        let attach_allowed = self.ast.structure.via_at_smd_allowed && padstack.attach_allowed;

        // `viaExists`: a duplicate via at the same centre, with the same nets and the same layer
        // span, is skipped with a warning (v2 only).
        let duplicate = self.items.iter().any(|pending| match pending {
            Pending::Via { via, nets, .. } => {
                via.center == center
                    && via.layers.first == first_layer
                    && via.layers.last == last_layer
                    && nets.as_slice() == net_numbers.as_slice()
            }
            _ => false,
        });
        if duplicate {
            self.warn(format!(
                "Wiring: duplicate via skipped at ({}, {})",
                center.x, center.y
            ));
            return;
        }

        let placed = self.placed_pad_shapes(
            padstack_index,
            0.0,
            0.0,
            false,
            false,
            true,
            IntVector::new(0, 0),
            IntVector::new(0, 0),
        );
        let board_padstack_no = self.intern_padstack(placed);
        self.items.push(Pending::Via {
            via: Via {
                center,
                padstack_no: board_padstack_no,
                layers: LayerRange::new(first_layer, last_layer),
                attach_allowed,
                is_escape_via: false,
                escape_via_smd_layer: -1,
            },
            nets: NetNos::from_slice(&net_numbers),
            clearance_class,
            fixed: fixed_state(via.fixed),
        });
    }

    // -----------------------------------------------------------------------------------------
    // item helpers and the final board assembly
    // -----------------------------------------------------------------------------------------

    fn push_obstacle(
        &mut self,
        shape: &BoardShape,
        layer: i32,
        kind: ObstacleKind,
        clearance_class: i32,
        fixed: FixedState,
        component_no: i32,
    ) {
        let area = PlacedArea::new(board_shape_to_area_shape(shape));
        self.push_placed_obstacle(area, layer, kind, clearance_class, fixed, component_no);
    }

    fn push_placed_obstacle(
        &mut self,
        area: PlacedArea,
        layer: i32,
        kind: ObstacleKind,
        clearance_class: i32,
        fixed: FixedState,
        component_no: i32,
    ) {
        let item = Item::new(
            NetNos::default(),
            clearance_class,
            fixed,
            ItemKind::ObstacleArea(ObstacleArea { area, layer, kind }),
        )
        .with_component(component_no);
        self.items.push(Pending::Plain(item));
    }

    /// §11.3: every conduction area the importer creates passes `is_obstacle = false`, so an
    /// imported pour is a connectable target but never blocks a foreign net.
    fn push_conduction_area(
        &mut self,
        shape: &BoardShape,
        layer: i32,
        nets: NetNos,
        clearance_class: i32,
        fixed: FixedState,
    ) {
        let item = Item::new(
            nets,
            clearance_class,
            fixed,
            ItemKind::ConductionArea(ConductionArea {
                area: PlacedArea::new(board_shape_to_area_shape(shape)),
                layer,
                is_obstacle: false,
                is_filled: true,
            }),
        );
        self.items.push(Pending::Plain(item));
    }

    fn finish(mut self) -> ImportedDesign {
        let class_count = self.rules.clearance_matrix.class_count();
        let mut clearances = fr_board::ClearanceTable::new(class_count, self.layer_count.max(1));
        for i in 0..class_count {
            for j in 0..class_count {
                for layer in 0..self.layer_count {
                    let value = self.rules.clearance_matrix.get_value(i, j, layer, false);
                    clearances.set_value_on_layer(i as i32, j as i32, layer as i32, value);
                }
            }
        }
        let board_layers = fr_board::LayerStructure::new(
            self.ast
                .structure
                .layers
                .iter()
                .map(|layer| fr_board::Layer {
                    name: layer.name.clone(),
                    is_signal: layer.is_signal,
                })
                .collect(),
        );
        let mut board = Board::new(board_layers, clearances);
        *board.padstacks_mut() = std::mem::take(&mut self.board_padstacks);
        board.set_angle_restriction(match self.rules.get_trace_angle_restriction() {
            fr_rules::AngleRestriction::NinetyDegree => fr_board::AngleRestriction::Ninety,
            fr_rules::AngleRestriction::FortyFiveDegree => fr_board::AngleRestriction::FortyFive,
            fr_rules::AngleRestriction::None => fr_board::AngleRestriction::None,
        });

        // §11.6 runs on the finished item set. It is applied to the pending list rather than to
        // the board because it mutates fixed states, and the board exposes no in-place item
        // mutation — the information it reads (traces per layer, conduction areas, the outline
        // area) is identical either way.
        let adjusted = if self.ast.structure.autoroute_settings.is_none() {
            adjust_plane_autoroute_settings(
                &mut self.items,
                &mut self.rules,
                &self.outline_shapes,
                self.layer_count,
            )
        } else {
            false
        };

        let items = std::mem::take(&mut self.items);
        for pending in items {
            match pending {
                Pending::Plain(item) => {
                    board.insert_item(item);
                }
                Pending::Trace {
                    polyline,
                    layer,
                    half_width,
                    nets,
                    clearance_class,
                    fixed,
                } => {
                    // §10.4: the wiring reader deliberately inserts without cleaning and defers
                    // normalization to the end of the scope, because cycles would otherwise be
                    // removed prematurely.
                    board.insert_trace_without_cleaning(
                        polyline,
                        layer,
                        half_width,
                        nets,
                        clearance_class,
                        fixed,
                    );
                }
                Pending::Via {
                    via,
                    nets,
                    clearance_class,
                    fixed,
                } => {
                    board.insert_via(via, nets, clearance_class, fixed);
                }
            }
        }

        // TODO(fr-board): `RoutingBoard.reduceNetsOfRouteItems()` belongs here, right after
        // normalization — `HeadlessBoardManager.applyImmediatePostLoadProcessing` runs it on
        // every freshly loaded board. It trims a trace or via with more than one net down to the
        // nets its normal contacts carry, skipping SYSTEM_FIXED items. The contact query already
        // exists (`Board::normal_contacts_at`); the missing piece is an in-place net-array
        // mutation, because remove + re-insert would renumber the items.
        //
        // `board.normalizeAllTraces()` at the end of the wiring scope, wrapped upstream so a
        // failure is a warning rather than a failed import. Upstream groups the traces into a
        // `HashMap<Integer, List<Trace>>` and iterates its entry set, i.e. in hash order;
        // normalization is independent per net, so iterating the nets in creation order instead
        // is equivalent and deterministic.
        let net_numbers: Vec<i32> = self.rules.nets.iter().map(|net| net.net_number).collect();
        for net_no in net_numbers {
            board.normalize_traces(net_no);
        }

        ImportedDesign {
            design_name: self.ast.design_name.clone(),
            unit: self.ast.unit,
            resolution: self.ast.resolution,
            scale_factor: self.transform.scale_factor(),
            transform: self.transform,
            bounds: self.bounds,
            board,
            rules: self.rules,
            padstacks: self.padstacks,
            packages: self.packages,
            components: self.components,
            via_padstack_indices: self.via_padstack_indices,
            outline_shapes: self.outline_shapes,
            outline_holes: self.outline_holes,
            flip_style_rotate_first: self.ast.structure.flip_style_rotate_first,
            via_at_smd_allowed: self.ast.structure.via_at_smd_allowed,
            autoroute_settings: self.ast.structure.autoroute_settings.clone(),
            string_quote: self.ast.string_quote.clone(),
            host_cad: self.ast.host_cad.clone(),
            host_version: self.ast.host_version.clone(),
            warnings: self.warnings,
            plane_autoroute_settings_adjusted: adjusted,
        }
    }
}

struct PlacedPadstack {
    key: PadstackKey,
    first_layer: i32,
    last_layer: i32,
    shapes: Vec<Option<PadGeometry>>,
    drill_radius: i32,
}

/// §11.6 `DsnFile.adjustPlaneAutorouteSettings`, run only when the file had no
/// `(autoroute_settings …)` scope — which, thanks to the guard bug, is most third-party files.
fn adjust_plane_autoroute_settings(
    items: &mut [Pending],
    rules: &mut BoardRules,
    outline_shapes: &[BoardShape],
    layer_count: usize,
) -> bool {
    if layer_count <= 2 {
        return false;
    }
    for index in 0..layer_count {
        if !rules.layer_structure.arr[index].is_signal {
            // The file already declares planes; the heuristic exists only for files that do not.
            return false;
        }
    }
    let mut layer_contains_wires = vec![false; layer_count];
    for pending in items.iter() {
        match pending {
            Pending::Trace { layer, .. } => {
                if let Some(slot) = layer_contains_wires.get_mut((*layer).max(0) as usize) {
                    *slot = true;
                }
            }
            Pending::Plain(item) => {
                if let ItemKind::Trace(trace) = &item.kind {
                    if let Some(slot) = layer_contains_wires.get_mut(trace.layer.max(0) as usize) {
                        *slot = true;
                    }
                }
            }
            Pending::Via { .. } => {}
        }
    }
    let board_area: f64 = outline_shapes.iter().map(board_shape_area).sum();
    let mut changed = false;
    let mut plane_nets: Vec<i32> = Vec::new();
    for pending in items.iter_mut() {
        let Pending::Plain(item) = pending else {
            continue;
        };
        let ItemKind::ConductionArea(area) = &item.kind else {
            continue;
        };
        let index = area.layer.max(0) as usize;
        if index >= layer_count || layer_contains_wires[index] {
            continue;
        }
        // Never the outer layers: a pour on the top or bottom layer is ordinary copper.
        if index == 0 || index == layer_count - 1 {
            continue;
        }
        if area_of(&area.area.relative_area) < 0.5 * board_area {
            continue;
        }
        plane_nets.extend(item.nets.iter().copied());
        if item.fixed_state < FixedState::UserFixed {
            item.fixed_state = FixedState::UserFixed;
        }
        changed = true;
    }
    for net_no in plane_nets {
        if let Some(net) = rules.nets.get_mut(net_no) {
            net.contains_plane = true;
        }
    }
    changed
}

fn area_of(shape: &AreaShape) -> f64 {
    match shape {
        AreaShape::Box(b) => b.area(),
        AreaShape::Octagon(o) => o.area(),
        AreaShape::Simplex(s) => s.area(),
        AreaShape::Convex(pieces) => pieces.iter().map(Simplex::area).sum(),
        AreaShape::Polygon(polygon) => polygon
            .corner_area()
            .map_or(0.0, |twice| (twice as f64).abs() / 2.0),
    }
}

fn board_shape_area(shape: &BoardShape) -> f64 {
    match shape {
        BoardShape::Box(b) => b.area(),
        BoardShape::Octagon(o) => o.area(),
        BoardShape::Simplex(s) => s.area(),
        BoardShape::Circle(c) => std::f64::consts::PI * f64::from(c.radius) * f64::from(c.radius),
        BoardShape::Polygon(corners) => {
            let points: Vec<Point> = corners.iter().map(|p| Point::Int(*p)).collect();
            fr_geometry::planar::Polygon::new(&points)
                .corner_area()
                .map_or(0.0, |twice| (twice as f64).abs() / 2.0)
        }
    }
}

/// §6.3 `Structure.separateHoles`.
///
/// The `isHole` flag is **assigned, not OR-ed**, inside the inner loop, so with three or more
/// nested rings only the last qualifying container decides. That is reproduced.
fn separate_holes(shapes: &mut Vec<BoardShape>) -> Vec<BoardShape> {
    let count = shapes.len();
    let boxes: Vec<IntBox> = shapes.iter().map(BoardShape::bounding_box).collect();
    let mut is_hole = vec![false; count];
    for i in 0..count {
        for j in 0..count {
            if i == j || is_hole[j] {
                continue;
            }
            // `if (!otherShape.boundingBox.contains(currentShape.boundingBox)) continue;`
            if !boxes[i].is_contained_in(&boxes[j]) {
                continue;
            }
            // Assigned, not OR-ed: with three or more nested rings only the last qualifying
            // container decides, so the result depends on iteration order. Reproduced.
            is_hole[i] = contains_all_corners(&shapes[j], &shapes[i]);
        }
    }
    let mut holes = Vec::new();
    let mut remaining = Vec::new();
    for (index, shape) in shapes.drain(..).enumerate() {
        if is_hole[index] {
            holes.push(shape);
        } else {
            remaining.push(shape);
        }
    }
    *shapes = remaining;
    holes
}

/// `OutlineShape.containsAllCorners`, approximated with the container's convex decomposition. A
/// container whose decomposition fails answers `false`, as upstream does.
fn contains_all_corners(container: &BoardShape, inner: &BoardShape) -> bool {
    let pieces = convex_pieces(container);
    if pieces.is_empty() {
        return false;
    }
    let corners = match inner {
        BoardShape::Polygon(corners) => corners.clone(),
        other => (0..4).map(|i| other.bounding_box().corner(i)).collect(),
    };
    corners.iter().all(|corner| {
        pieces
            .iter()
            .any(|piece| piece.contains(&Point::Int(*corner)))
    })
}

fn convex_pieces(shape: &BoardShape) -> Vec<Simplex> {
    match shape {
        BoardShape::Box(b) => vec![b.to_simplex()],
        BoardShape::Octagon(o) => vec![o.to_simplex()],
        BoardShape::Simplex(s) => vec![s.clone()],
        BoardShape::Circle(c) => vec![c.bounding_octagon().to_simplex()],
        // `split_to_convex` is upstream's own decomposition. When it fails — a corner list that is
        // not a simple polygon — the convex hull's tile stands in, which is conservative in the
        // containment test: a hole is detected slightly more eagerly than upstream.
        BoardShape::Polygon(corners) => split_to_convex(corners).unwrap_or_else(|| {
            let hull = convex_hull(corners);
            if hull.len() < 3 {
                Vec::new()
            } else {
                vec![bounding_tile(&hull)]
            }
        }),
    }
}

/// One placed area item: `ObstacleArea`'s `relativeArea` / `translation` / `rotationInDegree` /
/// `sideChanged` quartet, with the part of the rotation the board cannot realize already applied.
///
/// `PlacedArea::rotation_deg` is read by `search_tree::placed_area_pieces`, which turns a
/// right-angle rotation exactly and **widens any other one to the bounding octagon of the piece's
/// circumscribed circle about the component origin** — never a missed obstacle, which is what the
/// router needs, and ruinous for a DRC. Issue054 is the witness: `SW33` is placed at 330°, so each
/// of its 0.34 mm pad keepouts became a 16 mm blob centred on the component and overlapped every
/// trace in reach. 44 of the 45 records in that fixture's G1.2a gap were that one widening.
///
/// Upstream has no such restriction — `ObstacleArea.get_area` (`ObstacleArea.java:84-91`) rotates
/// the area with `rotate_approx`, which rounds each corner onto the grid and moves a circle's
/// centre — so the oblique rotation is applied here instead and the board is handed an area it
/// can place exactly. Right-angle rotations stay symbolic: the board realizes those exactly, and
/// leaving them alone keeps the common case free of rounding.
fn placed_area(
    shape: &BoardShape,
    translation: IntVector,
    rotation_deg: f64,
    side_changed: bool,
) -> PlacedArea {
    if rotation_deg % 90.0 == 0.0 {
        return PlacedArea {
            relative_area: board_shape_to_area_shape(shape),
            translation,
            rotation_deg,
            side_changed,
        };
    }
    PlacedArea {
        relative_area: board_shape_to_area_shape(&rotate_board_shape_approx(shape, rotation_deg)),
        translation,
        rotation_deg: 0.0,
        side_changed,
    }
}

/// `Shape.rotate_approx(angle, FloatPoint.ZERO)`: every corner rotated and rounded onto the grid,
/// except a circle, which upstream rotates by moving its centre and keeping its radius
/// (`Circle.java:193-196`).
///
/// A rotated tile is returned as a corner list rather than as a tile, because rounding can make a
/// convex corner very slightly concave, and the [`split_to_convex`] the search tree runs on the
/// stored polygon handles that where `TileShape.get_instance` would quietly cut the corner off.
/// That is also the order the two have to compose in: rotate, then split. Splitting first and
/// rotating the pieces would round each piece independently and let them drift apart.
fn rotate_board_shape_approx(shape: &BoardShape, rotation_deg: f64) -> BoardShape {
    let angle = rotation_deg.to_radians();
    let pole = FloatPoint::new(0.0, 0.0);
    let rotate = |point: FloatPoint| point.rotate(angle, &pole).jround();
    match shape {
        BoardShape::Circle(circle) => {
            BoardShape::Circle(Circle::new(rotate(circle.center.to_float()), circle.radius))
        }
        BoardShape::Box(b) => BoardShape::Polygon(
            (0..4)
                .map(|index| rotate(b.corner(index).to_float()))
                .collect(),
        ),
        BoardShape::Octagon(octagon) => BoardShape::Polygon(
            octagon
                .to_simplex()
                .corner_approx_arr()
                .into_iter()
                .map(rotate)
                .collect(),
        ),
        BoardShape::Simplex(simplex) => BoardShape::Polygon(
            simplex
                .corner_approx_arr()
                .into_iter()
                .map(rotate)
                .collect(),
        ),
        BoardShape::Polygon(corners) => {
            BoardShape::Polygon(corners.iter().map(|c| rotate(c.to_float())).collect())
        }
    }
}

fn board_shape_to_area_shape(shape: &BoardShape) -> AreaShape {
    match shape {
        BoardShape::Box(b) => AreaShape::Box(*b),
        BoardShape::Octagon(o) => AreaShape::Octagon(*o),
        BoardShape::Simplex(s) => AreaShape::Simplex(s.clone()),
        // No `Circle` variant in the board's area model; a circular keepout is indexed by its
        // bounding octagon, the same approximation `fr_board::Padstack::round` makes.
        BoardShape::Circle(c) => AreaShape::Octagon(c.bounding_octagon()),
        // The **original** corner list, as `ObstacleArea` stores it (`ObstacleArea.java:37-56`:
        // the constructor keeps `p_area` and nothing else). The convex decomposition upstream's
        // consumers want is `Area.split_to_convex()`, computed on demand — `fr-board`'s search
        // tree calls it through [`fr_geometry::planar::Polygon::split_to_convex`] — and is
        // deliberately *not* baked in here: `DsnWriter` writes the stored area's own corners, so
        // an importer that stored a decomposition instead would have no corners left to write.
        BoardShape::Polygon(corners) => AreaShape::Polygon(fr_geometry::planar::Polygon::new(
            &corners.iter().map(|p| Point::Int(*p)).collect::<Vec<_>>(),
        )),
    }
}

fn board_shape_to_polyline(shape: &BoardShape) -> Option<Polyline> {
    let corners: Vec<IntPoint> = match shape {
        BoardShape::Polygon(corners) => {
            let mut closed = corners.clone();
            if let Some(first) = corners.first() {
                closed.push(*first);
            }
            closed
        }
        BoardShape::Box(b) => vec![
            b.corner(0),
            b.corner(1),
            b.corner(2),
            b.corner(3),
            b.corner(0),
        ],
        BoardShape::Octagon(o) => {
            let mut corners: Vec<IntPoint> = (0..8).map(|i| o.corner(i)).collect();
            corners.push(corners[0]);
            corners
        }
        BoardShape::Simplex(s) => {
            let mut corners = Vec::new();
            for index in 0..s.border_line_count() {
                if let Point::Int(p) = s.corner(index) {
                    corners.push(p);
                }
            }
            if corners.len() < 3 {
                return None;
            }
            corners.push(corners[0]);
            corners
        }
        BoardShape::Circle(_) => return None,
    };
    if corners.len() < 3 {
        return None;
    }
    Some(Polyline::from_int_points(&corners))
}

fn simplify_pad(shape: PadGeometry) -> PadGeometry {
    match shape {
        PadGeometry::Simplex(simplex) => {
            if simplex.is_int_box() {
                PadGeometry::Box(simplex.bounding_box())
            } else if let Some(octagon) = simplex.to_int_octagon() {
                PadGeometry::Octagon(octagon)
            } else {
                PadGeometry::Simplex(simplex)
            }
        }
        other => other,
    }
}

/// `Vector.turn90Degree` when the angle is an exact multiple of 90, otherwise the rounded
/// approximate rotation. The `%` test is on the raw `f64`, so `90.0000001` takes the approximate
/// path — exactly as upstream.
fn rotate_vector(vector: IntVector, rotation_deg: f64) -> IntVector {
    if rotation_deg % 90.0 == 0.0 {
        let factor = (rotation_deg as i32) / 90;
        if factor == 0 {
            vector
        } else {
            vector.turn_90_degree_int(factor)
        }
    } else {
        let rotated = vector
            .to_float()
            .rotate(rotation_deg.to_radians(), &FloatPoint::new(0.0, 0.0))
            .jround();
        IntVector::new(rotated.x, rotated.y)
    }
}

fn fixed_state(state: DsnFixedState) -> FixedState {
    match state {
        DsnFixedState::Unfixed => FixedState::Unfixed,
        DsnFixedState::ShoveFixed => FixedState::ShoveFixed,
        DsnFixedState::UserFixed => FixedState::UserFixed,
        DsnFixedState::SystemFixed => FixedState::SystemFixed,
    }
}

fn float_point_in_box(point: &FloatPoint, box_: &IntBox) -> bool {
    point.x >= f64::from(box_.ll.x)
        && point.y >= f64::from(box_.ll.y)
        && point.x <= f64::from(box_.ur.x)
        && point.y <= f64::from(box_.ur.y)
}

/// `KiCadNetClassNames.isKiCadDefaultNetClassName`.
fn is_kicad_default_net_class_name(name: &str) -> bool {
    !name.is_empty()
        && (name.eq_ignore_ascii_case("default") || name.eq_ignore_ascii_case("kicad_default"))
}

fn strip_package_suffix(name: &str) -> String {
    // Java `replaceAll("::\\d+$", "")` — only a trailing `::<digits>`.
    if let Some(position) = name.rfind("::") {
        let tail = &name[position + 2..];
        if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) {
            return name[..position].to_string();
        }
    }
    name.to_string()
}

fn package_pins_identical(existing: &[ImportedPin], candidate: &[ImportedPin]) -> bool {
    if existing.len() != candidate.len() {
        return false;
    }
    existing.iter().zip(candidate).all(|(a, b)| {
        a.name == b.name
            && a.padstack_index == b.padstack_index
            && f64::from(a.relative_location.x - b.relative_location.x).abs() <= 0.001
            && f64::from(a.relative_location.y - b.relative_location.y).abs() <= 0.001
            && (a.rotation - b.rotation).abs() <= 0.001
    })
}

/// Compile-time reminder that [`Nets`] is re-exported for callers that inspect the result.
#[allow(dead_code)]
fn _nets_type_is_used(_: &Nets) {}
