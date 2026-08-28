//! Gate G1.2c: `.dsn` → board → `.ses` → board round trip on the pre-routed fixtures.
//!
//! # The shape of the check
//!
//! ```text
//! reference = import(fixture)                      the board the DSN describes
//! session   = write_session(import(fixture))       what the writer emits for it
//! actual    = read_session(session, import(fixture with (wiring …) emptied))
//! assert equivalent(reference, actual)
//! ```
//!
//! The target board is the *same DSN with its wiring scope emptied*, which is the real KiCad
//! workflow — export an unrouted board, route it, import the session — and it is the only way to
//! get an empty target: `Board::remove_item` refuses `USER_FIXED` items, which is what
//! `(type route)` copper imports as (§10.3).
//!
//! # What the session format cannot carry, and what that costs
//!
//! Two things, both spelled out in the spec and both asserted separately below:
//!
//! 1. **`shove_fixed`.** `SesWriter.writeFixedState` writes nothing at or below `SHOVE_FIXED`
//!    (§10.3), so `(type shove_fixed)` copper comes back `UNFIXED`. Since
//!    `Board::combine_trace` refuses to merge traces whose fixed states differ, erasing the
//!    distinction *unlocks merges the reference board did not make*. The reference is therefore
//!    built from the fixture with `shove_fixed` rewritten to `normal` — the exact information
//!    the format drops, and nothing else.
//! 2. **Integer coordinates.** Every SES coordinate is an integer (§12.2 rule 1) but a
//!    `Polyline` corner is the intersection of two `Line`s, i.e. a rational point. Writing it
//!    and rebuilding the polyline from the integers can move a corner by up to one board unit.
//!    [`assert_equivalent`] allows exactly that and no more.
//!
//! Everything else is exact: same trace count, same layer, same half width, same net set, same
//! corner count, and the via multiset matches with no tolerance at all.
//!
//! Needs `bench/fetch_fixtures.sh`; fails loudly rather than skipping (gate G1.3).

use std::collections::BTreeMap;

use fr_board::{Board, FixedState, ItemKind};
use fr_io::specctra::{
    import,
    ses_read::{read_session, read_session_with_options, SesCompat, SesReadOptions, SesTarget},
    ses_write::{write_session, SesInput},
    ImportedDesign, SesLibrary,
};

const FIXTURE_DIR: &str = "/tmp/freerouting-upstream/fixtures";

/// A board unit, the tolerance [`assert_equivalent`] allows on a trace corner. See the module
/// docs, point 2.
const CORNER_TOLERANCE: i32 = 1;

fn read_fixture(name: &str) -> Vec<u8> {
    let path = format!("{FIXTURE_DIR}/{name}");
    std::fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "fixture '{path}' is missing ({error}); run bench/fetch_fixtures.sh to populate \
             {FIXTURE_DIR}"
        )
    })
}

