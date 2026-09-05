//! G1.1 import-parity gate, as a cargo test over a five-board subset.
//!
//! `bench/tools/golden_compare.py --all --ours-dir …` is the full gate: 41 comparable fixtures,
//! thirteen field groups, ~20 s including the emit sweep. This test is the fast, in-repo version
//! of the same thing — it exists so a change to the importer that breaks parity fails in
//! `cargo test` rather than in a campaign someone has to remember to run.
//!
//! ```sh
//! cargo test -p fr-io --release --test golden_gate_test -- --ignored --nocapture
//! ```
//!
//! # Why it is `#[ignore]`d *and* environment-gated
//!
//! It needs three things this repository deliberately does not vendor:
//!
//! | need | default | override |
//! |---|---|---|
//! | the DSN fixtures (~90 MB, fetched by `bench/fetch_fixtures.sh`) | `/tmp/freerouting-upstream/fixtures` | `FR_FIXTURE_DIR` |
//! | the release jar's dumps (in-repo) | `bench/golden/dumps` | `FR_GOLDEN_DUMP_DIR` |
//! | `python3` on `PATH`, for the comparator | — | `FR_PYTHON` |
//!
//! `#[ignore]` keeps it out of the default run; the environment checks make it *skip loudly*
//! rather than fail when it is asked to run somewhere the inputs are absent. A test that fails
//! for want of a fixture teaches people to ignore red, which is worse than not running.
//!
//! # What runs in-process and what does not
//!
//! The import and the emit are in-process, through exactly the code the sweep uses
//! ([`fr_io::specctra::import`] + [`fr_io::golden_dump::emit`]) — so this test covers the emitter,
//! not a stale copy of it. The *comparison* shells out to `golden_compare.py`, which is
//! deliberate: that file is the normative definition of the schema, the field groups and the
//! lossy exclusions (`bench/tools/README.md`), and a second implementation in Rust would be a
//! second thing to keep in agreement with the oracle rather than a check on the first.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The subset, chosen so that between them the five boards exercise every group that has ever
/// been dirty in this gate — a regression in any of the wave-5 fixes fails at least one of them.
///
/// | fixture | what it pins |
/// |---|---|
/// | `Issue026-J2_reference` | component keepouts with per-keepout clearance classes; board outline + bounding box |
/// | `Issue034-Green14SegLED` | KiCad rounded-rectangle pads — the `Math.round` half-up rounding, whose absence moved 67 padstack corners by one board unit |
/// | `Issue015-StackOverflow` | conduction areas on *signal* layers (wiring scope, fixed state unobservable) and structure keepouts |
/// | `Issue066-Project_GP8B` | the `::N` package variant fallback: back-placed components resolve to the *base* image |
/// | `Issue230-CNH_Functional_Tester_1` | four layers, power-layer planes (structure scope, not wiring), 764 nets |
const SUBSET: [&str; 5] = [
    "Issue026-J2_reference",
    "Issue034-Green14SegLED",
    "Issue015-StackOverflow",
    "Issue066-Project_GP8B",
    "Issue230-CNH_Functional_Tester_1",
];

fn repo_root() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is `<root>/crates/fr-io`.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crate is two levels below the workspace root")
        .to_path_buf()
}

fn env_path(key: &str, fallback: PathBuf) -> PathBuf {
    std::env::var_os(key).map_or(fallback, PathBuf::from)
}

/// `None` plus a printed reason when a prerequisite is missing.
fn prerequisites() -> Option<(PathBuf, PathBuf, PathBuf, String)> {
    let root = repo_root();
    let fixtures = env_path(
        "FR_FIXTURE_DIR",
        PathBuf::from("/tmp/freerouting-upstream/fixtures"),
    );
    let dumps = env_path("FR_GOLDEN_DUMP_DIR", root.join("bench/golden/dumps"));
    let comparator = root.join("bench/tools/golden_compare.py");
    let python = std::env::var("FR_PYTHON").unwrap_or_else(|_| "python3".to_string());

    for (label, path) in [
        ("fixture directory (FR_FIXTURE_DIR)", &fixtures),
        ("Java dump directory (FR_GOLDEN_DUMP_DIR)", &dumps),
        ("comparator", &comparator),
    ] {
        if !path.exists() {
            eprintln!("SKIP golden gate: {label} not found at {}", path.display());
            return None;
        }
    }
    match Command::new(&python).arg("--version").output() {
        Ok(output) if output.status.success() => {}
        _ => {
            eprintln!("SKIP golden gate: {python} is not runnable (set FR_PYTHON)");
            return None;
        }
    }
    Some((fixtures, dumps, comparator, python))
}

