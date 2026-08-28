//! `fr-cli` end to end: the four modes, the four exit codes, and the flags that must keep
//! parsing.
//!
//! Hermetic on purpose. Every case runs against a synthetic two-pad board written into a
//! per-test temporary directory, so these tests say something about the *binary* — argument
//! scanning, mode selection, what lands on disk, what the exit code is — and never about the
//! fixture corpus. The corpus-dependent assertions live in `fr-core`'s pipeline tests, where a
//! missing fixture is supposed to fail loudly.
//!
//! The exit codes are a contract with `bench/run_bench.py`: 3 means "routing was asked for and
//! this build cannot do it yet", which a harness must classify as unroutable-yet rather than as
//! a crash. A test that only checked `success()` would not notice 3 becoming 1.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Two pads on one net with a `(type route)` wire between them. Small enough to keep every
/// assertion below derivable by hand: `(resolution um 10)` with a 10 000-unit outline keeps the
/// scale factor at 10, and the one wire is the one item `(routes …)` has to carry.
const BOARD: &str = r#"(pcb cli_smoke
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
    (component pad2
      (place P1 1000 1000 front 0)
      (place P2 6000 1000 front 0)
    )
  )
  (library
    (image pad2 (pin round_pad 1 0 0))
    (padstack round_pad (shape (circle F.Cu 400)) (shape (circle B.Cu 400)) (attach off))
  )
  (network
    (net SIG (pins P1-1 P2-1))
    (class default SIG (rule (width 250) (clearance 200)))
  )
  (wiring
    (wire (path F.Cu 250 1000 1000 6000 1000) (net SIG) (type route))
  )
)"#;

/// A scratch directory holding `board.dsn`, removed when the test ends.
struct Workspace {
    dir: PathBuf,
}