/// Replaces the `(wiring …)` scope with an empty one, giving the unrouted board the session file
/// is applied to.
///
/// The scan starts at the *last* `(wiring` in the file — the scope is top level and cannot
/// nest — and then counts brackets outside quoted strings. Starting there is what keeps the
/// `(string_quote ")` directive in the parser scope, whose lone `"` would otherwise flip the
/// quote state for the rest of the file, out of the way.
fn strip_wiring(src: &[u8]) -> Vec<u8> {
    let needle = b"(wiring";
    let start = (0..src.len())
        .rev()
        .find(|&index| src[index..].starts_with(needle))
        .expect("the fixture must have a wiring scope");
    let mut depth = 0i32;
    let mut in_quote = false;
    let mut end = start;
    while end < src.len() {
        match src[end] {
            b'"' => in_quote = !in_quote,
            b'(' if !in_quote => depth += 1,
            b')' if !in_quote => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        end += 1;
    }
    let mut out = Vec::with_capacity(src.len());
    out.extend_from_slice(&src[..start]);
    out.extend_from_slice(b"(wiring)");
    out.extend_from_slice(&src[end + 1..]);
    out
}

/// Rewrites `(type shove_fixed)` to `(type normal)`, keeping the byte length so nothing else in
/// the file shifts. See the module docs, point 1.
fn downgrade_shove_fixed(src: &[u8]) -> Vec<u8> {
    String::from_utf8_lossy(src)
        .replace("shove_fixed", "normal     ")
        .into_bytes()
}

/// `(layer, half_width, sorted nets, corner count)` — the part of a trace's identity that the
/// round trip must reproduce exactly.
type TraceKey = (i32, i32, Vec<i32>, usize);
/// `(x, y, first layer, last layer, sorted nets)` — a via's whole identity.
type ViaKey = (i32, i32, i32, i32, Vec<i32>);

fn nets_of(item: &fr_board::Item) -> Vec<i32> {
    let mut nets: Vec<i32> = item.nets.iter().copied().collect();
    nets.sort_unstable();
    nets
}

/// Corners rounded to the session's integer grid, canonicalised for direction: a merge can
/// reverse a trace, which changes nothing about the copper.
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

fn vias_by_key(board: &Board) -> BTreeMap<ViaKey, usize> {
    let mut map = BTreeMap::new();
    for (_, item) in board.store().iter() {
        let ItemKind::Via(via) = &item.kind else {
            continue;
        };
        *map.entry((
            via.center.x,
            via.center.y,
            via.layers.first,
            via.layers.last,
            nets_of(item),
        ))
        .or_insert(0) += 1;
    }
    map
}

/// Asserts the two boards carry the same routing: the via multiset exactly, and every trace with
/// the same layer, half width, net set and corner count, each corner within
/// [`CORNER_TOLERANCE`].
fn assert_equivalent(reference: &Board, actual: &Board, context: &str) {
    assert_eq!(
        vias_by_key(reference),
        vias_by_key(actual),
        "{context}: the via multiset changed"
    );

    let expected = traces_by_key(reference);
    let mut found = traces_by_key(actual);
    for (key, reference_corners) in &expected {
        let actual_corners = found.get_mut(key).unwrap_or_else(|| {
            panic!("{context}: no trace at all for {key:?}");
        });
        assert_eq!(
            reference_corners.len(),
            actual_corners.len(),
            "{context}: trace count differs for {key:?}"
        );
        // Same key, same count: pair them up. Both lists are sorted, so the obvious pairing is
        // the right one whenever the corner deviations are below the tolerance — and if it is
        // not, some corner moved further than a board unit, which is the failure being hunted.
        for (wanted, got) in reference_corners.iter().zip(actual_corners.iter()) {
            let deviation = wanted
                .iter()
                .zip(got.iter())
                .map(|(a, b)| (a.0 - b.0).abs().max((a.1 - b.1).abs()))
                .max()
                .unwrap_or(0);
            assert!(
                deviation <= CORNER_TOLERANCE,
                "{context}: a trace corner moved {deviation} board units (limit \
                 {CORNER_TOLERANCE}) for {key:?}\n  reference {wanted:?}\n  actual    {got:?}"
            );
        }
        found.remove(key);
    }
    assert!(
        found.is_empty(),
        "{context}: the round trip invented traces: {:?}",
        found.keys().collect::<Vec<_>>()
    );
}

fn count_fixed(design: &ImportedDesign, fixed: FixedState) -> usize {
    design
        .board
        .store()
        .iter()
        .filter(|(_, item)| {
            item.fixed_state == fixed && matches!(item.kind, ItemKind::Trace(_) | ItemKind::Via(_))
        })
        .count()
}

/// The whole G1.2c pipeline for one fixture.
fn round_trip(fixture: &str) {
    let bytes = read_fixture(fixture);
    let design = import(&bytes).unwrap_or_else(|error| panic!("{fixture} should import: {error}"));

    let session = write_session(&SesInput::from_design(&design, fixture));
    // Whatever the fixture holds, a session file is a session file.
    assert!(session.starts_with("(session "));
    assert!(session.ends_with("\n)"));

    let reference = import(&downgrade_shove_fixed(&bytes))
        .unwrap_or_else(|error| panic!("{fixture} without shove_fixed should import: {error}"));

    let mut actual = import(&strip_wiring(&bytes))
        .unwrap_or_else(|error| panic!("{fixture} without wiring should import: {error}"));
    assert_eq!(
        traces_by_key(&actual.board).len() + vias_by_key(&actual.board).len(),
        0,
        "{fixture}: the target board should start with no routing"
    );

    let library = SesLibrary::from_design(&actual);
    let summary = read_session(
        session.as_bytes(),
        SesTarget::for_design(&mut actual, &library),
    )
    .unwrap_or_else(|error| panic!("{fixture}: the written session should read back: {error}"));
    assert_eq!(
        summary.errors_encountered, 0,
        "{fixture}: {:?}",
        summary.warnings
    );

    assert_equivalent(&reference.board, &actual.board, fixture);

    // The counters are upstream's `SesImportSummary`; they must agree with what landed.
    assert_eq!(
        summary.vias_imported,
        vias_by_key(&actual.board).values().sum::<usize>(),
        "{fixture}: the via counter disagrees with the board"
    );
    assert!(
        summary.wires_imported
            >= traces_by_key(&actual.board)
                .values()
                .map(Vec::len)
                .sum::<usize>(),
        "{fixture}: fewer wires were read than ended up on the board"
    );
}

#[test]
fn issue026_an_unrouted_board_round_trips_to_an_empty_session() {
    // The degenerate case, and the one the format conformance test leans on: an empty
    // `(wiring …)` produces an empty `network_out`, and reading it back changes nothing.
    round_trip("Issue026-J2_reference.dsn");

    let bytes = read_fixture("Issue026-J2_reference.dsn");
    let design = import(&bytes).unwrap();
    let session = write_session(&SesInput::from_design(&design, "Issue026-J2_reference.dsn"));
    assert!(!session.contains("(net "), "got: {session}");
}

#[test]
fn issue190_user_fixed_routing_round_trips_item_for_item() {
    // Every wire in this fixture is `(type route)`, i.e. `USER_FIXED` (§10.3), which the session
    // format *can* express — `(type protect)`. Nothing merges, because
    // `combine_trace` refuses `USER_FIXED` traces outright, so this is the strictest case
    // available: 5981 wires and 223 vias have to come back one for one.
    let bytes = read_fixture("Issue190-processor.Z80.dsn");
    let design = import(&bytes).unwrap();
    assert_eq!(
        count_fixed(&design, FixedState::ShoveFixed),
        0,
        "Issue190 is supposed to be the no-shove_fixed case"
    );
    assert!(count_fixed(&design, FixedState::UserFixed) > 1000);

    round_trip("Issue190-processor.Z80.dsn");
}

#[test]
fn issue103_routed_round_trips_across_the_shove_fixed_erasure() {
    // This fixture mixes `shove_fixed` and unfixed copper, so it is the case where the format's
    // one real information loss bites. See the module docs.
    let bytes = read_fixture("Issue103-Board-Routed.dsn");
    let design = import(&bytes).unwrap();
    assert!(
        count_fixed(&design, FixedState::ShoveFixed) > 0,
        "Issue103-Board-Routed is supposed to be the shove_fixed case"
    );
    let session = write_session(&SesInput::from_design(&design, "Issue103-Board-Routed.dsn"));
    assert!(
        !session.contains("(type "),
        "no wire in this fixture is user-fixed, so no (type …) should be written"
    );

    round_trip("Issue103-Board-Routed.dsn");
}

#[test]
fn a_session_written_by_the_java_jar_reads_back() {
    // The reader has to cope with files this port did not write. The companion session file is
    // v1.9-era output for the same fixture: 93 `(wire …)` records, 10 vias, and a `library_out`
    // that declares the same padstack twice (the duplicate v2's `LinkedHashSet` removed).
    //
    // Only 89 of the 93 wires import: four carry a single-corner `(path …)`, which
    // `Shape.read_polygon_path_scope` refuses ("Single-point paths are not valid traces") and
    // `SesReader.processWireScope` then treats as a conduction area — silently, so the error
    // counter stays at zero. That is upstream's behaviour, verified against `Shape.java`.
    let bytes = read_fixture("Issue026-J2_reference.dsn");
    let session = read_fixture("Issue026-J2_reference.ses");
    let mut design = import(&bytes).unwrap();
    let library = SesLibrary::from_design(&design);
    let summary = read_session(&session, SesTarget::for_design(&mut design, &library))
        .expect("the jar's own session file should read");
    assert_eq!(summary.errors_encountered, 0, "{:?}", summary.warnings);
    assert_eq!(summary.wires_imported, 89);
    assert_eq!(summary.vias_imported, 10);
    assert_eq!(vias_by_key(&design.board).values().sum::<usize>(), 10);
}

#[test]
fn upstream_compat_mode_reproduces_the_two_documented_divergences() {
    // §12.5: `SesReader` marks everything `USER_FIXED` and takes the clearance classes from the
    // *default* net class rather than the item's own. Both are wrong for a round trip and right
    // for reproducing the jar, hence the mode flag.
    let bytes = read_fixture("Issue026-J2_reference.dsn");
    let session = read_fixture("Issue026-J2_reference.ses");

    let mut upstream = import(&bytes).unwrap();
    let library = SesLibrary::from_design(&upstream);
    read_session_with_options(
        &session,
        SesTarget::for_design(&mut upstream, &library),
        SesReadOptions {
            compat: SesCompat::Upstream,
        },
    )
    .unwrap();
    let user_fixed = upstream
        .board
        .store()
        .iter()
        .filter(|(_, item)| {
            matches!(item.kind, ItemKind::Trace(_) | ItemKind::Via(_))
                && item.fixed_state == FixedState::UserFixed
        })
        .count();
    assert_eq!(
        user_fixed, 99,
        "upstream marks every re-imported wire and via USER_FIXED (89 wires + 10 vias)"
    );

    // Lossless mode reads the fixed state back out of `(type …)`. The jar's file carries none,
    // so nothing lands USER_FIXED — which is exactly what `SesWriter.writeFixedState` means by
    // writing nothing for anything at or below `SHOVE_FIXED`.
    //
    // The item *count* is lower than upstream mode's 99, and that is the same effect the
    // module docs describe: unfixed traces are combinable, so `normalize_traces` merges the
    // chain the session file split at every corner. Upstream mode's blanket `USER_FIXED` is
    // what stops it there.
    let mut lossless = import(&bytes).unwrap();
    let library = SesLibrary::from_design(&lossless);
    read_session(&session, SesTarget::for_design(&mut lossless, &library)).unwrap();
    let mut unfixed = 0usize;
    let mut fixed_somehow = 0usize;
    for (_, item) in lossless.board.store().iter() {
        if !matches!(item.kind, ItemKind::Trace(_) | ItemKind::Via(_)) {
            continue;
        }
        if item.fixed_state == FixedState::Unfixed {
            unfixed += 1;
        } else {
            fixed_somehow += 1;
        }
    }
    assert_eq!(
        fixed_somehow, 0,
        "lossless mode should not invent a fixed state the file does not carry"
    );
    assert!(unfixed > 0 && unfixed <= 99, "got {unfixed}");
}

#[test]
fn a_protect_type_survives_the_round_trip_in_lossless_mode_only() {
    // The one fixed state the format *can* carry, in both directions.
    let dsn = br#"(pcb rt.dsn
  (parser (string_quote ") (space_in_quoted_tokens on))
  (resolution um 10)
  (structure
    (layer F.Cu (type signal))
    (layer B.Cu (type signal))
    (boundary (rect pcb 0 -2000 4000 0))
    (rule (width 200) (clearance 200))
  )
  (placement)
  (library)
  (network (net N (pins )))
  (wiring
    (wire (path F.Cu 200 1500 -1500 2500 -1500) (net N) (type protect))
  )
)"#;
    let design = import(dsn).unwrap();
    let session = write_session(&SesInput::from_design(&design, "rt.dsn"));
    assert!(session.contains("(type protect)"));

    let empty = String::from_utf8_lossy(dsn).replace(
        "(wire (path F.Cu 200 1500 -1500 2500 -1500) (net N) (type protect))",
        "",
    );
    let mut target = import(empty.as_bytes()).unwrap();
    let library = SesLibrary::from_design(&target);
    read_session(
        session.as_bytes(),
        SesTarget::for_design(&mut target, &library),
    )
    .unwrap();
    let states: Vec<FixedState> = target
        .board
        .store()
        .iter()
        .filter(|(_, item)| matches!(item.kind, ItemKind::Trace(_)))
        .map(|(_, item)| item.fixed_state)
        .collect();
    assert_eq!(states, vec![FixedState::UserFixed]);
}
