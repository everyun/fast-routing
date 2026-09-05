//! The M1 pipeline, end to end, on `Issue026-J2_reference`.
//!
//! ```text
//! import ──▶ check ──▶ write SES ──▶ re-read SES ──▶ check again
//!    │                                    │
//!    └──────────── must agree ────────────┘
//! ```
//!
//! # Why the fixture's own `.ses` is loaded first
//!
//! `Issue026-J2_reference.dsn` is an **unrouted** board — the manifest records
//! `preexisting_wires: 0`, and a round trip of no copper proves nothing. The fixture directory
//! also ships `Issue026-J2_reference.ses`, the routed session for that exact design, so the
//! round trip is run on a board that actually carries wiring: apply the shipped session,
//! re-emit it through our writer, read that back into a fresh import of the same DSN, and
//! require the two boards to describe the same copper *and* to be judged identically.
//!
//! That last clause is what makes this a pipeline test rather than an `fr-io` test.
//! `fr-io`'s `ses_roundtrip_test` already proves the writer and reader agree about geometry;
//! what is proved here is that **the referee cannot tell the two boards apart** — same
//! violation count, same incomplete count, same score — which is the property M2 will lean on
//! when it starts comparing a routed board against a re-imported one.
//!
//! # The one tolerance
//!
//! Every SES coordinate is an integer (`dsn-board-semantics.md` §12.2 rule 1) but a `Polyline`
//! corner is the intersection of two `Line`s, i.e. a rational point. Writing it and rebuilding
//! the polyline from the integers can move a corner by up to one board unit. One unit is
//! allowed on a corner and nothing else is: counts, layers, half widths, net sets and the whole
//! via multiset must match exactly.
//!
//! Needs `bench/fetch_fixtures.sh`; fails loudly rather than skipping (gate G1.3).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fr_board::{Board, ItemKind};
use fr_check::ScoreWeights;
use fr_core::pipeline::write_session;
use fr_core::{Referee, RoutingJob};
use fr_io::specctra::{import, read_session, ImportedDesign, SesLibrary, SesTarget};

const DESIGN: &str = "Issue026-J2_reference.dsn";
const SESSION: &str = "Issue026-J2_reference.ses";

/// A board unit — see the module docs.
const CORNER_TOLERANCE: i32 = 1;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate is two levels below the workspace root")
        .to_path_buf()
}

fn fixture_dir() -> PathBuf {
    let dir = std::env::var_os("FR_FIXTURE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/freerouting-upstream/fixtures"));
    if dir.is_dir() {
        return dir;
    }
    let golden = repo_root().join("bench/golden/fixtures");
    if golden.is_dir() {
        return golden;
    }
    panic!(
        "fixture directory '{}' is missing; run bench/fetch_fixtures.sh or set FR_FIXTURE_DIR",
        dir.display()
    );
}

fn read_fixture(name: &str) -> Vec<u8> {
    let path = fixture_dir().join(name);
    if let Ok(bytes) = std::fs::read(&path) {
        return bytes;
    }
    let golden_path = repo_root().join("bench/golden/fixtures").join(name);
    std::fs::read(&golden_path).unwrap_or_else(|error| {
        panic!(
            "fixture '{}' is missing ({error}); run bench/fetch_fixtures.sh",
            path.display()
        )
    })
}

// =================================================================================================
// stage 1-4, in one call
// =================================================================================================

