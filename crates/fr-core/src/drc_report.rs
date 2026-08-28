//! Gate G1.2a's emitter: a board in, `java -drc` shaped JSON out.
//!
//! `bench/tools/drc_compare.py` diffs our violation multiset against the release jar's `-drc`
//! report. It used to get our side from `cargo run -p fr-check --example drc_report`; this
//! module is that emitter promoted into the product, so the gate can call `fr-cli --drc-report`
//! and stop depending on an example binary. **`crates/fr-check/examples/drc_report.rs` is
//! superseded by this file** and should be deleted by whoever owns `fr-check`; until then the
//! two must be changed together, and their output is byte-identical apart from `"generator"`.
//!
//! It lives in `fr-core` rather than in `fr-check` because it needs both the checker and the
//! importer, and `fr-check`'s own rule is that the referee never depends on a file format —
//! `fr-io` is a dev-dependency there precisely so that the example could reach it.
//!
//! What it does is what `Freerouting.java:332-352` does with `-drc` and no SES file: import the
//! board, run the whole-board clearance DRC, print `DesignRulesChecker.generateReport`'s
//! `violations` array. Nothing is routed, so the numbers are a property of the *input board* and
//! of the checker alone — which is the point of the gate.
//!
//! # The output is Java's schema, plus diagnostics
//!
//! Every key the comparator reads is spelled as `DrcReport` / `DrcViolation` /
//! `DrcViolationItem` spell it (`drc-connectivity.md` §4.8), so it has one loader for both sides:
//!
//! ```json
//! { "coordinate_units": "mm", "source": "x.dsn",
//!   "violations": [ { "type": "hole_clearance", "severity": "error",
//!                     "description": "Hole clearance violation between … (expected: 0.1906 mm, actual: 0.0000 mm)",
//!                     "items": [ { "description": "Pin [/GND]", "pos": {"x": …, "y": …}, "uuid": "12" }, … ] } ] }
//! ```
//!
//! Three fields are ours and the comparator's Java loader ignores them: `expected_mm` /
//! `actual_mm` (the numbers Java only prints inside `description`, so neither side depends on
//! `%.4f` rounding agreeing) and `layer` (the third component of upstream's dedup key, invisible
//! in the report but the first thing you want when a count is off).
//!
//! `uuid` is *not* comparable: it is the decimal item id, and our importer assigns ids in its own
//! insertion order. Positions are the match key, as `docs/ROADMAP.md` G1.2a specifies.
//!
//! # Units
//!
//! `convertCoordinate` (`DesignRulesChecker.java:456-486`) is
//! `Unit.scale(boardValue / scaleFactor, boardUnit, reportUnit)` — the *scalar* `board_to_dsn`,
//! so the DSN base offset is never added, and when the board unit already is the report unit no
//! scaling happens at all. Both are reproduced; the default report unit is `mm`, which is what
//! the checked-in oracles were generated with.
//!
//! # Why `hole_clearance` needs no hole check
//!
//! It reads like a second geometric predicate and it is not one. `DesignRulesChecker.isHole`
//! (`:417-424`) is `item instanceof Via || item instanceof Pin`, and `convertToDrcViolation` uses
//! it to *label* a record the ordinary clearance pass already produced. The real drill-vs-copper
//! mechanism is the `hole_edge` clearance class and the `drillHoleClearanceDelta` inflation
//! (§1.3, §1.4), both inert at the shipped `holeClearanceUm = 0.0`. So a hole-clearance
//! violation is a clearance violation with a pin or a via on one side, and the two types add up
//! to the `M` in Java's `(N unrouted and M violations)` line.

use std::fmt::Write as _;

use fr_board::item::{Item, ItemKind};
use fr_check::adapter::{centre_of_gravity, item_bounding_box, shape_store, violation_type};
use fr_check::{check_clearance, StoreView, Violation};
use fr_io::specctra::{ImportOptions, ImportedDesign, Unit};

