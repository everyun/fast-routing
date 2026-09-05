//! Gate G1.2a bridge: `.dsn` in, `java -drc` shaped JSON out.
//!
//! ```text
//! cargo run -p fr-check --example drc_report --release -- <in.dsn> <out.json>
//! ```
//!
//! What it does is exactly what `Freerouting.java:332-352` does when it is
//! handed `-drc` and no SES file: import the board, run the whole-board
//! clearance DRC on it, and print `DesignRulesChecker.generateReport`'s
//! `violations` array. Nothing is routed, so the numbers are a property of the
//! *input board* and of the checker alone — which is the whole point of the
//! gate: it separates importer/checker parity from any router confound.
//!
//! # The output is Java's schema, plus diagnostics
//!
//! Every key `bench/tools/drc_compare.py` reads is spelled as
//! `DrcReport`/`DrcViolation`/`DrcViolationItem` spell it (`drc-connectivity.md`
//! §4.8), so the comparator has one loader for both sides:
//!
//! ```json
//! { "coordinate_units": "mm", "source": "x.dsn",
//!   "violations": [ { "type": "hole_clearance", "severity": "error",
//!                     "description": "Hole clearance violation between … (expected: 0.1906 mm, actual: 0.0000 mm)",
//!                     "items": [ { "description": "Pin [/GND]", "pos": {"x": …, "y": …}, "uuid": "12" }, … ] } ] }
//! ```
//!
//! Three fields are ours and are ignored by the comparator's Java loader:
//! `expected_mm` / `actual_mm` (the numbers Java only prints inside
//! `description`, so we do not have to depend on `%.4f` rounding agreeing) and
//! `layer` (the third component of upstream's dedup key, invisible in the
//! report but the first thing you want when a count is off).
//!
//! `uuid` is *not* comparable: it is the decimal item id, and our importer
//! assigns ids in its own insertion order. Positions are the match key, exactly
//! as `docs/ROADMAP.md` G1.2a specifies.
//!
//! # Units
//!
//! `convertCoordinate` (`DesignRulesChecker.java:456-486`) is
//! `Unit.scale(boardValue / scaleFactor, boardUnit, reportUnit)` — note that the
//! *scalar* `board_to_dsn` is used, so the DSN base offset is never added, and
//! that when the board unit already is the report unit no scaling happens at
//! all. Both are reproduced; `--unit` defaults to `mm`, which is what the
//! checked-in oracles were generated with.
//!
//! # Why `hole_clearance` needs no hole check
//!
//! It reads like a second geometric predicate and it is not one.
//! `DesignRulesChecker.isHole` (`:417-424`) is `item instanceof Via || item
//! instanceof Pin`, and `convertToDrcViolation` uses it to *label* a record that
//! the ordinary clearance pass already produced. Upstream's comment says so:
//! "Pins are treated as holes for DRC classification to match expected output,
//! although this might include SMT pins". The real drill-vs-copper mechanism is
//! the `hole_edge` clearance class and the `drillHoleClearanceDelta` inflation
//! (`drc-connectivity.md` §1.3, §1.4), and both are inert at the shipped
//! `holeClearanceUm = 0.0`. So a hole-clearance violation is a clearance
//! violation with a pin or a via on one side, and the two types add up to the
//! `M` in Java's `(N unrouted and M violations)` log line.

use std::fmt::Write as _;

use fr_board::item::{Item, ItemKind};
use fr_check::adapter::{centre_of_gravity, item_bounding_box, shape_store, violation_type};
use fr_check::{check_clearance, StoreView, Violation};
use fr_io::specctra::{self, ImportOptions, ImportedDesign, Unit};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut positional: Vec<&str> = Vec::new();
    let mut unit_name = "mm".to_string();
    let mut router_overrides = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--unit" => {
                index += 1;
                match args.get(index) {
                    Some(value) => unit_name = value.clone(),
                    None => fail("--unit needs a value"),
                }
            }
            "--pin-frame-fix" => {
                eprintln!(
                    "drc_report: --pin-frame-fix is a no-op and is deprecated; the importer now \
                     places pads in the pin frame itself (fr-io specctra::build::placed_pad_shapes)"
                );
            }
            "--router-overrides" => router_overrides = true,
            other => positional.push(other),
        }
        index += 1;
    }
    if positional.len() != 2 {
        eprintln!(
            "usage: drc_report [--unit mm|mil|inch|um] [--pin-frame-fix (deprecated, no-op)] \
             [--router-overrides] <in.dsn> <out.json>"
        );
        std::process::exit(2);
    }
    let Some(report_unit) = Unit::from_dsn(&unit_name) else {
        fail(&format!("unknown unit {unit_name}"));
    };

    let bytes = match std::fs::read(positional[0]) {
        Ok(bytes) => bytes,
        Err(error) => fail(&format!("read {}: {error}", positional[0])),
    };
    // `-drc` runs the post-load overrides in their *off* state; see
    // `DRC_IMPORT_OPTIONS`.
    let options = if router_overrides {
        ImportOptions::default()
    } else {
        DRC_IMPORT_OPTIONS
    };
    let design = match specctra::import_with_options(&bytes, options) {
        Ok(design) => design,
        Err(error) => fail(&format!("import failed: {error}")),
    };
    let source = std::path::Path::new(positional[0])
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();

    if std::env::var_os("FR_EMIT_BOXES").is_some() {
        emit_boxes(&design);
    }

    let json = report(&design, &source, report_unit, &unit_name);
    if let Err(error) = std::fs::write(positional[1], json) {
        fail(&format!("write {}: {error}", positional[1]));
    }
}