#[test]
#[ignore = "needs the DSN fixture corpus and python3; run with --ignored"]
fn g11_golden_dump_subset_is_clean() {
    let Some((fixtures, dumps, comparator, python)) = prerequisites() else {
        return;
    };
    let out_dir = std::env::temp_dir().join(format!("fr-golden-gate-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).expect("create the emit directory");

    let mut failures: Vec<String> = Vec::new();
    for fixture in SUBSET {
        let source = fixtures.join(format!("{fixture}.dsn"));
        let dump = dumps.join(format!("{fixture}.import.dsn"));
        if !source.exists() || !dump.exists() {
            eprintln!("SKIP {fixture}: no fixture or no dump");
            continue;
        }

        // --- in-process: import + emit, the exact path `golden_emit` takes -------------------
        let bytes = std::fs::read(&source).expect("read the fixture");
        let design = match fr_io::specctra::import(&bytes) {
            Ok(design) => design,
            Err(error) => {
                failures.push(format!("{fixture}: import failed: {error}"));
                continue;
            }
        };
        let ours = out_dir.join(format!("{fixture}.json"));
        std::fs::write(
            &ours,
            fr_io::golden_dump::emit(&design, &format!("{fixture}.dsn")),
        )
        .expect("write the emitted document");

        // --- out of process: the comparator, which owns the schema and the exclusions --------
        let output = Command::new(&python)
            .arg(&comparator)
            .arg("--gate-only")
            .arg("--java-dump")
            .arg(&dump)
            .arg("--ours")
            .arg(&ours)
            .arg("--verbose")
            .output()
            .expect("run golden_compare.py");
        let report = String::from_utf8_lossy(&output.stdout).into_owned();
        match output.status.code() {
            Some(0) => println!("OK   {fixture}"),
            Some(1) => failures.push(format!("{fixture}: differences\n{report}")),
            other => failures.push(format!(
                "{fixture}: comparator exited with {other:?}\n{report}\n{}",
                String::from_utf8_lossy(&output.stderr)
            )),
        }
    }

    let _ = std::fs::remove_dir_all(&out_dir);
    assert!(
        failures.is_empty(),
        "G1.1 golden gate: {} of {} boards differ from the release dump\n\n{}",
        failures.len(),
        SUBSET.len(),
        failures.join("\n\n")
    );
}

/// The emitter must always produce a document the comparator will accept, even for a board that
/// is not in the subset — a schema break is a different failure from a parity break, and this
/// catches it without any fixture at all.
#[test]
fn emitted_document_is_structurally_complete() {
    // A minimal but complete board: outline, two layers, one padstack, one image, one placement.
    let dsn = br#"(pcb tiny
  (parser (string_quote ") (space_in_quoted_tokens on) (host_cad KiCad) (host_version 8))
  (resolution um 10)
  (unit um)
  (structure
    (layer F.Cu (type signal))
    (layer B.Cu (type signal))
    (boundary (path signal 0 0 0 10000 0 10000 10000 0 10000 0 0))
    (via Via[0-1]_800:400_um)
    (rule (width 250) (clearance 200))
  )
  (placement
    (component pad_image (place R1 5000 -5000 front 90))
  )
  (library
    (image pad_image (outline (path signal 100 0 0 100 0)) (pin Round[A]Pad_600_um 1 0 0))
    (padstack Round[A]Pad_600_um (shape (circle F.Cu 600)) (shape (circle B.Cu 600)) (attach off))
    (padstack Via[0-1]_800:400_um (shape (circle F.Cu 800)) (shape (circle B.Cu 800)) (attach off))
  )
  (network (class default (rule (width 250))))
  (wiring)
)"#;
    let design = fr_io::specctra::import(dsn).expect("the tiny board imports");
    let json = fr_io::golden_dump::emit(&design, "tiny.dsn");

    assert!(json.starts_with('{') && json.trim_end().ends_with('}'));
    // `validate_document`'s required top-level keys (`golden_compare.py:_REQUIRED_TOP`). The keys
    // are checked, not the values: a group that legitimately comes out empty must still be
    // present, because the comparator distinguishes "absent" from "empty".
    for key in [
        "schema",
        "resolution",
        "unit",
        "layers",
        "rules",
        "padstacks",
        "images",
        "components",
        "nets",
        "net_classes",
        "via_infos",
        "via_rules",
        "via_padstacks",
        "wiring",
        "boundary",
        "structure_areas",
    ] {
        assert!(
            json.contains(&format!("\"{key}\":")),
            "emitted document is missing the top-level key {key:?}"
        );
    }
    assert!(json.contains("\"schema\": \"fast-routing.golden-dump/1\""));
    // The bounding box plus the outline contour — `Structure.write_boundaries` writes both.
    assert!(
        json.contains("\"kind\": \"boundary\", \"shape\": \"rect\", \"layer\": \"pcb\""),
        "the board bounding box is missing from `boundary`"
    );
    // `Package.write_scope` writes the image outline on the pseudo-layer `signal`.
    assert!(
        json.contains("\"kind\": \"outline\""),
        "the package outline is missing from `images[].outlines`"
    );
}