impl Workspace {
    fn new(tag: &str) -> Workspace {
        let dir = std::env::temp_dir().join(format!("fr-cli-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the scratch directory");
        std::fs::write(dir.join("board.dsn"), BOARD).expect("write the board");
        Workspace { dir }
    }

    fn design(&self) -> PathBuf {
        self.dir.join("board.dsn")
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn run<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fr-cli"))
        .args(args)
        .output()
        .expect("fr-cli runs")
}

/// `ExitStatus::code()` is `None` when the process was killed by a signal, which is a distinct
/// failure from any exit code and must not be silently mapped onto one.
fn code(output: &Output) -> i32 {
    output
        .status
        .code()
        .unwrap_or_else(|| panic!("fr-cli died on a signal: {:?}", output.status))
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

// =================================================================================================
// help / version
// =================================================================================================

#[test]
fn help_and_version_still_work() {
    let help = run(&["--help"]);
    assert_eq!(code(&help), 0);
    let text = format!("{}{}", stdout(&help), stderr(&help));
    assert!(text.contains("fast-routing (Freerouting in Rust)"));
    assert!(text.contains("USAGE:"));
    // The new modes are documented, not just implemented.
    assert!(text.contains("--import-only"));
    assert!(text.contains("--golden-dump"));
    assert!(text.contains("--drc-report"));

    let version = run(&["-v"]);
    assert_eq!(code(&version), 0);
    assert!(stdout(&version).starts_with("fast-routing v"));
}

#[test]
fn no_arguments_is_a_usage_error() {
    let output = run::<&str>(&[]);
    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("USAGE:"));
}

// =================================================================================================
// the default path: routing was requested
// =================================================================================================

/// The headline behaviour change. A plain `-de/-do` run does the M1 work, writes a real session,
/// says what it did not do, and exits 3.
#[test]
fn a_routing_request_writes_the_passthrough_session_and_exits_three() {
    let workspace = Workspace::new("route");
    let ses = workspace.path("out.ses");
    let output = run(&[
        "-de".as_ref(),
        workspace.design().as_os_str(),
        "-do".as_ref(),
        ses.as_os_str(),
    ]);

    assert_eq!(code(&output), 3, "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains(
            "router rewrite in progress — running import+check+SES passthrough (M1); \
             routing lands in M2"
        ),
        "stderr: {}",
        stderr(&output)
    );

    let session = std::fs::read_to_string(&ses).expect("the session was written");
    // The session name comes from the *design* — `designName.replace(".dsn", ".ses")`,
    // `dsn-board-semantics.md` §12 — not from `-do`, which only says where to put the file.
    assert!(session.starts_with("(session board.ses"), "{session}");
    assert!(session.contains("(base_design board.dsn)"));
    assert!(session.contains("(routes"));
    // The imported wire is passed through, so the session is not empty of copper.
    assert!(session.contains("(wire"), "{session}");

    // The referee ran and reported.
    let report = stdout(&output);
    assert!(report.contains("Referee:"), "{report}");
    assert!(report.contains("clearance violations"), "{report}");
}

/// With no `-do`, the default path still writes `<input>.ses` next to the design — the
/// pre-rewrite behaviour, preserved.
#[test]
fn the_default_output_path_is_the_design_with_a_ses_extension() {
    let workspace = Workspace::new("default-out");
    let output = run(&["-de".as_ref(), workspace.design().as_os_str()]);
    assert_eq!(code(&output), 3);
    assert!(workspace.path("board.ses").is_file());
}

/// `-mp` and `-us` are inert but must not become argument errors: a harness passes them.
#[test]
fn the_inert_router_flags_are_still_accepted() {
    let workspace = Workspace::new("inert");
    let output = run(&[
        "-de".as_ref(),
        workspace.design().as_os_str(),
        "-mp".as_ref(),
        "20".as_ref(),
        "-us".as_ref(),
        "4".as_ref(),
    ]);
    assert_eq!(code(&output), 3, "stderr: {}", stderr(&output));
    assert!(stderr(&output).contains("-mp 20 is accepted but inert"));
    assert!(stderr(&output).contains("-us 4 is accepted but inert"));
}

// =================================================================================================
// --import-only
// =================================================================================================

/// Import-only asks for nothing the build cannot do, so it exits 0 — and writes no session
/// unless one was requested.
#[test]
fn import_only_exits_zero_and_writes_nothing_by_default() {
    let workspace = Workspace::new("import-only");
    let output = run(&[
        "-de".as_ref(),
        workspace.design().as_os_str(),
        "--import-only".as_ref(),
    ]);
    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));
    assert!(
        !workspace.path("board.ses").exists(),
        "nothing was asked for"
    );
    let report = stdout(&output);
    assert!(report.contains("Imported cli_smoke"), "{report}");
    assert!(report.contains("2 layers"), "{report}");
    assert!(
        !stderr(&output).contains("router rewrite in progress"),
        "import-only did not request routing"
    );
}

#[test]
fn import_only_writes_the_session_when_one_is_requested() {
    let workspace = Workspace::new("import-only-out");
    let ses = workspace.path("explicit.ses");
    let output = run(&[
        "-de".as_ref(),
        workspace.design().as_os_str(),
        "--import-only".as_ref(),
        "-do".as_ref(),
        ses.as_os_str(),
    ]);
    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));
    assert!(std::fs::read_to_string(&ses)
        .expect("the session was written")
        .contains("(routes"));
}

// =================================================================================================
// the gate emitters
// =================================================================================================

/// Gate G1.1's document, from the shipped binary rather than from `--example golden_emit`.
#[test]
fn golden_dump_emits_the_exchange_schema() {
    let workspace = Workspace::new("golden");
    let out = workspace.path("dump.json");
    let output = run(&[
        "-de".as_ref(),
        workspace.design().as_os_str(),
        "--golden-dump".as_ref(),
        out.as_os_str(),
    ]);
    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));

    let json = std::fs::read_to_string(&out).expect("the dump was written");
    assert!(
        json.contains("\"schema\": \"fast-routing.golden-dump/1\""),
        "{json}"
    );
    assert!(json.contains("\"source\": \"board.dsn\""), "{json}");
    assert!(json.contains("\"design\": \"cli_smoke\""), "{json}");
    // A dump mode must not also write a session.
    assert!(!workspace.path("board.ses").exists());
}

