//! SES writer conformance: the exact bytes KiCad's `SPECCTRA_DB::LoadSESSION` is handed.
//!
//! Every expectation here is derived from `docs/upstream-study/dsn-board-semantics.md` §12 and
//! cross-checked against a session file the released 2.3.0 jar produced
//! (`bench/results/20260828-013025/ses/`), never read off a run of this code. The derivation is
//! spelled out above each assertion so a disagreement can be adjudicated against the spec.
//!
//! The fixture-driven cases need `bench/fetch_fixtures.sh`; they fail loudly rather than
//! skipping (gate G1.3).

use fr_io::specctra::{
    import,
    ses_write::{write_session, write_session_verbose, SesInput},
};

/// The 2.3.0 jar's own session files, from the bench run that produced
/// `bench/results/20260828-013025/`. Integration tests run with the crate directory as their
/// working directory, hence the climb to the workspace root.
const JAVA_SES_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../bench/results/20260828-013025/ses"
);

fn fixture_dir() -> std::path::PathBuf {
    std::env::var_os("FR_FIXTURE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/freerouting-upstream/fixtures"))
}

fn read_fixture(name: &str) -> Vec<u8> {
    let path = fixture_dir().join(name);
    std::fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "fixture '{}' is missing ({error}); run bench/fetch_fixtures.sh or set FR_FIXTURE_DIR",
            path.display()
        )
    })
}

/// The golden design: two layers, one back-side component at a fractional rotation, a padstack
/// that forbids attachment, a net whose name needs the v2-only `/` quoting, one `(type protect)`
/// wire and one via.
///
/// `(resolution um 10)` with a boundary of `(0,-2000)-(4000,0)`: §3.2 gives
/// `maxCoor = 4000 * 10 = 40000` and `5 * 40000 < 2^25`, so the board scale factor stays at the
/// resolution, 10. §3.5's session factor is therefore `10 / 10 = 1` and **a SES coordinate is a
/// board coordinate**, i.e. ten times the DSN number.
const GOLDEN_DSN: &str = r#"(pcb golden.dsn
  (parser
    (string_quote ")
    (space_in_quoted_tokens on)
    (host_cad "KiCad's Pcbnew")
    (host_version "8.0.1")
  )
  (resolution um 10)
  (structure
    (layer F.Cu (type signal))
    (layer B.Cu (type signal))
    (boundary (rect pcb 0 -2000 4000 0))
    (via "Via[0-1]_800:400_um")
    (rule (width 200) (clearance 200))
  )
  (placement
    (component MYPKG
      (place U1 1000 -1000 back 338.5 (lock_type position))
    )
  )
  (library
    (image MYPKG
      (pin RoundPad600 1 0 0)
      (pin RoundPad600 2 500 0)
    )
    (padstack RoundPad600
      (shape (circle F.Cu 600))
      (shape (circle B.Cu 600))
      (attach off)
    )
    (padstack "Via[0-1]_800:400_um"
      (shape (circle F.Cu 800))
      (shape (circle B.Cu 800))
      (attach off)
    )
  )
  (network
    (net /CLK
      (pins U1-1 U1-2)
    )
    (class kicad_default /CLK
      (circuit (use_via "Via[0-1]_800:400_um"))
      (rule (width 200) (clearance 200))
    )
  )
  (wiring
    (wire (path F.Cu 200 1500 -1500 2500 -1500) (net /CLK) (type protect))
    (via "Via[0-1]_800:400_um" 2500 -1500 (net /CLK))
  )
)
"#;

