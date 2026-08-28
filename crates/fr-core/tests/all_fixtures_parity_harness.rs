//! The corpus harness: every Specctra file the fixture tree holds goes through the M1 pipeline.
//!
//! # What this replaces
//!
//! It used to assert `valid_count >= 140` against a corpus that has never had more than 53
//! files, on a code path (`parse_dsn` + `BasicBoard`) that is no longer the product. The number
//! came from a header comment claiming "148 upstream fixtures"; nothing in the repository has
//! ever produced 148, so the assertion was unreachable and the test was kept green only by the
//! `if fixtures.is_empty() { return; }` guard above it — a vacuous pass wearing a strict-looking
//! bound. `docs/ROADMAP.md` G1.3 asks for it to become manifest-driven, and this is that.
//!
//! # The three sources of truth, and how they check each other
//!
//! | source | what it says | failure it catches |
//! |---|---|---|
//! | the fixture tree ([`fixture_dir`]) | what is actually on disk, walked recursively | a corpus that was never fetched, or fetched short |
//! | `bench/fixtures-manifest.json` | which of the 46 top-level fixtures `bench/manifest.py` classified as Specctra DSN | a corpus that silently lost files, *by name* |
//! | [`fr_io::specctra::import`] | which files the importer accepts | a regression that turns a board into a parse error |
//!
//! No count is hard-coded. The floor is the manifest's own Specctra count, the ceiling is the
//! census, and the two are cross-checked by name — so shrinking the corpus fails, and editing
//! the manifest to match a shrunken corpus fails too, because the named files must still be on
//! disk and must still import.
//!
//! # It fails loudly when the corpus is absent
//!
//! `docs/ROADMAP.md` G1.3: "fixture-dependent tests fail loudly with fetch script". A test that
//! goes green because its inputs are missing teaches people to ignore red. Run
//! `bench/fetch_fixtures.sh`, or point `FR_FIXTURE_DIR` at an existing tree.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fr_core::{BoardCensus, RoutingJob};
use fr_io::specctra;

/// Where the DSN corpus lives. Same default and same override as `fr-io`'s golden gate.
fn fixture_dir() -> PathBuf {
    let dir = std::env::var_os("FR_FIXTURE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/freerouting-upstream/fixtures"));
    assert!(
        dir.is_dir(),
        "fixture directory '{}' is missing; run bench/fetch_fixtures.sh or set FR_FIXTURE_DIR",
        dir.display()
    );
    dir
}

fn repo_root() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is `<root>/crates/fr-core`.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate is two levels below the workspace root")
        .to_path_buf()
}

/// Every `*.dsn` under `dir`, recursively, sorted.
fn walk_dsn(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_dsn(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "dsn") {
            out.push(path);
        }
    }
}

fn corpus() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    walk_dsn(&fixture_dir(), &mut paths);
    paths.sort();
    assert!(
        !paths.is_empty(),
        "no *.dsn under {}; run bench/fetch_fixtures.sh",
        fixture_dir().display()
    );
    paths
}