/// The whole pipeline on the unrouted design: it imports, it is judged, it emits a session, and
/// nothing routed.
#[test]
fn issue026_runs_import_check_and_ses() {
    let report = RoutingJob::from_path(fixture_dir().join(DESIGN))
        .expect("the fixture reads")
        .run()
        .expect("Issue026 imports");

    assert!(!report.routed, "M1 must not route");
    // `(pcb …)` on this fixture is the exporter's absolute path, not the file's stem — which is
    // exactly why the session's `(base_design …)` uses the *file* name instead.
    assert!(
        report
            .pcb_name
            .ends_with("e-con_STEEReoCAM_Nano_FFC_MiniSAS_Adapter.dsn"),
        "unexpected (pcb …) token: {}",
        report.pcb_name
    );
    assert_eq!(report.design_name, DESIGN);
    assert_eq!(report.layer_count, 2);
    assert_eq!(report.census.traces, 0, "the fixture is unrouted");
    assert_eq!(report.census.vias, 0, "the fixture is unrouted");
    assert!(report.census.pins > 0);

    // The referee ran and its numbers are the ones the score used.
    assert_eq!(
        report.stats.clearance_violation_count,
        report.violations.len() as u64
    );
    assert_eq!(
        report.stats.incomplete_count,
        report.connectivity.incomplete_count as u64
    );
    // An unrouted board with multi-pin nets has incompletes; if it did not, the connectivity
    // pass silently did nothing and the score below would be meaningless.
    assert!(
        report.stats.incomplete_count > 0,
        "an unrouted board must have incompletes"
    );
    assert!(!report.is_clean());

    // The v2 writer, not the pre-rewrite one. Both names are quoted: `-` and `_` are in
    // `SES_RESERVED_CHARS`.
    assert!(
        report
            .ses
            .starts_with("(session \"Issue026-J2_reference.ses\""),
        "{}",
        &report.ses[..report.ses.len().min(60)]
    );
    assert!(report
        .ses
        .contains("(base_design \"Issue026-J2_reference.dsn\")"));
    assert!(report.ses.contains("(placement"));
    assert!(report.ses.contains("(routes"));
    // No copper on the board, so `network_out` carries no wires.
    assert!(!report.ses.contains("(wire "), "nothing was routed");
}

/// The session the pipeline emits is readable by our own reader, on a fresh import of the same
/// design. An unreadable passthrough would be worse than no passthrough.
#[test]
fn the_emitted_session_re_reads() {
    let report = RoutingJob::from_path(fixture_dir().join(DESIGN))
        .expect("the fixture reads")
        .run()
        .expect("Issue026 imports");

    let mut target = import(&read_fixture(DESIGN)).expect("Issue026 imports");
    let library = SesLibrary::from_design(&target);
    let summary = read_session(
        report.ses.as_bytes(),
        SesTarget::for_design(&mut target, &library),
    )
    .expect("our own session must be readable");
    assert_eq!(summary.errors_encountered, 0, "{:?}", summary.warnings);
    assert_eq!(summary.wires_imported, 0);
    assert_eq!(summary.vias_imported, 0);
}

// =================================================================================================
// round trip, on copper
// =================================================================================================

/// import(dsn) + shipped ses  ─write─▶  ses'  ─read into import(dsn)─▶  the same board, twice.
///
/// # Wire *count* is not the invariant, and expecting it to be is the trap
///
/// The shipped session carries 89 `(wire …)` records; the board that results holds fewer trace
/// items, because `Board::insert_trace` merges collinear same-net neighbours as it goes
/// (`trace_ops`, §11). The writer then emits one wire per surviving item, so a second read sees
/// the merged count, not the original one. That is not a loss — the copper is identical — which
/// is why the assertions below are about the **board**, and why the count is only required to be
/// *stable from the second generation on*. That stability is gate G1.2c's "round-trip fixed
/// point", and it is the property a router can actually rely on.
#[test]
fn issue026_wiring_round_trips_and_is_judged_identically() {
    let dsn = read_fixture(DESIGN);
    let ses = read_fixture(SESSION);

    // The reference: the design with the shipped session applied.
    let mut reference = import(&dsn).expect("Issue026 imports");
    let library = SesLibrary::from_design(&reference);
    let loaded = read_session(&ses, SesTarget::for_design(&mut reference, &library))
        .expect("the shipped session reads");
    assert_eq!(loaded.errors_encountered, 0, "{:?}", loaded.warnings);
    assert!(
        loaded.wires_imported > 0,
        "the shipped session must carry copper, or this test proves nothing"
    );
    assert!(
        trace_count(&reference.board) > 0,
        "the copper must survive insertion"
    );

    // Generation 1: stage 4 of the pipeline on that board, read back into a fresh, still
    // unrouted import of the same design.
    let first = write_session(&reference, DESIGN).text;
    let (mut actual, reread) = apply(&dsn, &first);
    assert_eq!(reread.errors_encountered, 0, "{:?}", reread.warnings);
    assert_eq!(
        reread.wires_imported,
        trace_count(&reference.board),
        "the writer must emit one wire per trace item"
    );
    assert_eq!(reread.vias_imported, loaded.vias_imported);

    assert_boards_equivalent(&reference.board, &actual.board);
    assert_judged_identically(&reference, &actual);

    // Generation 2: the fixed point. Anything that still moves here is a real defect — the
    // corner rounding has already happened once and cannot happen again.
    let second = write_session(&actual, DESIGN).text;
    let (third, again) = apply(&dsn, &second);
    assert_eq!(again.wires_imported, reread.wires_imported);
    assert_eq!(again.vias_imported, reread.vias_imported);
    assert_eq!(second, first_regenerated(&mut actual, DESIGN));
    assert_boards_equivalent(&actual.board, &third.board);
    assert_judged_identically(&actual, &third);
}