/// Hand-derived from §12.1 (the scope tree), §12.2 (integers, layer names, `library_out` dedup,
/// `(attach off)`, the reduced `parser` scope, lazy `(net …)` headers), §12.4 (rotation) and
/// `IndentFileWriter`'s two-space indent.
///
/// The details that are easy to get wrong, each visible below:
///
/// * `(routes `, `(library_out ` and `(network_out ` carry a **trailing space** — upstream writes
///   the keyword with one and the following `start_scope()` emits the newline. It is spelled
///   `\x20` below so it survives `rustfmt` and a trailing-whitespace stripper.
/// * `(placement` and `(was_is` do **not**.
/// * The file ends `\n)` with no trailing newline.
/// * `/CLK`, `Via[0-1]_800:400_um` and `8.0.1` are quoted — the first two because `/` and `-`
///   are in the SES reserved set, the third because `IdentifierType` also quotes anything
///   matching `^-?\d.*`. `F.Cu`, `U1`, `MYPKG`, `golden.ses` and `golden.dsn` are not: `.` is
///   not reserved.
/// * The rotation is `338.5`, not `338.500` (§12.4) and not `339` (which is what v1.9 wrote).
///   `(place … back …)` carries the DSN's own rotation; the back-side flip is applied to the
///   pad geometry (§9.2), not to this number.
/// * `(type protect)` sits inside the `(wire …)` scope, after the `(path …)` scope closes, and
///   a `(via …)` scope closes on its own line even when it has no `(type …)` — both confirmed
///   against the jar's output for Issue026.
const GOLDEN_SES: &str = "(session golden.ses
  (base_design golden.dsn)
  (placement
    (resolution um 10)
    (component MYPKG
      (place U1 10000 -10000 back 338.5
       (lock_type position))
    )
  )
  (was_is
  )
  (routes\x20
    (resolution um 10)
    (parser
      (host_cad \"KiCad's Pcbnew\")
      (host_version \"8.0.1\")
    )
    (library_out\x20
      (padstack \"Via[0-1]_800:400_um\"
        (shape
          (circle F.Cu 8000 0 0)
        )
        (shape
          (circle B.Cu 8000 0 0)
        )
        (attach off)
      )
    )
    (network_out\x20
      (net \"/CLK\"
        (via \"Via[0-1]_800:400_um\" 25000 -15000
        )
        (wire
          (path F.Cu 2000
            15000 -15000
            25000 -15000
          )
          (type protect)
        )
      )
    )
  )
)";

#[test]
fn the_golden_snippet_is_written_byte_for_byte() {
    let design = import(GOLDEN_DSN.as_bytes()).expect("the golden design should import");
    let output = write_session_verbose(&SesInput::from_design(&design, "golden.dsn"));
    assert!(
        output.warnings.is_empty(),
        "unexpected warnings: {:?}",
        output.warnings
    );
    assert_eq!(output.text, GOLDEN_SES);
}

#[test]
fn the_session_file_ends_without_a_trailing_newline() {
    // `IndentFileWriter.end_scope` writes `\n` + indent + `)`, and `SesWriter.write` flushes
    // without adding anything. A trailing newline would be a diff against every golden file.
    let design = import(GOLDEN_DSN.as_bytes()).unwrap();
    let text = write_session(&SesInput::from_design(&design, "golden.dsn"));
    assert!(text.ends_with("\n)"));
    assert!(!text.ends_with("\n)\n"));
    assert!(text.starts_with("(session "));
}

#[test]
fn the_session_name_is_the_design_name_with_dsn_replaced_by_ses() {
    // `designName.replace(".dsn", ".ses")` is a plain substring replace, so every occurrence
    // goes — `my.dsn.board.dsn` becomes `my.ses.board.ses` (§12).
    let design = import(GOLDEN_DSN.as_bytes()).unwrap();
    let text = write_session(&SesInput::from_design(&design, "my.dsn.board.dsn"));
    assert!(
        text.starts_with("(session my.ses.board.ses\n  (base_design my.dsn.board.dsn)"),
        "got: {}",
        &text[..text.len().min(90)]
    );
}

#[test]
fn issue026_matches_the_java_writer_up_to_the_routing() {
    // The strongest conformance evidence available: the released jar's own session file for a
    // fixture whose `(wiring …)` scope is empty. Everything before `network_out` — the header,
    // the placement scope, `was_is`, the reduced parser scope and `library_out` — is produced
    // from the DSN alone and must match byte for byte. What follows is the jar's routing
    // result, which this test has nothing to say about.
    let bytes = read_fixture("Issue026-J2_reference.dsn");
    let design = import(&bytes).expect("Issue026 should import");
    // The bench run passed the job name, which is what lands in `(base_design …)`.
    let ours = write_session(&SesInput::from_design(
        &design,
        "Issue026-J2_reference.java",
    ));
    let java_path = format!("{JAVA_SES_DIR}/Issue026-J2_reference.java.ses");
    let theirs = std::fs::read_to_string(&java_path)
        .unwrap_or_else(|error| panic!("golden '{java_path}' is missing ({error})"));

    let prefix_end = theirs
        .find("(network_out ")
        .expect("the Java session must have a network_out scope")
        + "(network_out ".len();
    assert_eq!(
        ours[..prefix_end.min(ours.len())],
        theirs[..prefix_end],
        "the writer diverged from the 2.3.0 jar before network_out"
    );

    // The fixture routes nothing, so our own `network_out` closes immediately: three nested
    // `)` for network_out, routes and session.
    assert!(
        ours.ends_with("(network_out \n    )\n  )\n)"),
        "unrouted board should emit an empty network_out; tail was: {:?}",
        &ours[ours.len().saturating_sub(40)..]
    );
}

#[test]
fn via_padstacks_are_declared_once_even_when_the_dsn_names_them_twice() {
    // §12.2 rule 3, v2-only: `writeLibrary` deduplicates with a `LinkedHashSet`. The companion
    // v1.9-era session file for this very fixture declares `Via[0-1]_800:400_um` twice, which
    // is the bug the set fixed.
    let bytes = read_fixture("Issue026-J2_reference.dsn");
    let design = import(&bytes).unwrap();
    let text = write_session(&SesInput::from_design(&design, "Issue026-J2_reference.dsn"));
    assert_eq!(text.matches("(padstack \"Via[0-1]_800:400_um\"").count(), 1);

    let v19 = std::fs::read_to_string(fixture_dir().join("Issue026-J2_reference.ses"))
        .expect("the companion v1.9 session file is part of the fixture set");
    assert_eq!(
        v19.matches("(padstack \"Via[0-1]_800:400_um\"").count(),
        2,
        "the companion file is supposed to be the duplicated-padstack case"
    );
}

#[test]
fn system_fixed_copper_is_omitted() {
    // §10.3 / §12.2 rule 6: `(type fix)` wiring imports as `SYSTEM_FIXED`, and `writeNet` skips
    // every `SYSTEM_FIXED` item — which is how DSN-imported keepouts and locked component
    // copper stay out of the session. With the only wire fixed, the net header is never
    // written either, because it is emitted lazily on the first eligible item.
    let dsn = GOLDEN_DSN.replace("(type protect)", "(type fix)");
    let design = import(dsn.as_bytes()).unwrap();
    let text = write_session(&SesInput::from_design(&design, "golden.dsn"));
    assert!(!text.contains("(path F.Cu"), "got: {text}");
    assert!(!text.contains("(type fix)"), "got: {text}");
    // The via is still `UNFIXED`, so the net header *is* written — for the via alone.
    assert!(text.contains("(net \"/CLK\"\n        (via "), "got: {text}");
}

#[test]
fn shove_fixed_copper_loses_its_type_but_keeps_its_geometry() {
    // §10.3: `SesWriter.writeFixedState` writes nothing at or below `SHOVE_FIXED`, unlike
    // `Wiring.writeFixedState`, which emits `(type shove_fixed)`. The session format simply
    // cannot express it. This is the one thing a SES round trip is allowed to lose.
    let dsn = GOLDEN_DSN.replace("(type protect)", "(type shove_fixed)");
    let design = import(dsn.as_bytes()).unwrap();
    let text = write_session(&SesInput::from_design(&design, "golden.dsn"));
    assert!(text.contains("(path F.Cu 2000\n            15000 -15000"));
    assert!(!text.contains("(type "), "got: {text}");
}

#[test]
fn unfixed_copper_carries_no_type_scope() {
    let dsn = GOLDEN_DSN.replace("(type protect)", "(type normal)");
    let design = import(dsn.as_bytes()).unwrap();
    let text = write_session(&SesInput::from_design(&design, "golden.dsn"));
    assert!(text.contains("(path F.Cu 2000"));
    assert!(!text.contains("(type "), "got: {text}");
}

#[test]
fn a_net_with_no_routing_gets_no_scope_at_all() {
    // §12.2 rule 6: the `(net <name>` header is written lazily, so a net whose items are all
    // pins produces nothing.
    let dsn = GOLDEN_DSN
        .replace(
            "(wire (path F.Cu 200 1500 -1500 2500 -1500) (net /CLK) (type protect))",
            "",
        )
        .replace("(via \"Via[0-1]_800:400_um\" 2500 -1500 (net /CLK))", "");
    let design = import(dsn.as_bytes()).unwrap();
    let text = write_session(&SesInput::from_design(&design, "golden.dsn"));
    assert!(!text.contains("(net "), "got: {text}");
    assert!(text.contains("(network_out \n    )"), "got: {text}");
}

#[test]
fn every_reserved_character_forces_quotes_in_a_net_name() {
    // §12: the SES reserved set is `( ) SPACE ; - _ / ~ { }`, four characters wider than the
    // DSN writer's. `~{RESET}` is the KiCad spelling that made v2 add `~ { }`.
    for (net_name, expected) in [
        ("~{RESET}", "\"~{RESET}\""),
        ("Sheet1/CLK", "\"Sheet1/CLK\""),
        ("VCC_3V3", "\"VCC_3V3\""),
        ("A-B", "\"A-B\""),
        ("PLAIN", "PLAIN"),
    ] {
        let dsn = GOLDEN_DSN.replace("/CLK", net_name);
        let design = import(dsn.as_bytes())
            .unwrap_or_else(|error| panic!("net '{net_name}' should import: {error}"));
        let text = write_session(&SesInput::from_design(&design, "golden.dsn"));
        assert!(
            text.contains(&format!("(net {expected}\n")),
            "net '{net_name}' should be written as {expected}; got:\n{text}"
        );
    }
}

#[test]
fn a_fractional_rotation_keeps_its_decimals_and_a_whole_one_does_not() {
    // §12.4, v2-only. v1.9 wrote `(int) Math.round(rotation)` and lost the fraction entirely.
    let whole = GOLDEN_DSN.replace("back 338.5", "back 270");
    let design = import(whole.as_bytes()).unwrap();
    let text = write_session(&SesInput::from_design(&design, "golden.dsn"));
    assert!(text.contains(" back 270\n"), "got: {text}");

    // Three decimals survive; a fourth is rounded away by `rint(deg * 1000) / 1000`.
    let fraction = GOLDEN_DSN.replace("back 338.5", "back 338.2505");
    let design = import(fraction.as_bytes()).unwrap();
    let text = write_session(&SesInput::from_design(&design, "golden.dsn"));
    assert!(text.contains(" back 338.25\n"), "got: {text}");
}

/// One pin on net `N` at DSN `(1000, -1000)` — board `(10000, -10000)` at a scale factor of 10 —
/// with a `600 µm` round pad, and one wire on that net. `<<ENDPOINT>>` is the wire's first
/// coordinate pair.
const SNAP_DSN: &str = r#"(pcb snap.dsn
  (parser (string_quote ") (space_in_quoted_tokens on))
  (resolution um 10)
  (structure
    (layer F.Cu (type signal))
    (layer B.Cu (type signal))
    (boundary (rect pcb 0 -2000 4000 0))
    (rule (width 200) (clearance 200))
  )
  (placement
    (component SNAPPKG
      (place U1 1000 -1000 front 0)
    )
  )
  (library
    (image SNAPPKG
      (pin RoundPad600 1 0 0)
    )
    (padstack RoundPad600
      (shape (circle F.Cu 600))
      (shape (circle B.Cu 600))
    )
  )
  (network
    (net N
      (pins U1-1)
    )
    (class kicad_default N
      (rule (width 200) (clearance 200))
    )
  )
  (wiring
    (wire (path F.Cu 200 <<ENDPOINT>> 1500 -1000) (net N))
  )
)
"#;

fn snap_session(endpoint: &str) -> String {
    let dsn = SNAP_DSN.replace("<<ENDPOINT>>", endpoint);
    let design = import(dsn.as_bytes()).expect("the snapping design should import");
    write_session(&SesInput::from_design(&design, "snap.dsn"))
}

#[test]
fn an_endpoint_already_on_the_pad_centre_is_left_alone() {
    // §12.3's first guard: `centerDistance <= 0.5` returns null, because there is nothing to
    // fix and returning the centre would only feed the duplicate-corner elision.
    let text = snap_session("1000 -1000");
    assert!(
        text.contains("(path F.Cu 2000\n            10000 -10000\n            15000 -10000\n"),
        "got: {text}"
    );
}

#[test]
fn an_endpoint_inside_the_pad_is_not_snapped_because_it_is_not_a_contact() {
    // §12.3 says `snappedEndpoint` snaps a trace end that lies inside the pad's inradius onto
    // the pad centre, and that failing to do so makes KiCad report spurious unconnecteds.
    //
    // In the shipped 2.3.0 code it can never fire, and this test pins that down. The candidate
    // set comes from `Trace.get_start_contacts()` = `get_normal_contacts(first_corner(), false)`
    // (`Trace.java:101-103`), whose drill-item branch is
    // `p_point.equals(curr_drill_item.get_center())` (`Trace.java:189`) — an *exact* match. So
    // a drill item is only ever a contact when the endpoint already is its centre, and then
    // `centerDistance == 0 <= 0.5` returns null one line later. An endpoint that is inside the
    // pad but off-centre — the case the fix is for — has no drill contact at all, so the loop
    // never sees it.
    //
    // The endpoint below is 100 board units from the centre of a pad whose inradius is 3000,
    // i.e. squarely inside the window §12.3 describes. It is written out unchanged.
    let text = snap_session("1010 -1000");
    assert!(
        text.contains("(path F.Cu 2000\n            10100 -10000\n            15000 -10000\n"),
        "got: {text}"
    );
}