/// `FR_EMIT_BOXES=1` — one `BOX` line per item on stderr, in **board units**.
///
/// The diagnostic that tells an importer bug from a checker bug: it is the exact
/// input to the reported position, so a disagreement with Java's `-do` dump
/// localises to geometry, and an agreement localises to the predicate.
fn emit_boxes(design: &ImportedDesign) {
    let board = &design.board;
    for (id, item) in board.store().iter_ascending() {
        let bounds = item_bounding_box(board, item);
        let shapes = board
            .store()
            .slot_of(id)
            .map(|slot| board.default_tree().item_shapes(slot, id).len())
            .unwrap_or(0);
        eprintln!(
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
        );
    }
}

/// The post-load overrides as `java -drc` runs them: **not at all**.
///
/// `HeadlessBoardManager.applyCopperToEdgeClearanceOverride` and
/// `assignHoleKeepoutClearanceClass` both bail out on
/// `routingJob.routerSettings.copperToEdgeClearanceUm == null`
/// (`HeadlessBoardManager.java:386-389`). `DefaultSettings.apply` is what fills
/// those fields in, and `Freerouting.InitializeDRC` (`Freerouting.java:164-231`)
/// builds a bare `new RoutingJob(sessionId)` — whose `routerSettings` is a raw
/// `new RouterSettings()` with the `Double` fields still `null` — and never
/// copies `globalSettings.routerSettings` into it. So on the `-drc` path the
/// board outline keeps the clearance class the DSN gave it and no `board_edge`
/// class is ever synthesised.
///
/// The evidence is in the oracles: `BoardOutline` appears in 98 golden records
/// across 8 fixtures and its required clearance is the board's own default every
/// time (0.2002, 0.2542, 0.4002, 2.0002 mm) — never the 0.5 mm the override
/// would write. The `-de/-do` dump path *does* run it (41 of 43 dumps contain a
/// `board_edge` class), which is why the two oracles disagree and why this bin
/// must pick the one it is being judged against. `--router-overrides` selects
/// the routing-path configuration instead.
const DRC_IMPORT_OPTIONS: ImportOptions = ImportOptions {
    copper_to_edge_clearance_um: None,
    hole_clearance_um: None,
    cutout_clearance_um: None,
};

fn fail(message: &str) -> ! {
    eprintln!("drc_report: {message}");
    std::process::exit(1)
}

// ---------------------------------------------------------------------------
// the report
// ---------------------------------------------------------------------------

fn report(design: &ImportedDesign, source: &str, report_unit: Unit, unit_name: &str) -> String {
    let board = &design.board;
    let shapes = shape_store(board);
    let view = StoreView::new(
        board.store(),
        &shapes,
        &design.rules.clearance_matrix,
        board.layers().len(),
    );
    let violations = check_clearance(&view);

    // `boardValue / scaleFactor`, then into the report unit — but only when the
    // units differ, which is upstream's early return and not just an identity.
    let convert = |value: f64| {
        let dsn = design.transform.board_to_dsn(value);
        if report_unit == design.unit {
            dsn
        } else {
            Unit::scale(dsn, design.unit, report_unit)
        }
    };

    let mut out = String::with_capacity(256 * violations.len() + 512);
    out.push_str("{\n  \"$schema\": \"https://schemas.kicad.org/drc.v1.json\",\n");
    let _ = writeln!(out, "  \"coordinate_units\": {},", quote(unit_name));
    let _ = writeln!(out, "  \"source\": {},", quote(source));
    let _ = writeln!(out, "  \"generator\": \"fr-check drc_report\",");
    out.push_str("  \"violations\": [");
    for (index, violation) in violations.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("\n    ");
        write_violation(&mut out, design, violation, &convert, unit_name);
    }
    out.push_str(if violations.is_empty() {
        "]\n}\n"
    } else {
        "\n  ]\n}\n"
    });
    out
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
/// The class-name fallback is spelled with upstream's Java class names, because
/// the golden JSONs carry them verbatim (`BoardOutline`, `ObstacleArea`).
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
// tiny JSON writer (fr-check has no serde dependency and should not grow one)
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

/// A JSON number that round-trips: Rust's `{}` prints the shortest decimal that
/// reads back to the same `f64`, which is what Gson does on the Java side.
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

// ---------------------------------------------------------------------------
// `--pin-frame-fix`: retired
// ---------------------------------------------------------------------------
//
// The flag used to undo an importer defect from this side of the fence.
// `fr_board::Padstack`'s contract is that pad shapes are **pin-relative** —
// `search_tree::drill_shapes` translates them by `Pin::center` — and
// `fr_io::specctra::build::placed_pad_shapes` used to leave the pin's
// package-relative location baked into them. Since `PadstackKey` does not carry
// that location (a pin-relative padstack must not depend on it), the first pin
// interned under a key donated its offset to every later pin sharing the key,
// so most pins carried a *foreign* pin's displacement. The flag replayed the
// interning order to recover each pin's donor and translated the donor's offset
// back out, which measured the damage without repairing it.
//
// The importer now subtracts the pin's own placed offset (`placed_pad_shapes`,
// "the frame"), so there is nothing left to undo: the flag is accepted, warns,
// and does nothing. Its numbers are the ones the default path now produces —
// G1.2a went from 2/45 exact without it and 39/45 with it to 44/45 with no
// flags at all.
