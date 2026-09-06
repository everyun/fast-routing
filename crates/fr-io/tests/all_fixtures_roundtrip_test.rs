//! Tests SES write -> read round-trip property across all 55 test fixtures (46 upstream + 9 OOS).

use std::collections::BTreeMap;
use std::path::PathBuf;

use fr_board::{Board, FixedState, ItemKind};
use fr_io::specctra::{
    import,
    ses_read::{read_session, SesTarget},
    ses_write::{write_session, SesInput},
    strip_session_copper, SesLibrary,
};

const CORNER_TOLERANCE: i32 = 1;

fn find_fixture_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(env_dir) = std::env::var_os("FR_FIXTURE_DIR") {
        dirs.push(PathBuf::from(env_dir));
    }
    dirs.push(PathBuf::from("/tmp/freerouting-upstream/fixtures"));
    if let Some(home) = std::env::var_os("HOME") {
        let home_path = PathBuf::from(home);
        dirs.push(home_path.join("fr-ref/freerouting-upstream/fixtures"));
    }
    dirs.push(PathBuf::from("bench/fixtures-oos"));
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dirs.push(manifest_dir.join("../../bench/fixtures-oos"));
    dirs.push(manifest_dir.join("../../../bench/fixtures-oos"));
    dirs
}

fn collect_all_fixtures() -> Vec<(String, PathBuf)> {
    let mut fixtures = BTreeMap::new();
    for dir in find_fixture_dirs() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("dsn") {
                    let name = path.file_name().unwrap().to_string_lossy().to_string();
                    fixtures.entry(name).or_insert(path);
                }
            }
        }
    }
    fixtures.into_iter().collect()
}

fn downgrade_shove_fixed(src: &[u8]) -> Vec<u8> {
    String::from_utf8_lossy(src)
        .replace("shove_fixed", "normal     ")
        .into_bytes()
}

type TraceKey = (i32, i32, Vec<i32>, usize);
type ViaKey = (i32, i32, i32, i32, Vec<i32>);

fn nets_of(item: &fr_board::Item) -> Vec<i32> {
    let mut nets: Vec<i32> = item.nets.iter().copied().collect();
    nets.sort_unstable();
    nets
}

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

#[test]
fn test_all_55_fixtures_round_trip() {
    let fixtures = collect_all_fixtures();
    println!(
        "Found {} fixture files for round-trip testing",
        fixtures.len()
    );

    let mut tested_count = 0usize;
    let mut non_dsn_count = 0usize;

    for (name, path) in fixtures {
        let bytes = std::fs::read(&path).expect("read fixture file");

        let design_result = import(&bytes);
        if name == "Issue006-LPC18XX_43XX_SCH.dsn" {
            // Known non-DSN fixture (schematic)
            assert!(
                design_result.is_err(),
                "Issue006 should be cleanly rejected as non-DSN"
            );
            non_dsn_count += 1;
            continue;
        }

        let design = match design_result {
            Ok(d) => d,
            Err(e) => {
                panic!("Fixture '{name}' failed to import: {e}");
            }
        };

        let session = write_session(&SesInput::from_design(&design, &name));
        assert!(
            session.starts_with("(session "),
            "{name}: invalid session header"
        );
        assert!(session.ends_with("\n)"), "{name}: invalid session footer");

        let reference = import(&downgrade_shove_fixed(&bytes))
            .unwrap_or_else(|error| panic!("{name} without shove_fixed should import: {error}"));

        let mut actual = import(&strip_session_copper(&bytes))
            .unwrap_or_else(|error| panic!("{name} without wiring should import: {error}"));

        let carried_over_vias = vias_by_key(&actual.board).values().sum::<usize>();
        let survivors: Vec<FixedState> = actual
            .board
            .store()
            .iter()
            .filter(|(_, item)| matches!(item.kind, ItemKind::Trace(_) | ItemKind::Via(_)))
            .map(|(_, item)| item.fixed_state)
            .collect();
        let carried_over_traces = survivors.len() - carried_over_vias;

        let library = SesLibrary::from_design(&actual);
        let summary = read_session(
            session.as_bytes(),
            SesTarget::for_design(&mut actual, &library),
        )
        .unwrap_or_else(|error| panic!("{name}: the written session should read back: {error}"));

        assert_eq!(
            summary.errors_encountered, 0,
            "{name}: errors during ses re-import: {:?}",
            summary.warnings
        );

        assert_equivalent(&reference.board, &actual.board, &name);

        assert_eq!(
            summary.vias_imported + carried_over_vias,
            vias_by_key(&actual.board).values().sum::<usize>(),
            "{name}: the via counter disagrees with the board"
        );
        assert!(
            summary.wires_imported + carried_over_traces
                >= traces_by_key(&actual.board)
                    .values()
                    .map(Vec::len)
                    .sum::<usize>(),
            "{name}: fewer wires were read than ended up on the board"
        );

        tested_count += 1;
    }

    println!("All fixtures round-trip test passed on {tested_count} boards ({non_dsn_count} non-DSN correctly rejected).");
    assert!(
        tested_count >= 54,
        "Expected at least 54 valid boards tested out of 55 fixtures"
    );
}