/// The post-load overrides as `java -drc` runs them: **not at all**.
///
/// `HeadlessBoardManager.applyCopperToEdgeClearanceOverride` and
/// `assignHoleKeepoutClearanceClass` both bail out on
/// `routingJob.routerSettings.copperToEdgeClearanceUm == null` (`HeadlessBoardManager.java:386-389`).
/// `DefaultSettings.apply` is what fills those fields in, and `Freerouting.InitializeDRC`
/// (`Freerouting.java:164-231`) builds a bare `new RoutingJob(sessionId)` — whose `routerSettings`
/// is a raw `new RouterSettings()` with the `Double` fields still `null` — and never copies
/// `globalSettings.routerSettings` into it. So on the `-drc` path the board outline keeps the
/// clearance class the DSN gave it and no `board_edge` class is ever synthesised.
///
/// The evidence is in the oracles: `BoardOutline` appears in 98 golden records across 8 fixtures
/// and its required clearance is the board's own default every time (0.2002, 0.2542, 0.4002,
/// 2.0002 mm) — never the 0.5 mm the override would write. The `-de/-do` dump path *does* run it
/// (41 of 43 dumps contain a `board_edge` class), which is why the two oracles disagree and why
/// this emitter must pick the one it is being judged against.
/// [`ImportOptions::default`] selects the routing-path configuration instead.
pub const DRC_IMPORT_OPTIONS: ImportOptions = ImportOptions {
    copper_to_edge_clearance_um: None,
    hole_clearance_um: None,
};

/// Emitter knobs.
#[derive(Debug, Clone)]
pub struct ReportOptions {
    /// The unit coordinates and clearances are reported in. `mm` matches the checked-in oracles.
    pub unit: Unit,
    /// The spelling of [`ReportOptions::unit`] to echo into `coordinate_units` and into every
    /// `description`. Kept separate because Java prints whatever string it was given.
    pub unit_name: String,
}

impl Default for ReportOptions {
    fn default() -> Self {
        ReportOptions {
            unit: Unit::Mm,
            unit_name: "mm".to_string(),
        }
    }
}

impl ReportOptions {
    /// Parses a unit name into both fields, or returns `None` for an unknown unit.
    pub fn with_unit_name(mut self, name: &str) -> Option<ReportOptions> {
        self.unit = Unit::from_dsn(name)?;
        self.unit_name = name.to_string();
        Some(self)
    }
}

/// The report as JSON text.
///
/// `source` is echoed verbatim into `"source"`; the comparator keys rows by it, so pass the
/// input file's name.
pub fn report(design: &ImportedDesign, source: &str, options: &ReportOptions) -> String {
    let board = &design.board;
    let shapes = shape_store(board);
    let view = StoreView::new(
        board.store(),
        &shapes,
        &design.rules.clearance_matrix,
        board.layers().len(),
    );
    let violations = check_clearance(&view);

    // `boardValue / scaleFactor`, then into the report unit — but only when the units differ,
    // which is upstream's early return and not just an identity.
    let convert = |value: f64| {
        let dsn = design.transform.board_to_dsn(value);
        if options.unit == design.unit {
            dsn
        } else {
            Unit::scale(dsn, design.unit, options.unit)
        }
    };

    let mut out = String::with_capacity(256 * violations.len() + 512);
    out.push_str("{\n  \"$schema\": \"https://schemas.kicad.org/drc.v1.json\",\n");
    let _ = writeln!(out, "  \"coordinate_units\": {},", quote(&options.unit_name));
    let _ = writeln!(out, "  \"source\": {},", quote(source));
    let _ = writeln!(out, "  \"generator\": \"fr-check drc_report\",");
    out.push_str("  \"violations\": [");
    for (index, violation) in violations.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("\n    ");
        write_violation(&mut out, design, violation, &convert, &options.unit_name);
    }
    out.push_str(if violations.is_empty() {
        "]\n}\n"
    } else {
        "\n  ]\n}\n"
    });
    out
}

/// One `BOX` line per item, in **board units**, written to `sink`.
///
/// The diagnostic that tells an importer bug from a checker bug: it is the exact input to the
/// reported position, so a disagreement with Java's `-do` dump localises to geometry and an
/// agreement localises to the predicate. `fr-cli` puts it behind `--emit-boxes`; the example
/// this module supersedes reached it through `FR_EMIT_BOXES`.
pub fn emit_boxes(design: &ImportedDesign, sink: &mut dyn std::io::Write) -> std::io::Result<()> {
    let board = &design.board;
    for (id, item) in board.store().iter_ascending() {
        let bounds = item_bounding_box(board, item);
        let shapes = board
            .store()
            .slot_of(id)
            .map(|slot| board.default_tree().item_shapes(slot, id).len())
            .unwrap_or(0);
        writeln!(
            sink,
            "BOX {} {} cc={} shapes={} {} {} {} {} {}",
            id.raw(),
            item_description(design, item).replace(' ', "_"),
            item.clearance_class,
            shapes,
            bounds.ll.x,
            bounds.ll.y,
            bounds.ur.x,
            bounds.ur.y,
            match &item.kind {
                ItemKind::Pin(pin) => format!(
                    "centre={},{} comp={}",
                    pin.center.x, pin.center.y, item.component_no
                ),
                ItemKind::Via(via) => format!("centre={},{}", via.center.x, via.center.y),
                _ => String::new(),
            }
        )?;
    }
    Ok(())
}