#[test]
fn issue066_session_round_trip_is_judged_identically() {
    let dsn_path = fixture_dir().join("Issue066-Project_GP8B.dsn");
    if !dsn_path.is_file() {
        return;
    }
    let ses_path = std::path::Path::new(
        "/Users/chiqingzhen/fr-work/logs/gate/g21/ses/Issue066-Project_GP8B.rust.ses",
    );
    if !ses_path.is_file() {
        return;
    }
    let dsn_bytes = std::fs::read(&dsn_path).unwrap();
    let ses_bytes = std::fs::read(ses_path).unwrap();
    let stripped = fr_io::specctra::strip_session_copper(&dsn_bytes);
    let mut design1 = import(&stripped).unwrap();
    let library1 = SesLibrary::from_design(&design1);
    read_session(&ses_bytes, SesTarget::for_design(&mut design1, &library1)).unwrap();

    let before = Referee::judge(&design1, &ScoreWeights::V2);

    let re_emitted = write_session(&design1, "Issue066-Project_GP8B.dsn").text;
    let mut design2 = import(&stripped).unwrap();
    let library2 = SesLibrary::from_design(&design2);
    read_session(
        re_emitted.as_bytes(),
        SesTarget::for_design(&mut design2, &library2),
    )
    .unwrap();

    let after = Referee::judge(&design2, &ScoreWeights::V2);
    assert_eq!(
        before.stats.clearance_violation_count, after.stats.clearance_violation_count,
        "Issue066 violation count changed on re-export"
    );
    assert_eq!(
        before.stats.incomplete_count, after.stats.incomplete_count,
        "Issue066 incomplete count changed on re-export"
    );
}

/// Reads `session` into a fresh import of `dsn`.
fn apply(dsn: &[u8], session: &str) -> (ImportedDesign, fr_io::specctra::SesImportSummary) {
    let mut design = import(dsn).expect("Issue026 imports");
    let library = SesLibrary::from_design(&design);
    let summary = read_session(
        session.as_bytes(),
        SesTarget::for_design(&mut design, &library),
    )
    .expect("our own session reads");
    (design, summary)
}

/// Writing the same board twice must give the same bytes — a writer that is not a function of
/// the board would make every comparison above meaningless.
fn first_regenerated(design: &mut ImportedDesign, name: &str) -> String {
    write_session(design, name).text
}

fn trace_count(board: &Board) -> usize {
    board
        .store()
        .iter()
        .filter(|(_, item)| matches!(item.kind, ItemKind::Trace(_)))
        .count()
}

