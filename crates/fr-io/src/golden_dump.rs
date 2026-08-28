//! `fast-routing.golden-dump/1` — the G1.1 exchange document, emitted from an [`ImportedDesign`].
//!
//! The schema and the comparator that consumes it are `bench/tools/README.md` and
//! `bench/tools/golden_compare.py`. The comparator's *left*-hand document is the release jar's
//! own `-do` dump re-parsed, so this emitter is written against the **writer** side of upstream —
//! `io/specctra/parser/{Structure,Wiring,Plane,Package,Component}.write_scope` — not against the
//! board model in the abstract. Where the writer cannot express a field (a conduction area's
//! fixed state, a fractional placement rotation, an empty identifier) the loss is recorded in the
//! comparator's `EXCLUSIONS` and folded away on both sides rather than papered over here.
//!
//! Every length is converted back to DSN units (`board / scale_factor`), which is the unit both
//! the schema and the Java dumps use. The JSON is written by hand — the crate has no serde
//! dependency and the document is small and fully determined by the model.
//!
//! ```no_run
//! let bytes = std::fs::read("board.dsn").unwrap();
//! let design = fr_io::specctra::import(&bytes).unwrap();
//! let json = fr_io::golden_dump::emit(&design, "board.dsn");
//! assert!(json.starts_with("{"));
//! ```

use std::collections::{BTreeMap, BTreeSet};

use fr_board::item::{AreaShape, ItemKind, ObstacleKind};
use fr_board::FixedState;
use fr_geometry::planar::{IntBox, IntPoint, Point};

use crate::specctra::ast::BoardShape;
use crate::specctra::jround::JavaRound;
use crate::specctra::{ImportedDesign, PadGeometry};

// ---------------------------------------------------------------------------------------------
// Tiny JSON writer
// ---------------------------------------------------------------------------------------------

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn num(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

// ---------------------------------------------------------------------------------------------

struct Emitter<'a> {
    design: &'a ImportedDesign,
    scale: f64,
}

impl<'a> Emitter<'a> {
    fn d(&self, board_value: i32) -> f64 {
        if self.scale == 0.0 {
            0.0
        } else {
            f64::from(board_value) / self.scale
        }
    }

    fn dp(&self, p: IntPoint) -> (f64, f64) {
        (self.d(p.x), self.d(p.y))
    }

    fn layer_name(&self, index: usize) -> String {
        self.design
            .rules
            .layer_structure
            .arr
            .get(index)
            .map(|l| l.name.clone())
            .unwrap_or_else(|| format!("layer{index}"))
    }

    fn class_name(&self, index: usize) -> String {
        self.design
            .rules
            .clearance_matrix
            .get_name(index)
            .unwrap_or("?")
            .to_string()
    }

    fn net_name(&self, net_no: i32) -> Option<String> {
        self.design
            .rules
            .nets
            .iter()
            .find(|net| net.net_number == net_no)
            .map(|net| net.name.clone())
    }

    /// `CoordinateTransform.board_to_dsn(Shape, Layer)` (`io/CoordinateTransform.java:159-177`):
    /// an `IntBox` becomes a `(rect …)`, a `Circle` becomes a `(circle …)`, and **every**
    /// `PolylineShape` — simplex, octagon, polygon alike — becomes a `(polygon …)` over its
    /// `cornerApproxArr()`, with the leading `0` aperture `Polygon.write_scope` always emits.
    fn board_shape_coords(&self, shape: &BoardShape) -> Option<(&'static str, Vec<f64>)> {
        Some(match shape {
            BoardShape::Box(b) => ("rect", self.box_coords(b)),
            BoardShape::Circle(c) => {
                let (x, y) = self.dp(c.center);
                ("circle", vec![self.d(c.radius.saturating_mul(2)), x, y])
            }
            BoardShape::Octagon(o) => ("polygon", self.polygon_coords((0..8).map(|i| o.corner(i)))),
            BoardShape::Simplex(s) => (
                "polygon",
                self.polygon_coords((0..s.border_line_count()).map(|i| match s.corner(i) {
                    Point::Int(p) => p,
                    Point::Rational(r) => r.to_float().jround(),
                })),
            ),
            BoardShape::Polygon(corners) => {
                ("polygon", self.polygon_coords(corners.iter().copied()))
            }
        })
    }