fn write_violation(
    out: &mut String,
    design: &ImportedDesign,
    violation: &Violation,
    convert: &impl Fn(f64) -> f64,
    unit_name: &str,
) {
    let board = &design.board;
    let first = board.store().get(violation.a).expect("violation item a");
    let second = board.store().get(violation.b).expect("violation item b");
    let first_desc = item_description(design, first);
    let second_desc = item_description(design, second);
    let expected = convert(f64::from(violation.expected));
    let actual = convert(violation.actual);
    let kind = violation_type(first, second);
    let headline = if kind == "hole_clearance" {
        "Hole clearance violation between"
    } else {
        "Clearance violation between"
    };
    let description = format!(
        "{headline} {first_desc} and {second_desc} (expected: {expected:.4} {unit_name}, actual: {actual:.4} {unit_name})"
    );

    out.push('{');
    let _ = write!(out, "\"type\": {},", quote(kind));
    let _ = write!(out, " \"severity\": \"error\",");
    let _ = write!(out, " \"description\": {},", quote(&description));
    let _ = write!(out, " \"layer\": {},", violation.layer);
    let _ = write!(out, " \"expected_mm\": {},", number(expected));
    let _ = write!(out, " \"actual_mm\": {},", number(actual));
    out.push_str(" \"items\": [");
    for (index, (item, desc)) in [(first, first_desc), (second, second_desc)]
        .into_iter()
        .enumerate()
    {
        if index > 0 {
            out.push_str(", ");
        }
        let centre = centre_of_gravity(&item_bounding_box(&design.board, item));
        let _ = write!(
            out,
            "{{\"description\": {}, \"pos\": {{\"x\": {}, \"y\": {}}}, \"uuid\": {}}}",
            quote(&desc),
            number(convert(centre.x)),
            number(convert(centre.y)),
            quote(&item.id.raw().to_string())
        );
    }
    out.push_str("]}");
}

/// `DesignRulesChecker.getItemDescription` (`:388-416`).
///
/// The class-name fallback is spelled with upstream's Java class names, because the golden JSONs
/// carry them verbatim (`BoardOutline`, `ObstacleArea`).
fn item_description(design: &ImportedDesign, item: &Item) -> String {
    let mut description = match &item.kind {
        ItemKind::Trace(_) => "Trace".to_string(),
        ItemKind::Via(_) => "Via".to_string(),
        ItemKind::Pin(_) => "Pin".to_string(),
        ItemKind::ConductionArea(_) => "Conduction Area".to_string(),
        ItemKind::ObstacleArea(_) => "ObstacleArea".to_string(),
        ItemKind::BoardOutline(_) => "BoardOutline".to_string(),
        ItemKind::ComponentOutline(_) => "ComponentOutline".to_string(),
    };
    if let Some(net_no) = item.nets.as_slice().first() {
        if let Some(net) = design.rules.nets.get(*net_no) {
            description.push_str(" [");
            description.push_str(&net.name);
            description.push(']');
        }
    }
    description
}

// ---------------------------------------------------------------------------
// tiny JSON writer (neither fr-core nor fr-check has a serde dependency, and
// neither should grow one for a six-key document)
// ---------------------------------------------------------------------------

fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A JSON number that round-trips: Rust's `{}` prints the shortest decimal that reads back to the
/// same `f64`, which is what Gson does on the Java side.
fn number(value: f64) -> String {
    if !value.is_finite() {
        return "null".to_string();
    }
    let text = format!("{value}");
    if text.contains(['.', 'e', 'E']) {
        text
    } else {
        format!("{text}.0")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RoutingJob;

    /// Two round pads 300 µm apart edge to edge on nets that ask for 2000 µm of clearance.
    const CLASHING: &str = r#"(pcb clash
  (parser (string_quote "))
  (resolution um 10)
  (unit um)
  (structure
    (layer F.Cu (type signal))
    (layer B.Cu (type signal))
    (boundary (path signal 0 0 0 10000 0 10000 10000 0 10000 0 0))
    (rule (width 250) (clearance 2000))
  )
  (placement
    (component pad1
      (place P1 3000 5000 front 0)
      (place P2 4000 5000 front 0)
    )
  )
  (library
    (image pad1 (pin round_pad 1 0 0))
    (padstack round_pad (shape (circle F.Cu 700)) (shape (circle B.Cu 700)) (attach off))
  )
  (network
    (net A (pins P1-1))
    (net B (pins P2-1))
    (class default A B (rule (width 250) (clearance 2000)))
  )
  (wiring)
)"#;

    /// Same board, pads far enough apart to satisfy the class.
    const CLEAN: &str = r#"(pcb clean
  (parser (string_quote "))
  (resolution um 10)
  (unit um)
  (structure
    (layer F.Cu (type signal))
    (layer B.Cu (type signal))
    (boundary (path signal 0 0 0 10000 0 10000 10000 0 10000 0 0))
    (rule (width 250) (clearance 200))
  )
  (placement
    (component pad1
      (place P1 2000 5000 front 0)
      (place P2 8000 5000 front 0)
    )
  )
  (library
    (image pad1 (pin round_pad 1 0 0))
    (padstack round_pad (shape (circle F.Cu 700)) (shape (circle B.Cu 700)) (attach off))
  )
  (network
    (net A (pins P1-1))
    (net B (pins P2-1))
    (class default A B (rule (width 250) (clearance 200)))
  )
  (wiring)
)"#;

    fn design(src: &str) -> ImportedDesign {
        RoutingJob::new(src.as_bytes(), "board.dsn")
            .with_import_options(DRC_IMPORT_OPTIONS)
            .import()
            .expect("the board imports")
    }

    #[test]
    fn a_clean_board_reports_an_empty_array() {
        let design = design(CLEAN);
        let json = report(&design, "clean.dsn", &ReportOptions::default());
        assert!(json.contains("\"violations\": []"), "{json}");
        assert!(json.contains("\"coordinate_units\": \"mm\""));
        assert!(json.contains("\"source\": \"clean.dsn\""));
        assert!(json.ends_with("}\n"));
    }

    /// The two pads are on different nets and closer than the 2000 µm the class demands.
    ///
    /// Two records, not one: the padstack has a shape on both layers, and the dedup key is
    /// `(min_id, max_id, **layer**)` — upstream reports a violating pair once per layer it
    /// violates on, which is why `M` on a through-hole board is not a pair count. Both are
    /// labelled `hole_clearance`, because `DesignRulesChecker.isHole` calls a pin a hole.
    #[test]
    fn a_pin_pair_violation_is_reported_as_a_hole_clearance() {
        let design = design(CLASHING);
        let json = report(&design, "clash.dsn", &ReportOptions::default());
        assert_eq!(json.matches("\"severity\": \"error\"").count(), 2, "{json}");
        assert!(json.contains("\"layer\": 0"), "{json}");
        assert!(json.contains("\"layer\": 1"), "{json}");
        assert!(json.contains("\"type\": \"hole_clearance\""), "{json}");
        assert!(
            json.contains("Hole clearance violation between Pin [A] and Pin [B]"),
            "{json}"
        );
        // 2000 µm of clearance printed in millimetres, with Java's four decimals.
        assert!(json.contains("expected: 2.0000 mm"), "{json}");
    }

    /// `--unit` changes both the numbers and the label; `um` is the identity conversion for this
    /// board, which is upstream's early return rather than a scale of 1.
    #[test]
    fn the_report_unit_is_configurable() {
        let design = design(CLASHING);
        let options = ReportOptions::default()
            .with_unit_name("um")
            .expect("um is a unit");
        let json = report(&design, "clash.dsn", &options);
        assert!(json.contains("\"coordinate_units\": \"um\""), "{json}");
        assert!(json.contains("expected: 2000.0000 um"), "{json}");
    }

    #[test]
    fn an_unknown_unit_is_rejected() {
        assert!(ReportOptions::default().with_unit_name("furlong").is_none());
    }

    #[test]
    fn emit_boxes_writes_one_line_per_item() {
        let design = design(CLASHING);
        let mut sink: Vec<u8> = Vec::new();
        emit_boxes(&design, &mut sink).expect("writing to a Vec cannot fail");
        let text = String::from_utf8(sink).expect("ascii");
        assert_eq!(text.lines().count(), design.board.store().iter().count());
        assert!(text.lines().all(|line| line.starts_with("BOX ")));
    }
}