/// Gate G1.2a's document, likewise.
#[test]
fn drc_report_emits_the_java_shaped_json() {
    let workspace = Workspace::new("drc");
    let out = workspace.path("drc.json");
    let output = run(&[
        "-de".as_ref(),
        workspace.design().as_os_str(),
        "--drc-report".as_ref(),
        out.as_os_str(),
    ]);
    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));

    let json = std::fs::read_to_string(&out).expect("the report was written");
    assert!(json.contains("\"coordinate_units\": \"mm\""), "{json}");
    assert!(json.contains("\"source\": \"board.dsn\""), "{json}");
    assert!(json.contains("\"violations\": "), "{json}");
    assert!(!workspace.path("board.ses").exists());
}

/// The flags `bench/tools/drc_compare.py` passes must all be accepted together.
#[test]
fn drc_report_takes_the_comparator_flags() {
    let workspace = Workspace::new("drc-flags");
    let out = workspace.path("drc.json");
    let output = run(&[
        "-de".as_ref(),
        workspace.design().as_os_str(),
        "--drc-report".as_ref(),
        out.as_os_str(),
        "--unit".as_ref(),
        "um".as_ref(),
        "--pin-frame-fix".as_ref(),
        "--router-overrides".as_ref(),
    ]);
    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));
    let json = std::fs::read_to_string(&out).expect("the report was written");
    assert!(json.contains("\"coordinate_units\": \"um\""), "{json}");
}

#[test]
fn both_emitters_can_run_in_one_invocation() {
    let workspace = Workspace::new("both");
    let dump = workspace.path("dump.json");
    let drc = workspace.path("drc.json");
    let output = run(&[
        "-de".as_ref(),
        workspace.design().as_os_str(),
        "--golden-dump".as_ref(),
        dump.as_os_str(),
        "--drc-report".as_ref(),
        drc.as_os_str(),
    ]);
    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));
    assert!(dump.is_file() && drc.is_file());
}

// =================================================================================================
// failure modes
// =================================================================================================

#[test]
fn a_missing_design_is_a_usage_error() {
    let output = run(&["-do", "/tmp/whatever.ses"]);
    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("missing required input design file"));
}

#[test]
fn an_unreadable_design_is_an_error_not_a_panic() {
    let output = run(&["-de", "/nonexistent/definitely-not-here.dsn"]);
    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("read /nonexistent/definitely-not-here.dsn"));
}

/// A file that is not a DSN exits 1, **not** 3: the design is broken, not the router.
#[test]
fn a_non_dsn_input_exits_one_not_three() {
    let workspace = Workspace::new("junk");
    let junk = workspace.path("junk.dsn");
    std::fs::write(&junk, "this is not a board").expect("write");
    let output = run(&["-de".as_ref(), junk.as_os_str()]);
    assert_eq!(code(&output), 1, "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains("import failed"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn an_unknown_argument_is_reported_rather_than_ignored() {
    let workspace = Workspace::new("unknown");
    let output = run(&[
        "-de".as_ref(),
        workspace.design().as_os_str(),
        "--rout-it-please".as_ref(),
    ]);
    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("unknown argument '--rout-it-please'"));
}

#[test]
fn a_flag_missing_its_value_is_reported() {
    let output = run(&["-de"]);
    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("-de needs a value"));
}

#[test]
fn an_unknown_report_unit_is_rejected() {
    let workspace = Workspace::new("unit");
    let out = workspace.path("drc.json");
    let output = run(&[
        "-de".as_ref(),
        workspace.design().as_os_str(),
        "--drc-report".as_ref(),
        out.as_os_str(),
        "--unit".as_ref(),
        "furlong".as_ref(),
    ]);
    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("unknown unit 'furlong'"));
    assert!(!Path::new(&out).exists());
}