    /// The same, for the board item model's [`AreaShape`].
    ///
    /// [`AreaShape::Convex`] is upstream's `PolylineArea`, whose writer takes `get_border()`; the
    /// importer never builds one, so it is reported as unwritable rather than guessed at.
    fn area_shape_coords(&self, shape: &AreaShape) -> Option<(&'static str, Vec<f64>)> {
        Some(match shape {
            AreaShape::Box(b) => ("rect", self.box_coords(b)),
            AreaShape::Octagon(o) => ("polygon", self.polygon_coords((0..8).map(|i| o.corner(i)))),
            AreaShape::Simplex(s) => (
                "polygon",
                self.polygon_coords((0..s.border_line_count()).map(|i| match s.corner(i) {
                    Point::Int(p) => p,
                    Point::Rational(r) => r.to_float().jround(),
                })),
            ),
            AreaShape::Polygon(polygon) => (
                "polygon",
                self.polygon_coords(polygon.corner_array().iter().map(|p| match p {
                    Point::Int(p) => *p,
                    Point::Rational(r) => r.to_float().jround(),
                })),
            ),
            AreaShape::Convex(_) => return None,
        })
    }

    fn box_coords(&self, b: &IntBox) -> Vec<f64> {
        let (x1, y1) = self.dp(b.ll);
        let (x2, y2) = self.dp(b.ur);
        vec![x1, y1, x2, y2]
    }

    fn polygon_coords(&self, corners: impl Iterator<Item = IntPoint>) -> Vec<f64> {
        // `Polygon.write_scope` writes the aperture width first and it is always 0 here.
        let mut coords = vec![0.0];
        for corner in corners {
            let (x, y) = self.dp(corner);
            coords.push(x);
            coords.push(y);
        }
        coords
    }
}

/// One reduced area object, in the shape `golden_compare.reduce_area` produces from a dump.
///
/// The reduction — `(kind, shape, layer, corner count, bounding box, window count, clearance
/// class[, net])` — is the comparator's "area geometry reduction" exclusion; `net` exists only on
/// planes, and its **absence** on every other kind is part of the multiset key, so it must not be
/// emitted as `null`.
struct Area<'a> {
    kind: &'a str,
    shape: Option<(&'static str, Vec<f64>)>,
    layer: &'a str,
    windows: usize,
    clearance_class: Option<String>,
    net: Option<String>,
}

impl Area<'_> {
    fn to_json(&self) -> String {
        let (shape, layer, corners, bbox) = match &self.shape {
            None => (
                "null".to_string(),
                "null".to_string(),
                0usize,
                "null".to_string(),
            ),
            Some((kind, coords)) => {
                let (corners, bbox) = corners_and_bbox(kind, coords);
                (
                    esc(kind),
                    esc(self.layer),
                    corners,
                    match bbox {
                        None => "null".to_string(),
                        Some(b) => format!(
                            "[{}, {}, {}, {}]",
                            num(b[0]),
                            num(b[1]),
                            num(b[2]),
                            num(b[3])
                        ),
                    },
                )
            }
        };
        let mut out = format!(
            "{{\"kind\": {}, \"shape\": {}, \"layer\": {}, \"corners\": {}, \"bbox\": {}, \"windows\": {}, \"clearance_class\": {}",
            esc(self.kind),
            shape,
            layer,
            corners,
            bbox,
            self.windows,
            match &self.clearance_class {
                Some(name) => esc(name),
                None => "null".to_string(),
            }
        );
        if let Some(net) = &self.net {
            out.push_str(&format!(", \"net\": {}", esc(net)));
        }
        out.push('}');
        out
    }
}