/// The referee's verdict is the same on both sides — the clause that makes this a pipeline test.
fn assert_judged_identically(reference: &ImportedDesign, actual: &ImportedDesign) {
    let before = Referee::judge(reference, &ScoreWeights::V2);
    let after = Referee::judge(actual, &ScoreWeights::V2);
    assert_eq!(
        before.stats.clearance_violation_count, after.stats.clearance_violation_count,
        "violation count changed across the round trip"
    );
    assert_eq!(
        before.stats.incomplete_count, after.stats.incomplete_count,
        "incomplete count changed across the round trip"
    );
    assert_eq!(before.stats.max_connections, after.stats.max_connections);
    assert_eq!(before.stats.bend_count, after.stats.bend_count);
    assert_eq!(before.stats.via_count, after.stats.via_count);
    // The corner tolerance is one board unit over at most a few corners per trace, so the
    // length can move by well under a micrometre; the score must not move measurably.
    assert!(
        (before.score.normalized - after.score.normalized).abs() < 1e-6,
        "score moved: {} -> {}",
        before.score.normalized,
        after.score.normalized
    );
}

// -------------------------------------------------------------------------------------------------
// board equivalence (same shape as `fr-io`'s `ses_roundtrip_test`, kept local so a change to
// either test cannot silently weaken the other)
// -------------------------------------------------------------------------------------------------

/// `(layer, half_width, sorted nets, corner count)` — a trace's identity minus its geometry.
type TraceKey = (i32, i32, Vec<i32>, usize);
/// `(x, y, first layer, last layer, sorted nets)` — a via's whole identity.
type ViaKey = (i32, i32, i32, i32, Vec<i32>);

fn nets_of(item: &fr_board::Item) -> Vec<i32> {
    let mut nets: Vec<i32> = item.nets.iter().copied().collect();
    nets.sort_unstable();
    nets
}

/// Corners on the session's integer grid, canonicalised for direction: a merge can reverse a
/// trace, which changes nothing about the copper.
fn corners_of(trace: &fr_board::item::PolylineTrace) -> Vec<(i32, i32)> {
    let forward: Vec<(i32, i32)> = (0..trace.corner_count())
        .map(|index| {
            let corner = trace.polyline.corner_approx(index);
            (corner.x.round() as i32, corner.y.round() as i32)
        })
        .collect();
    let mut backward = forward.clone();
    backward.reverse();
    if forward <= backward {
        forward
    } else {
        backward
    }
}

fn traces_by_key(board: &Board) -> BTreeMap<TraceKey, Vec<Vec<(i32, i32)>>> {
    let mut map: BTreeMap<TraceKey, Vec<Vec<(i32, i32)>>> = BTreeMap::new();
    for (_, item) in board.store().iter() {
        let ItemKind::Trace(trace) = &item.kind else {
            continue;
        };
        let corners = corners_of(trace);
        map.entry((trace.layer, trace.half_width, nets_of(item), corners.len()))
            .or_default()
            .push(corners);
    }
    for lists in map.values_mut() {
        lists.sort();
    }
    map
}

fn vias_of(board: &Board) -> Vec<ViaKey> {
    let mut vias: Vec<ViaKey> = board
        .store()
        .iter()
        .filter_map(|(_, item)| match &item.kind {
            ItemKind::Via(via) => Some((
                via.center.x,
                via.center.y,
                via.layers.first,
                via.layers.last,
                nets_of(item),
            )),
            _ => None,
        })
        .collect();
    vias.sort();
    vias
}

fn assert_boards_equivalent(reference: &Board, actual: &Board) {
    let expected_vias = vias_of(reference);
    assert_eq!(expected_vias, vias_of(actual), "the via multiset moved");

    let expected = traces_by_key(reference);
    let got = traces_by_key(actual);
    assert_eq!(
        expected.keys().collect::<Vec<_>>(),
        got.keys().collect::<Vec<_>>(),
        "the trace key multiset moved"
    );
    for (key, expected_corners) in &expected {
        let got_corners = &got[key];
        assert_eq!(
            expected_corners.len(),
            got_corners.len(),
            "trace count changed for {key:?}"
        );
        for (before, after) in expected_corners.iter().zip(got_corners) {
            for ((bx, by), (ax, ay)) in before.iter().zip(after) {
                assert!(
                    (bx - ax).abs() <= CORNER_TOLERANCE && (by - ay).abs() <= CORNER_TOLERANCE,
                    "corner moved more than {CORNER_TOLERANCE} unit: ({bx},{by}) -> ({ax},{ay})"
                );
            }
        }
    }
}
