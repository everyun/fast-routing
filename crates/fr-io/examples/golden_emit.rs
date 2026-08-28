//! G1.1 gate dry-run instrument: emit `fast-routing.golden-dump/1` from the new Specctra importer.
//!
//! This is deliberately a small, dependency-free reference emitter so the golden comparator
//! (`bench/tools/golden_compare.py`) can be pointed at real importer output *today* and produce an
//! honest field-level coverage number. It is NOT the production `fr-cli` emitter that
//! `bench/tools/README.md` asks for; several groups are intentionally left empty and are reported
//! as such:
//!
//! * `boundary`, `structure_areas` — area objects are not reconstructed here.
//!
//! Usage: `cargo run -p fr-io --example golden_emit -- <in.dsn> <out.json>`
//!
//! Every length is converted back to DSN units (`board / scale_factor`), which is the unit the
//! schema and the Java `DsnWriter` dumps both use.

use std::collections::BTreeMap;

use fr_board::item::ItemKind;
use fr_board::FixedState;
use fr_geometry::planar::{IntPoint, Point};
use fr_io::specctra::{self, ImportedDesign, PadGeometry};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: golden_emit <in.dsn> <out.json>");
        std::process::exit(2);
    }
    let bytes = std::fs::read(&args[1]).expect("read dsn");
    let design = match specctra::import(&bytes) {
        Ok(design) => design,
        Err(error) => {
            eprintln!("import failed: {error}");
            std::process::exit(1);
        }
    };
    let source = std::path::Path::new(&args[1])
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if std::env::var_os("FR_EMIT_PINS").is_some() {
        // Resolved pin centres, in BOARD units — the §9.2.1 output the schema does not carry.
        for component in &design.components {
            let package = &design.packages[component.package_index];
            for (index, pin) in package.pins.iter().enumerate() {
                let center = component.pin_centers[index];
                eprintln!(
                    "PIN {} {} {} {}",
                    component.name, pin.name, center.x, center.y
                );
            }
        }
    }
    if std::env::var_os("FR_EMIT_WARNINGS").is_some() {
        for warning in &design.warnings {
            eprintln!("WARN {warning}");
        }
    }
    let json = emit(&design, &source);
    std::fs::write(&args[2], json).expect("write json");
}

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
}

fn emit(design: &ImportedDesign, source: &str) -> String {
    let e = Emitter {
        design,
        scale: design.scale_factor,
    };
    let layer_count = design.rules.layer_structure.len();
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!("  \"schema\": {},\n", esc("fast-routing.golden-dump/1")));
    out.push_str(&format!("  \"producer\": {},\n", esc("fr-io golden_emit (dry-run)")));
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
    out.push_str(&format!("    \"smd_to_turn_gap\": {{{}}},\n", gaps.join(", ")));
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
            let (x, y) = (
                e.d(pin.relative_location.x),
                e.d(pin.relative_location.y),
            );
            pins.push(format!(
                "{{\"padstack\": {}, \"name\": {}, \"x\": {}, \"y\": {}, \"rotation\": {}}}",
                esc(&design.padstacks[pin.padstack_index].name),
                esc(&pin.name),
                num(x),
                num(y),
                num(pin.rotation)
            ));
        }
        images.push(format!(
            "{{\"name\": {}, \"side\": {}, \"pins\": [{}], \"keepouts\": {}, \"via_keepouts\": {}, \"place_keepouts\": {}, \"outlines\": []}}",
            esc(&package.name),
            esc(if package.is_front { "front" } else { "back" }),
            pins.join(", "),
            package.keepouts.len(),
            package.via_keepouts.len(),
            package.place_keepouts.len()
        ));
    }
    out.push_str(&format!("  \"images\": [{}],\n", images.join(", ")));

    // ---- components --------------------------------------------------------------------------
    let mut comps = Vec::new();
    for component in &design.components {
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
        let (x, y) = match component.location {
            Some(p) => e.dp(p),
            None => (0.0, 0.0),
        };
        comps.push(format!(
            "{{\"name\": {}, \"image\": {}, \"placed\": {}, \"x\": {}, \"y\": {}, \"side\": {}, \"rotation\": {}, \"position_fixed\": {}, \"pins\": [{}], \"keepouts\": []}}",
            esc(&component.name),
            esc(&package.name),
            component.location.is_some(),
            num(x),
            num(y),
            esc(if component.is_front { "front" } else { "back" }),
            num(component.rotation),
            component.position_fixed,
            pins.join(", ")
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
            let vias: Vec<String> = rule
                .vias()
                .iter()
                .map(|info| esc(info.name()))
                .collect();
            format!(
                "{{\"name\": {}, \"vias\": [{}]}}",
                esc(&rule.name),
                vias.join(", ")
            )
        })
        .collect();
    out.push_str(&format!("  \"via_rules\": [{}],\n", via_rules.join(", ")));

    // ---- wiring ------------------------------------------------------------------------------
    let mut counts: BTreeMap<(&'static str, &'static str), usize> = BTreeMap::new();
    for (_, item) in design.board.store().iter() {
        let kind = match &item.kind {
            ItemKind::Trace(_) => "trace",
            ItemKind::Via(_) => "via",
            ItemKind::ConductionArea(_) => "conduction_area",
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

    // ---- NOT EMITTED -------------------------------------------------------------------------
    out.push_str("  \"boundary\": [],\n");
    out.push_str("  \"structure_areas\": []\n");
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