/// `golden_compare._corner_count` / `_bbox`, reproduced so the two documents agree by
/// construction rather than by luck.
fn corners_and_bbox(kind: &str, coords: &[f64]) -> (usize, Option<[f64; 4]>) {
    match kind {
        "circle" => {
            let radius = coords.first().copied().unwrap_or(0.0) / 2.0;
            let cx = coords.get(1).copied().unwrap_or(0.0);
            let cy = coords.get(2).copied().unwrap_or(0.0);
            (
                0,
                Some([cx - radius, cy - radius, cx + radius, cy + radius]),
            )
        }
        "rect" if coords.len() >= 4 => (
            4,
            Some([
                coords[0].min(coords[2]),
                coords[1].min(coords[3]),
                coords[0].max(coords[2]),
                coords[1].max(coords[3]),
            ]),
        ),
        _ => {
            // Polygons and paths carry the aperture / width first; it is not a coordinate.
            let points = &coords[coords.len().min(1)..];
            let corners = points.len() / 2;
            if points.len() < 2 {
                return (corners, None);
            }
            let xs: Vec<f64> = points.iter().step_by(2).copied().collect();
            let ys: Vec<f64> = points.iter().skip(1).step_by(2).copied().collect();
            let bbox = [
                xs.iter().copied().fold(f64::INFINITY, f64::min),
                ys.iter().copied().fold(f64::INFINITY, f64::min),
                xs.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                ys.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            ];
            (corners, Some(bbox))
        }
    }
}

