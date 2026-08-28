//! The `FRMETRIC settle` records must parse with `bench/tools/frlab_parse.py`.
//!
//! The parser's regex (`frlab_parse.py`, `RE_SETTLE`) is
//!
//! ```text
//! \bFR(?:LAB|METRIC)\s+settle\s+(-?\d+)\s+(-?\d+)\s+(\S+)\s+(\S+)
//! ```
//!
//! and `diff_settles` compares two sequences **by index**: a `(door_id,
//! section)` mismatch is a `topology` divergence, a `g`/`f` drift beyond the
//! tolerance is a `value` divergence, and a clean length difference is a
//! `length` divergence. This test pins the emitted format so that the
//! differential harness keeps working.
//!
//! The sink is resolved once per process from `FR_SETTLE_TRACE`, so this file
//! holds exactly one test and sets the variable before anything else runs.

use std::io::Read;

use fr_router::maze::search::MazeSearch;
use fr_router::test_support::TestFixture;

#[test]
fn emits_records_the_frlab_parser_accepts() {
    let path = std::env::temp_dir().join(format!(
        "fr-router-settle-{}-{}.log",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    // SAFETY-equivalent note: this is the only test in this binary, and the
    // sink is read lazily on the first `settle` call, which happens below.
    std::env::set_var("FR_SETTLE_TRACE", &path);

    let recorded = {
        let fixture = TestFixture::two_layer_with_pads();
        let mut engine = fixture.engine();
        let pads = fixture.pad_ids();
        let mut search =
            MazeSearch::get_instance(&mut engine, &pads[..1], &pads[1..2]).expect("instance");
        search.record_settles(true);
        search.find_connection().expect("routable");
        search.settles().to_vec()
    };
    fr_metrics::settle_trace::flush();

    let mut text = String::new();
    std::fs::File::open(&path)
        .expect("the trace file was created")
        .read_to_string(&mut text)
        .expect("readable");
    let _ = std::fs::remove_file(&path);

    let lines: Vec<&str> = text.lines().filter(|l| l.contains("settle")).collect();
    assert_eq!(
        lines.len(),
        recorded.len(),
        "one traced line per recorded settle"
    );
    assert!(!lines.is_empty());

    for (line, record) in lines.iter().zip(recorded.iter()) {
        let mut fields = line.split_whitespace();
        assert_eq!(fields.next(), Some("FRMETRIC"), "tool token: {line}");
        assert_eq!(fields.next(), Some("settle"), "record kind: {line}");
        let door_id: i64 = fields
            .next()
            .expect("door id")
            .parse()
            .expect("door id is an integer");
        let section: i64 = fields
            .next()
            .expect("section")
            .parse()
            .expect("section is an integer");
        let g: f64 = fields
            .next()
            .expect("g")
            .parse()
            .expect("g parses as a float");
        let f: f64 = fields
            .next()
            .expect("f")
            .parse()
            .expect("f parses as a float");
        assert_eq!(fields.next(), None, "no trailing fields: {line}");

        assert_eq!(door_id as u64, record.door_id);
        assert_eq!(section as u32, record.section);
        assert_eq!(g.to_bits(), record.g.to_bits(), "g must round-trip");
        assert_eq!(f.to_bits(), record.f.to_bits(), "f must round-trip");
    }
}