/// `bench/fixtures-manifest.json` → `name` ⇒ `properties.is_specctra_dsn`.
///
/// A five-line scan rather than a JSON dependency: the file is machine-written by
/// `bench/manifest.py` with one `"name"` key and one `"is_specctra_dsn"` key per fixture, in
/// that order, and nothing else in the document spells either. If that ever stops being true
/// the pairing collapses to zero entries and [`manifest_specctra_names`]'s own assertion fires,
/// which is the loud failure we want — never a silently weaker gate.
fn manifest_specctra_names() -> BTreeMap<String, bool> {
    let path = repo_root().join("bench/fixtures-manifest.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));

    let mut map = BTreeMap::new();
    let mut rest = text.as_str();
    while let Some(at) = rest.find("\"name\":") {
        rest = &rest[at + "\"name\":".len()..];
        let Some(open) = rest.find('"') else { break };
        let after = &rest[open + 1..];
        let Some(close) = after.find('"') else { break };
        let name = after[..close].to_string();
        rest = &after[close + 1..];

        let Some(flag_at) = rest.find("\"is_specctra_dsn\":") else {
            break;
        };
        // A later fixture's `name` must not be crossed to reach this fixture's flag.
        if let Some(next_name) = rest.find("\"name\":") {
            assert!(
                flag_at < next_name,
                "manifest layout changed: '{name}' has no is_specctra_dsn before the next entry"
            );
        }
        let tail = rest[flag_at + "\"is_specctra_dsn\":".len()..].trim_start();
        map.insert(name, tail.starts_with("true"));
    }

    assert!(
        !map.is_empty(),
        "no fixtures parsed out of {}; bench/manifest.py's output format changed and this \
         scanner needs updating",
        path.display()
    );
    map
}

/// What the census found, so a failure message can say which side is wrong.
#[derive(Default)]
struct Census {
    imported: Vec<String>,
    rejected: Vec<(String, String)>,
}

/// **The gate.** Every `.dsn` in the tree either imports or is provably not a DSN, the corpus
/// covers every Specctra fixture the manifest names, and both counts are reported.
#[test]
fn every_specctra_fixture_in_the_corpus_imports() {
    let paths = corpus();
    let manifest = manifest_specctra_names();
    let expected: Vec<&String> = manifest
        .iter()
        .filter(|(_, is_dsn)| **is_dsn)
        .map(|(name, _)| name)
        .collect();

    let mut census = Census::default();
    for path in &paths {
        let name = path
            .file_name()
            .expect("a walked file has a name")
            .to_string_lossy()
            .to_string();
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        match specctra::import(&bytes) {
            Ok(design) => {
                // Not a smoke check: a board with no layers or a collapsed scale factor imports
                // "successfully" and is useless downstream, which is exactly the failure the old
                // `>= 140` assertion could never see.
                assert!(!design.board.layers().is_empty(), "{name}: no layers");
                assert!(design.scale_factor > 0.0, "{name}: collapsed scale factor");
                assert!(!design.board.store().is_empty(), "{name}: empty board");
                census.imported.push(name);
            }
            Err(error) => {
                // The only legitimate rejection is a file that is not a DSN at all — Issue006 is
                // an OLE compound document that happens to carry a `.dsn` extension.
                let header = bytes
                    .iter()
                    .position(|b| !b.is_ascii_whitespace())
                    .map(|start| &bytes[start..])
                    .unwrap_or(&bytes);
                assert!(
                    header.len() < 4 || !header[..4].eq_ignore_ascii_case(b"(pcb"),
                    "{name} looks like a DSN but was rejected: {error}"
                );
                census.rejected.push((name, error.to_string()));
            }
        }
    }

    // Every Specctra fixture the manifest names is on disk and did import. This is the check
    // that makes a short corpus fail instead of quietly narrowing the gate.
    let missing: Vec<&&String> = expected
        .iter()
        .filter(|name| !census.imported.iter().any(|got| &got == *name))
        .collect();
    assert!(
        missing.is_empty(),
        "{} fixture(s) named by bench/fixtures-manifest.json did not import: {missing:?}",
        missing.len()
    );

    // Nothing is unaccounted for: every walked file is in exactly one bucket.
    assert_eq!(
        census.imported.len() + census.rejected.len(),
        paths.len(),
        "census lost a file"
    );

    // And the manifest's non-DSN entries really are the ones we rejected — the classification
    // agrees in both directions, so neither side can drift alone.
    for (name, is_dsn) in &manifest {
        let rejected = census.rejected.iter().any(|(got, _)| got == name);
        assert_eq!(
            !*is_dsn, rejected,
            "{name}: manifest says is_specctra_dsn={is_dsn}, importer {}",
            if rejected { "rejected it" } else { "accepted it" }
        );
    }

    println!(
        "corpus: {} files, {} imported, {} not DSN ({:?}); manifest floor {}",
        paths.len(),
        census.imported.len(),
        census.rejected.len(),
        census
            .rejected
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        expected.len()
    );
}

/// The pipeline, end to end, on the boards the golden gate uses.
///
/// A subset rather than the corpus because [`fr_core::Referee`] runs the whole-board clearance
/// DRC, which is minutes on the big boards in a debug build. These five are `fr-io`'s
/// `golden_gate_test::SUBSET`: between them they carry component keepouts, rounded-rectangle
/// pads, conduction areas on signal layers, back-placed package variants and four layers with
/// power planes.
#[test]
fn the_pipeline_runs_end_to_end_on_the_gate_subset() {
    const SUBSET: [&str; 5] = [
        "Issue026-J2_reference.dsn",
        "Issue034-Green14SegLED.dsn",
        "Issue015-StackOverflow.dsn",
        "Issue066-Project_GP8B.dsn",
        "Issue230-CNH_Functional_Tester_1.dsn",
    ];
    let dir = fixture_dir();
    for name in SUBSET {
        let path = dir.join(name);
        assert!(
            path.is_file(),
            "{name} is missing from {}; run bench/fetch_fixtures.sh",
            dir.display()
        );
        let report = RoutingJob::from_path(&path)
            .expect("the fixture reads")
            .run()
            .unwrap_or_else(|error| panic!("{name}: {error}"));

        assert!(!report.routed, "{name}: M1 must not route");
        assert!(report.layer_count >= 2, "{name}: too few layers");
        assert!(report.census.total() > 0, "{name}: empty board");
        assert_eq!(
            report.census,
            BoardCensus::of(&report.design.board),
            "{name}: the census disagrees with the board it came from"
        );

        // The session is the v2 writer's and it names the file it was built from. Every fixture
        // name here contains `-` or `_`, both in `SES_RESERVED_CHARS`, so it is always quoted.
        let session_name = name.replace(".dsn", ".ses");
        assert!(
            report
                .ses
                .starts_with(&format!("(session \"{session_name}\"")),
            "{name}: unexpected session header: {:?}",
            &report.ses[..report.ses.len().min(80)]
        );
        assert!(report.ses.contains("(routes"), "{name}: no routes scope");
        assert!(report.ses.contains("(placement"), "{name}: no placement");

        // The referee's counts are the ones the score was computed from — the pipeline must not
        // report one number and score another.
        assert_eq!(
            report.stats.clearance_violation_count,
            report.violations.len() as u64
        );
        assert_eq!(
            report.stats.incomplete_count,
            report.connectivity.incomplete_count as u64
        );
        assert!(
            (0.0..=1000.0).contains(&report.score.normalized),
            "{name}: score {} out of range",
            report.score.normalized
        );

        println!(
            "{name}: {} items, {} violations, {} incomplete, score {:.2}",
            report.census.total(),
            report.stats.clearance_violation_count,
            report.stats.incomplete_count,
            report.score.normalized
        );
    }
}