/// Emits the whole document for `design`.
///
/// `source` is recorded as metadata only; the comparator never looks at it.
pub fn emit(design: &ImportedDesign, source: &str) -> String {
    let e = Emitter {
        design,
        scale: design.scale_factor,
    };
    let layer_count = design.rules.layer_structure.len();
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!(
        "  \"schema\": {},\n",
        esc("fast-routing.golden-dump/1")
    ));
    out.push_str(&format!(
        "  \"producer\": {},\n",
        esc("fr-io golden_emit (dry-run)")
    ));
    out.push_str(&format!("  \"source\": {},\n", esc(source)));
    out.push_str(&format!("  \"design\": {},\n", esc(&design.design_name)));

    let unit = format!("{:?}", design.unit).to_lowercase();
    out.push_str(&format!("  \"unit\": {},\n", esc(&unit)));
    out.push_str(&format!(
        "  \"resolution\": {{\"unit\": {}, \"value\": {}}},\n",
        esc(&unit),
        design.resolution
    ));

    // ---- layers ------------------------------------------------------------------------------
    let mut items = Vec::new();
    for index in 0..layer_count {
        let layer = &design.rules.layer_structure.arr[index];
        let kind = if layer.is_signal { "signal" } else { "power" };
        items.push(format!(
            "{{\"name\": {}, \"type\": {}}}",
            esc(&layer.name),
            esc(kind)
        ));
    }
    out.push_str(&format!("  \"layers\": [{}],\n", items.join(", ")));

    // ---- rules -------------------------------------------------------------------------------
    let class_count = design.rules.clearance_matrix.class_count();
    let mut cells = Vec::new();
    // Class 0 is the reserved `null` class upstream never writes.
    for i in 1..class_count {
        for j in i..class_count {
            for layer in 0..layer_count {
                let value = design.rules.clearance_matrix.get_value(i, j, layer, false);
                cells.push(format!(
                    "{{\"classes\": [{}, {}], \"layer\": {}, \"value\": {}}}",
                    esc(&e.class_name(i)),
                    esc(&e.class_name(j)),
                    esc(&e.layer_name(layer)),
                    num(e.d(value))
                ));
            }
        }
    }
    let default_class = design.rules.get_default_net_class();
    let mut widths = Vec::new();
    let mut gaps = Vec::new();
    for layer in 0..layer_count {
        widths.push(format!(
            "{}: {}",
            esc(&e.layer_name(layer)),
            num(e.d(default_class.get_trace_half_width(layer).saturating_mul(2)))
        ));
        gaps.push(format!(
            "{}: {}",
            esc(&e.layer_name(layer)),
            num(design.rules.get_pin_edge_to_turn_dist() / e.scale.max(1.0))
        ));
    }
    let snap = match design.rules.get_trace_angle_restriction() {
        fr_rules::AngleRestriction::None => "none",
        fr_rules::AngleRestriction::FortyFiveDegree => "fortyfive_degree",
        fr_rules::AngleRestriction::NinetyDegree => "ninety_degree",
    };
    out.push_str("  \"rules\": {\n");
    out.push_str(&format!(
        "    \"clearance_matrix\": [{}],\n",
        cells.join(", ")
    ));
    out.push_str(&format!(
        "    \"default_trace_width\": {{{}}},\n",
        widths.join(", ")
    ));
    out.push_str(&format!(
        "    \"smd_to_turn_gap\": {{{}}},\n",
        gaps.join(", ")
    ));
    out.push_str(&format!("    \"snap_angle\": {},\n", esc(snap)));
    // `Structure.writeControlScope` derives this from the via infos, NOT from the DSN
    // `(control (via_at_smd …))` flag, so the schema means "some via info allows SMD attach".
    let any_attach = design
        .rules
        .via_infos
        .iter()
        .any(|info| info.attach_smd_allowed());
    out.push_str(&format!(
        "    \"via_at_smd\": {}\n",
        esc(if any_attach { "on" } else { "off" })
    ));
    out.push_str("  },\n");

    // ---- padstacks ---------------------------------------------------------------------------
    let mut stacks = Vec::new();
    for stack in &design.padstacks {
        let mut shapes = Vec::new();
        for (layer, shape) in stack.shapes.iter().enumerate() {
            let Some(shape) = shape else { continue };
            let (kind, coords) = pad_coords(&e, shape);
            shapes.push(format!(
                "{{\"kind\": {}, \"layer\": {}, \"coords\": [{}]}}",
                esc(kind),
                esc(&e.layer_name(layer)),
                coords
                    .iter()
                    .map(|v| num(*v))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        stacks.push(format!(
            "{{\"name\": {}, \"attach\": {}, \"absolute\": {}, \"shapes\": [{}]}}",
            esc(&stack.name),
            stack.attach_allowed,
            stack.placed_absolute,
            shapes.join(", ")
        ));
    }
    out.push_str(&format!("  \"padstacks\": [{}],\n", stacks.join(", ")));

    // ---- images ------------------------------------------------------------------------------
    let mut images = Vec::new();
    for package in &design.packages {
        let mut pins = Vec::new();
        for pin in &package.pins {
            let (x, y) = (e.d(pin.relative_location.x), e.d(pin.relative_location.y));
            pins.push(format!(
                "{{\"padstack\": {}, \"name\": {}, \"x\": {}, \"y\": {}, \"rotation\": {}}}",
                esc(&design.padstacks[pin.padstack_index].name),
                esc(&pin.name),
                num(x),
                num(y),
                // `Package.write_scope` writes `(rotate (int) Math.round(rotation_in_degree))`
                // and omits it entirely when the rounded value is 0.
                pin.rotation.jround()
            ));
        }
        // `Package.write_scope` writes every package outline as `(outline (polygon signal …))`
        // through `board_to_dsn_rel`, i.e. relative to the component origin and always on the
        // pseudo-layer `signal`.
        let outlines: Vec<String> = package
            .outlines
            .iter()
            .flatten()
            .map(|shape| {
                Area {
                    kind: "outline",
                    shape: e.board_shape_coords(shape),
                    layer: "signal",
                    windows: 0,
                    clearance_class: None,
                    net: None,
                }
                .to_json()
            })
            .collect();
        images.push(format!(
            "{{\"name\": {}, \"side\": {}, \"pins\": [{}], \"keepouts\": {}, \"via_keepouts\": {}, \"place_keepouts\": {}, \"outlines\": [{}]}}",
            esc(&package.name),
            esc(if package.is_front { "front" } else { "back" }),
            pins.join(", "),
            package.keepouts.len(),
            package.via_keepouts.len(),
            package.place_keepouts.len(),
            outlines.join(", ")
        ));
    }
    out.push_str(&format!("  \"images\": [{}],\n", images.join(", ")));

    // ---- components --------------------------------------------------------------------------
    // `Package.write_placement_scope` (`Package.java:321-350`) writes a component only when it is
    // unplaced *or* still owns at least one board item — "check, if not all items of the component
    // are deleted". A placed component whose image is empty (`(image LOGO )`, a silkscreen-only
    // footprint with no pins, keepouts or outlines) contributes nothing to `item_list` and is
    // therefore absent from the dump even though its `(image …)` is written. Reproduce that here,
    // or every such component shows up as a phantom difference.
    let mut components_with_items: BTreeSet<i32> = BTreeSet::new();
    for (_, item) in design.board.store().iter() {
        if item.component_no > 0 {
            components_with_items.insert(item.component_no);
        }
    }
    let mut comps = Vec::new();
    for (index, component) in design.components.iter().enumerate() {
        let component_no = index as i32 + 1;
        if component.location.is_some() && !components_with_items.contains(&component_no) {
            continue;
        }
        let package = &design.packages[component.package_index];
        let mut pins = Vec::new();
        for (index, pin) in package.pins.iter().enumerate() {
            let class = component
                .pin_clearance_classes
                .get(index)
                .copied()
                .unwrap_or(-1);
            let name = if class >= 0 {
                e.class_name(class as usize)
            } else {
                "default".to_string()
            };
            pins.push(format!(
                "{{\"name\": {}, \"clearance_class\": {}}}",
                esc(&pin.name),
                esc(&name)
            ));
        }
        // `Component.write_keepout_infos` (`Component.java:110-146`): nothing for an unplaced
        // component, and one record per package keepout whose board area resolved to a real
        // clearance class — class 0 is the reserved null class and is skipped there.
        let mut keepouts = Vec::new();
        for (kind_index, (kind, areas)) in [
            ("keepout", &package.keepouts),
            ("via_keepout", &package.via_keepouts),
            ("place_keepout", &package.place_keepouts),
        ]
        .into_iter()
        .enumerate()
        {
            let classes = component.keepout_clearance_classes.of_kind(kind_index);
            for (index, area) in areas.iter().enumerate() {
                let Some(name) = &area.name else { continue };
                let class = classes.get(index).copied().unwrap_or(0);
                if class <= 0 {
                    continue;
                }
                keepouts.push(format!(
                    "{{\"kind\": {}, \"name\": {}, \"clearance_class\": {}}}",
                    esc(kind),
                    esc(name),
                    esc(&e.class_name(class as usize))
                ));
            }
        }
        let (x, y) = match component.location {
            Some(p) => e.dp(p),
            None => (0.0, 0.0),
        };
        comps.push(format!(
            "{{\"name\": {}, \"image\": {}, \"placed\": {}, \"x\": {}, \"y\": {}, \"side\": {}, \"rotation\": {}, \"position_fixed\": {}, \"pins\": [{}], \"keepouts\": [{}]}}",
            esc(&component.name),
            esc(&package.name),
            component.location.is_some(),
            num(x),
            num(y),
            esc(if component.is_front { "front" } else { "back" }),
            num(component.rotation),
            component.position_fixed,
            pins.join(", "),
            keepouts.join(", ")
        ));
    }
    out.push_str(&format!("  \"components\": [{}],\n", comps.join(", ")));

    // ---- nets --------------------------------------------------------------------------------
    // Reconstructed the way Java's `Net.write_scope` does: every board pin carrying the net.
    let mut by_net: BTreeMap<i32, Vec<(String, String)>> = BTreeMap::new();
    for (_, item) in design.board.store().iter() {
        let ItemKind::Pin(pin) = &item.kind else {
            continue;
        };
        let Some(component) = design
            .components
            .get((item.component_no - 1).max(0) as usize)
        else {
            continue;
        };
        let package = &design.packages[component.package_index];
        let Some(pin_def) = package.pins.get(pin.pin_no as usize) else {
            continue;
        };
        for net_no in item.nets.iter() {
            by_net
                .entry(*net_no)
                .or_default()
                .push((component.name.clone(), pin_def.name.clone()));
        }
    }
    let mut nets = Vec::new();
    for net in design.rules.nets.iter() {
        let pins = by_net.get(&net.net_number).cloned().unwrap_or_default();
        nets.push(format!(
            "{{\"name\": {}, \"subnet\": {}, \"pins\": [{}]}}",
            esc(&net.name),
            net.subnet_number,
            pins.iter()
                .map(|(c, p)| format!("[{}, {}]", esc(c), esc(p)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    out.push_str(&format!("  \"nets\": [{}],\n", nets.join(", ")));

    // ---- net classes -------------------------------------------------------------------------
    let mut classes = Vec::new();
    for (index, class) in design.rules.net_classes.iter().enumerate() {
        let members: Vec<String> = design
            .rules
            .nets
            .iter()
            .filter(|n| n.net_class_index == index)
            .map(|n| esc(&n.name))
            .collect();
        let layers: Vec<String> = (0..layer_count)
            .filter(|l| class.is_active_routing_layer(*l))
            .map(|l| esc(&e.layer_name(l)))
            .collect();
        classes.push(format!(
            "{{\"name\": {}, \"nets\": [{}], \"clearance_class\": {}, \"via_rule\": {}, \"trace_width\": {}, \"use_layer\": [{}], \"length_max\": null, \"length_min\": null, \"pull_tight\": true, \"shove_fixed\": false}}",
            esc(&class.name),
            members.join(", "),
            esc(&e.class_name(class.get_trace_clearance_class())),
            class
                .get_via_rule()
                .map(|r| esc(&r.name))
                .unwrap_or_else(|| "null".to_string()),
            num(e.d(class.get_trace_half_width(0) * 2)),
            layers.join(", ")
        ));
    }
    out.push_str(&format!("  \"net_classes\": [{}],\n", classes.join(", ")));

    // ---- vias --------------------------------------------------------------------------------
    let via_padstacks: Vec<String> = design
        .via_padstack_indices
        .iter()
        .map(|i| esc(&design.padstacks[*i].name))
        .collect();
    out.push_str(&format!(
        "  \"via_padstacks\": [{}],\n",
        via_padstacks.join(", ")
    ));
    let via_infos: Vec<String> = design
        .rules
        .via_infos
        .iter()
        .map(|info| {
            format!(
                "{{\"name\": {}, \"padstack\": {}, \"clearance_class\": {}, \"attach\": {}}}",
                esc(info.name()),
                esc(&info.padstack().name),
                esc(&e.class_name(info.clearance_class())),
                info.attach_smd_allowed()
            )
        })
        .collect();
    out.push_str(&format!("  \"via_infos\": [{}],\n", via_infos.join(", ")));
    let via_rules: Vec<String> = design
        .rules
        .via_rules
        .iter()
        .map(|rule| {
            let vias: Vec<String> = rule.vias().iter().map(|info| esc(info.name())).collect();
            format!(
                "{{\"name\": {}, \"vias\": [{}]}}",
                esc(&rule.name),
                vias.join(", ")
            )
        })
        .collect();
    out.push_str(&format!("  \"via_rules\": [{}],\n", via_rules.join(", ")));

    // ---- wiring ------------------------------------------------------------------------------
    // `Wiring.write_scope` (`Wiring.java:61-77`) writes a conduction area only when its layer is a
    // **signal** layer; the power-layer ones go to the structure scope as `(plane …)` instead
    // (`Structure.write_conduction_areas`, `Structure.java:91-108`). Splitting them the same way
    // is what keeps a board with power planes from showing a phantom `conduction_area` in `wiring`
    // and a missing `plane` in `structure_areas`.
    let is_signal_layer = |layer: i32| -> bool {
        usize::try_from(layer)
            .ok()
            .and_then(|index| design.rules.layer_structure.arr.get(index))
            .is_some_and(|layer| layer.is_signal)
    };
    let mut counts: BTreeMap<(&'static str, &'static str), usize> = BTreeMap::new();
    for (_, item) in design.board.store().iter() {
        let kind = match &item.kind {
            ItemKind::Trace(_) => "trace",
            ItemKind::Via(_) => "via",
            ItemKind::ConductionArea(area) if is_signal_layer(area.layer) => "conduction_area",
            _ => continue,
        };
        let fixed = match item.fixed_state {
            FixedState::Unfixed => "unfixed",
            FixedState::ShoveFixed => "shove_fixed",
            FixedState::UserFixed => "user_fixed",
            FixedState::SystemFixed => "system_fixed",
        };
        *counts.entry((kind, fixed)).or_default() += 1;
    }
    let wiring: Vec<String> = counts
        .iter()
        .map(|((kind, fixed), count)| {
            format!(
                "{{\"kind\": {}, \"fixed\": {}, \"count\": {}}}",
                esc(kind),
                esc(fixed),
                count
            )
        })
        .collect();
    out.push_str(&format!(
        "  \"wiring\": {{\"by_kind_fixed\": [{}]}},\n",
        wiring.join(", ")
    ));

    // ---- boundary ----------------------------------------------------------------------------
    // `Structure.write_boundaries` (`Structure.java:133-168`) writes the board bounding box first,
    // as a `(rect pcb …)`, and then one `(boundary …)` per outline contour on the pseudo-layer
    // `signal`. Both are `(boundary …)` scopes; only the layer tells them apart.
    let mut boundary = vec![Area {
        kind: "boundary",
        shape: Some(("rect", e.box_coords(&design.bounds))),
        layer: "pcb",
        windows: 0,
        clearance_class: None,
        net: None,
    }
    .to_json()];
    for shape in &design.outline_shapes {
        boundary.push(
            Area {
                kind: "boundary",
                shape: e.board_shape_coords(shape),
                layer: "signal",
                windows: 0,
                clearance_class: None,
                net: None,
            }
            .to_json(),
        );
    }
    out.push_str(&format!("  \"boundary\": [{}],\n", boundary.join(", ")));

    // ---- structure areas ---------------------------------------------------------------------
    let mut areas = Vec::new();
    for (_, item) in design.board.store().iter() {
        match &item.kind {
            ItemKind::ObstacleArea(obstacle) => {
                // `Structure.write_keepouts` skips component-owned areas — those are written as
                // `(keepout <name> (clearance_class …))` inside the placement scope instead.
                if item.component_no != 0 {
                    continue;
                }
                // `write_keepout_scope` names the scope from the Java *class*, and only
                // `ViaObstacleArea` has its own spelling: a structure `(place_keepout …)` becomes
                // a `ComponentObstacleArea`, which falls into the `else` and is written back as a
                // plain `(keepout …)`. The kind is therefore not round-trippable, and this
                // reproduces the collapse rather than hiding it in the comparator.
                let kind = match obstacle.kind {
                    ObstacleKind::ViaKeepout => "via_keepout",
                    ObstacleKind::Keepout | ObstacleKind::ComponentKeepout => "keepout",
                };
                // The class is written only when it is neither the reserved null class nor
                // `default`.
                let class = if item.clearance_class > 0 {
                    let name = e.class_name(item.clearance_class as usize);
                    if name == "default" {
                        None
                    } else {
                        Some(name)
                    }
                } else {
                    None
                };
                areas.push(
                    Area {
                        kind,
                        shape: e.area_shape_coords(&obstacle.area.relative_area),
                        layer: &e.layer_name(obstacle.layer.max(0) as usize),
                        windows: 0,
                        clearance_class: class,
                        net: None,
                    }
                    .to_json(),
                );
            }
            ItemKind::ConductionArea(area) if !is_signal_layer(area.layer) => {
                // `Plane.write_scope` writes the net name and no clearance class.
                let Some(net) = item.nets.iter().next().and_then(|no| e.net_name(*no)) else {
                    continue;
                };
                areas.push(
                    Area {
                        kind: "plane",
                        shape: e.area_shape_coords(&area.area.relative_area),
                        layer: &e.layer_name(area.layer.max(0) as usize),
                        windows: 0,
                        clearance_class: None,
                        net: Some(net),
                    }
                    .to_json(),
                );
            }
            _ => {}
        }
    }
    out.push_str(&format!("  \"structure_areas\": [{}]\n", areas.join(", ")));
    out.push_str("}\n");
    out
}

fn pad_coords(e: &Emitter, shape: &PadGeometry) -> (&'static str, Vec<f64>) {
    match shape {
        PadGeometry::Circle(c) => {
            let (x, y) = e.dp(c.center);
            ("circle", vec![e.d(c.radius.saturating_mul(2)), x, y])
        }
        PadGeometry::Box(b) => {
            let (x1, y1) = e.dp(b.ll);
            let (x2, y2) = e.dp(b.ur);
            ("rect", vec![x1, y1, x2, y2])
        }
        PadGeometry::Octagon(o) => {
            let mut coords = vec![0.0];
            for index in 0..8 {
                let (x, y) = e.dp(o.corner(index));
                coords.push(x);
                coords.push(y);
            }
            ("polygon", coords)
        }
        PadGeometry::Simplex(s) => {
            let mut coords = vec![0.0];
            for index in 0..s.border_line_count() {
                if let Point::Int(p) = s.corner(index) {
                    let (x, y) = e.dp(p);
                    coords.push(x);
                    coords.push(y);
                }
            }
            ("polygon", coords)
        }
    }
}
