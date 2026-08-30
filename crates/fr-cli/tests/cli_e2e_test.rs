//! `fr-cli` end to end: the four modes, the four exit codes, and the flags that must keep
//! parsing.
//!
//! Hermetic on purpose. Every case runs against a synthetic two-pad board written into a
//! per-test temporary directory, so these tests say something about the *binary* — argument
//! scanning, mode selection, what lands on disk, what the exit code is — and never about the
//! fixture corpus. The corpus-dependent assertions live in `fr-core`'s pipeline tests, where a
//! missing fixture is supposed to fail loudly.
//!
//! The exit codes are a contract with `bench/run_bench.py`: **2** means "the board was routed and
//! the session written, but incompletes remain", which a harness must classify as a routing-quality
//! result rather than as a crash; **1** is a tool failure. A test that only checked `success()`
//! would not notice 2 becoming 1. (The M1-only exit 3, "cannot route yet", is gone.)

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

/// Four pads on two nets and **no `(wiring)` scope at all**: nothing is connected, so the
/// router has to make both connections itself. The two nets are 3 mm apart and each spans 5 mm
/// on an empty 10 mm board, so a straight trace on `F.Cu` exists for both and the run must come
/// out with zero incompletes and zero violations.
const UNROUTED_BOARD: &str = r#"(pcb cli_route
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
    (component pad4
      (place P1 1000 1000 front 0)
      (place P2 6000 1000 front 0)
      (place P3 1000 4000 front 0)
      (place P4 6000 4000 front 0)
    )
  )
  (library
    (image pad4 (pin round_pad 1 0 0))
    (padstack round_pad (shape (circle F.Cu 400)) (shape (circle B.Cu 400)) (attach off))
  )
  (network
    (net SIG (pins P1-1 P2-1))
    (net SIG2 (pins P3-1 P4-1))
    (class default SIG SIG2 (rule (width 250) (clearance 200)))
  )
)"#;

/// A scratch directory holding `board.dsn`, removed when the test ends.
struct Workspace {
    dir: PathBuf,
}

impl Workspace {
    fn new(tag: &str) -> Workspace {
        Workspace::with_board(tag, BOARD)
    }

    fn with_board(tag: &str, board: &str) -> Workspace {
        let dir = std::env::temp_dir().join(format!("fr-cli-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the scratch directory");
        std::fs::write(dir.join("board.dsn"), board).expect("write the board");
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

/// The headline behaviour change: a plain `-de/-do` run **routes**. This board arrives with its
/// one connection already wired, so the router finds nothing to do, the referee finds nothing
/// wrong, and the run exits 0.
#[test]
fn a_routing_request_writes_the_session_and_exits_zero_when_the_board_is_complete() {
    let workspace = Workspace::new("route");
    let ses = workspace.path("out.ses");
    let output = run(&[
        "-de".as_ref(),
        workspace.design().as_os_str(),
        "-do".as_ref(),
        ses.as_os_str(),
    ]);

    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));

    let session = std::fs::read_to_string(&ses).expect("the session was written");
    // The session name comes from the *design* — `designName.replace(".dsn", ".ses")`,
    // `dsn-board-semantics.md` §12 — not from `-do`, which only says where to put the file.
    assert!(session.starts_with("(session board.ses"), "{session}");
    assert!(session.contains("(base_design board.dsn)"));
    assert!(session.contains("(routes"));
    assert!(session.contains("(wire"), "{session}");

    // Both stages reported.
    let report = stdout(&output);
    assert!(report.contains("Router:"), "{report}");
    assert!(report.contains("Referee:"), "{report}");
    assert!(report.contains("clearance violations"), "{report}");
}

/// **The M2a acceptance case.** An unrouted two-net board goes in, a routed one comes out: zero
/// incompletes, zero clearance violations from the referee, real copper in the session, exit 0.
#[test]
fn an_unrouted_two_net_board_comes_out_complete_and_clean() {
    let workspace = Workspace::with_board("unrouted", UNROUTED_BOARD);
    let ses = workspace.path("routed.ses");
    let output = run(&[
        "-de".as_ref(),
        workspace.design().as_os_str(),
        "-do".as_ref(),
        ses.as_os_str(),
        "-mp".as_ref(),
        "1".as_ref(),
    ]);

    let report = stdout(&output);
    assert_eq!(
        code(&output),
        0,
        "stdout: {report}\nstderr: {}",
        stderr(&output)
    );
    assert!(
        report.contains("0 clearance violations, 0 of 2 connections incomplete"),
        "{report}"
    );
    // Two connections, so at least two routed items and no failures in the final pass.
    assert!(report.contains("Router:"), "{report}");
    assert!(report.contains("0 failed"), "{report}");

    // The session carries copper the input did not have.
    let session = std::fs::read_to_string(&ses).expect("the session was written");
    assert!(session.contains("(wire"), "{session}");
    assert!(
        session.matches("(wire").count() >= 2,
        "one wire per net at least: {session}"
    );
}

/// A board the router cannot finish exits 2, not 0 and not 1. Nothing on this board can reach
/// `NC`'s second pad — it is outside the outline — so one connection stays open.
#[test]
fn an_unfinishable_board_exits_two_with_the_session_still_written() {
    let unreachable = UNROUTED_BOARD.replace(
        "(place P4 6000 4000 front 0)",
        "(place P4 60000 40000 front 0)",
    );
    let workspace = Workspace::with_board("incomplete", &unreachable);
    let ses = workspace.path("partial.ses");
    let output = run(&[
        "-de".as_ref(),
        workspace.design().as_os_str(),
        "-do".as_ref(),
        ses.as_os_str(),
    ]);

    assert_eq!(code(&output), 2, "stdout: {}", stdout(&output));
    assert!(
        stderr(&output).contains("still incomplete"),
        "stderr: {}",
        stderr(&output)
    );
    assert!(ses.is_file(), "the session is written even when incomplete");
}

// =================================================================================================
// --job-timeout and the signal path: never leave the table empty
// =================================================================================================

/// A budget of zero is already spent, so the router stops before its first pass — and the run
/// still judges the board and writes the session. Exit 4, not 2: the caller has to be able to
/// tell "this is as far as the budget got" from "this is as far as the router got".
///
/// Zero seconds is what makes this test deterministic. Every other value would be a race
/// between the machine and the assertion.
#[test]
fn a_spent_job_budget_still_writes_the_session_and_exits_four() {
    let workspace = Workspace::with_board("deadline", UNROUTED_BOARD);
    let ses = workspace.path("deadline.ses");
    let output = run(&[
        "-de".as_ref(),
        workspace.design().as_os_str(),
        "-do".as_ref(),
        ses.as_os_str(),
        "--job-timeout".as_ref(),
        "0".as_ref(),
    ]);

    assert_eq!(code(&output), 4, "stdout: {}", stdout(&output));
    let report = stdout(&output);
    assert!(
        report.contains("Router stopped on job deadline"),
        "the stop reason names the budget: {report}"
    );
    assert!(
        report.contains("0 passes"),
        "a spent budget routes nothing: {report}"
    );
    assert!(
        stderr(&output).contains("job budget spent"),
        "stderr: {}",
        stderr(&output)
    );

    // The session is on disk and is a real one — the *imported* board, which on this fixture
    // is bare copper-free, but a session a caller can read all the same.
    let session = std::fs::read_to_string(&ses).expect("the session was written");
    assert!(session.starts_with("(session board.ses"), "{session}");
    assert!(session.contains("(routes"), "{session}");
}

/// A budget nothing comes close to spending changes nothing at all: same exit code, same
/// session as the run without one.
#[test]
fn a_job_budget_that_is_not_spent_changes_nothing() {
    let workspace = Workspace::with_board("deadline-slack", UNROUTED_BOARD);
    let with = workspace.path("with.ses");
    let without = workspace.path("without.ses");
    let budgeted = run(&[
        "-de".as_ref(),
        workspace.design().as_os_str(),
        "-do".as_ref(),
        with.as_os_str(),
        "--job-timeout".as_ref(),
        "600".as_ref(),
    ]);
    let plain = run(&[
        "-de".as_ref(),
        workspace.design().as_os_str(),
        "-do".as_ref(),
        without.as_os_str(),
    ]);

    assert_eq!(code(&budgeted), 0, "stderr: {}", stderr(&budgeted));
    assert_eq!(code(&plain), 0, "stderr: {}", stderr(&plain));
    assert_eq!(
        std::fs::read_to_string(&with).expect("with"),
        std::fs::read_to_string(&without).expect("without"),
        "an unspent budget is not allowed to change the board"
    );
}

/// A bad `--job-timeout` is refused rather than silently ignored, for the same reason `-mp tow`
/// is: a typo in a long bench run must not look like a successful one.
#[test]
fn a_bad_job_timeout_is_a_usage_error() {
    let workspace = Workspace::new("deadline-typo");
    for bad in ["-5", "soon", ""] {
        let output = run(&[
            "-de".as_ref(),
            workspace.design().as_os_str(),
            "--job-timeout".as_ref(),
            bad.as_ref(),
        ]);
        assert_eq!(code(&output), 1, "--job-timeout {bad}");
        assert!(
            stderr(&output).contains("--job-timeout"),
            "the message names the flag: {}",
            stderr(&output)
        );
    }
    // And a missing value is the usual "needs a value", not a panic.
    let output = run(&[
        "-de".as_ref(),
        workspace.design().as_os_str(),
        "--job-timeout".as_ref(),
    ]);
    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("needs a value"));
}

/// **`SIGTERM` must never cost us the board.** The signal sets the same flag the deadline
/// does, so the run leaves through the ordinary path: session written, exit code 4.
///
/// The one race that has to be excluded is the process's own start-up: a signal that arrives
/// before `main` installs the handler is fatal in any program, and nothing this binary does can
/// change that. `main` installs it on its first line, before it looks at a single argument.
///
/// This test used to wait for the child's **first output** — the `-us N is accepted but inert`
/// note, written during argument scanning — as proof that the handler was up. That note is gone
/// now that `--threads` is live, and with it went the binary's only pre-run output: `fr-cli`
/// prints nothing at all until `print_summary` runs, i.e. until the job is already over. There
/// is no observable "I have started" edge left to synchronise on, so this is a grace period
/// instead, and the length is not arbitrary: **250 ms was measured to be too short** on a
/// loaded machine — `exec` plus dyld plus the Rust runtime had not reached `main`'s first line,
/// the signal hit the default disposition, and the child died of `SIGTERM` (raw wait status 15).
/// Two seconds makes that window negligible without making the suite slower in any way anyone
/// will notice.
///
/// Whether the run is still going when the signal lands is left open on purpose — both outcomes
/// satisfy the contract — but note that on a board this small it usually *has* finished by then,
/// so the signal path itself is only lightly exercised. Restoring a real synchronisation point
/// means giving `fr-cli` something to say before it starts working; that is a CLI-contract
/// decision, not a test fix.
#[test]
fn a_sigterm_does_not_cost_us_the_session() {
    let workspace = Workspace::with_board("sigterm", UNROUTED_BOARD);
    let ses = workspace.path("sigterm.ses");
    let mut child = Command::new(env!("CARGO_BIN_EXE_fr-cli"))
        .args([
            "-de".as_ref(),
            workspace.design().as_os_str(),
            "-do".as_ref(),
            ses.as_os_str(),
            "-mp".as_ref(),
            "20".as_ref(),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("fr-cli spawns");

    let mut piped = child.stderr.take().expect("stderr is piped");
    // See the doc comment: 250 ms was measured to be too short under load.
    std::thread::sleep(std::time::Duration::from_secs(2));

    // `kill(1)` rather than a `libc` dependency, for the same reason `main.rs` declares
    // `signal` itself: this workspace has no third-party crates. A run that already finished
    // makes this a no-op, which is fine — the assertions below do not depend on it landing.
    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .expect("kill runs");

    let status = child.wait().expect("fr-cli exits");
    let mut output = String::new();
    let _ = std::io::Read::read_to_string(&mut piped, &mut output);
    assert!(
        status.code().is_some(),
        "a first SIGTERM must be handled, not fatal: {status:?}"
    );
    assert!(
        matches!(status.code(), Some(0 | 2 | 4)),
        "exit {:?} — a handled signal is still a result: {output}",
        status.code(),
    );
    assert!(
        ses.is_file(),
        "the session must exist however the run ended: {output}"
    );
}

/// With no `-do`, the default path still writes `<input>.ses` next to the design — the
/// pre-rewrite behaviour, preserved.
#[test]
fn the_default_output_path_is_the_design_with_a_ses_extension() {
    let workspace = Workspace::new("default-out");
    let output = run(&["-de".as_ref(), workspace.design().as_os_str()]);
    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));
    assert!(workspace.path("board.ses").is_file());
}

/// `-mp` and `-us` are both live now, and neither may become an argument error: a harness
/// passes both. Neither may announce itself as inert either — `-us` did until `--threads`
/// started driving the T1 optimizer and the T2 referee scan, and a stale note is a lie a
/// harness would go on believing.
#[test]
fn the_router_flags_are_still_accepted() {
    let workspace = Workspace::new("inert");
    let output = run(&[
        "-de".as_ref(),
        workspace.design().as_os_str(),
        "-mp".as_ref(),
        "20".as_ref(),
        "-us".as_ref(),
        "4".as_ref(),
    ]);
    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));
    assert!(
        !stderr(&output).contains("inert"),
        "both flags are live now: {}",
        stderr(&output)
    );
    // `--portfolio` is the other count flag on the same scan; a harness may pass it too.
    let output = run(&[
        "-de".as_ref(),
        workspace.design().as_os_str(),
        "--portfolio".as_ref(),
        "1".as_ref(),
    ]);
    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));
}

/// `--settle-trace` is the differential harness's producer: one `FRMETRIC settle` record per
/// settled maze element, in the lab jar's field order.
#[test]
fn settle_trace_writes_the_frlab_shaped_records() {
    let workspace = Workspace::with_board("settle", UNROUTED_BOARD);
    let trace = workspace.path("settle.log");
    let output = run(&[
        "-de".as_ref(),
        workspace.design().as_os_str(),
        "-do".as_ref(),
        workspace.path("out.ses").as_os_str(),
        "--settle-trace".as_ref(),
        trace.as_os_str(),
    ]);
    assert_eq!(code(&output), 0, "stderr: {}", stderr(&output));

    let log = std::fs::read_to_string(&trace).expect("the trace was written");
    let mut lines = log.lines().filter(|line| line.starts_with("FRMETRIC settle "));
    let first = lines.next().expect("at least one settle record");
    // `FRMETRIC settle <door_id> <section> <g> <f>` — four fields after the tag.
    let fields: Vec<&str> = first.split_whitespace().collect();
    assert_eq!(fields.len(), 6, "{first}");
    assert_eq!(fields[0], "FRMETRIC");
    assert_eq!(fields[1], "settle");
    fields[2].parse::<u64>().expect("door id");
    fields[3].parse::<u32>().expect("section");
    let g: f64 = fields[4].parse().expect("g");
    let f: f64 = fields[5].parse().expect("f");
    assert!(f >= g, "f is never below g: {first}");
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

/// A file that is not a DSN exits 1, **not** 2: the design is broken, not the routing.
#[test]
fn a_non_dsn_input_exits_one_not_two() {
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

// =================================================================================================
// the summary line means what it says
// =================================================================================================

/// The first word of the first line is "Imported", and the counts after it are the importer's.
///
/// They were the *router's*: the summary read `JobReport::census`, which is taken after the
/// router stage, so a routed run reported items the importer never saw — on `Issue103` the line
/// claimed 4 617 items for a board the DSN describes with a different number. The two counts are
/// separate now, and a routing run prints both.
#[test]
fn the_imported_line_counts_the_imported_board_not_the_routed_one() {
    let workspace = Workspace::with_board("census", UNROUTED_BOARD);
    let ses = workspace.path("routed.ses");

    let import_only = run(&[
        "-de".as_ref(),
        workspace.design().as_os_str(),
        "--import-only".as_ref(),
    ]);
    assert_eq!(code(&import_only), 0, "stderr: {}", stderr(&import_only));
    let imported_line = imported_counts(&stdout(&import_only));

    let routed = run(&[
        "-de".as_ref(),
        workspace.design().as_os_str(),
        "-do".as_ref(),
        ses.as_os_str(),
        "-mp".as_ref(),
        "1".as_ref(),
    ]);
    assert_eq!(code(&routed), 0, "stderr: {}", stderr(&routed));
    let report = stdout(&routed);

    assert_eq!(
        imported_counts(&report),
        imported_line,
        "routing must not change what the import line reports: {report}"
    );
    // This board arrives with no wiring at all and leaves with two traces, so the claim is
    // falsifiable in both directions: the import line must say zero and the board line two.
    assert!(
        imported_line.contains("0 traces"),
        "the input has no wiring: {report}"
    );
    assert!(
        report
            .contains("Board: 7 items (4 pins, 2 traces, 0 vias); router delta +2 traces, +0 vias"),
        "{report}"
    );
    // …and the board line belongs to the router, so an import-only run does not print it.
    assert!(
        !stdout(&import_only).contains("Board:"),
        "{}",
        stdout(&import_only)
    );
}

/// The `N items (…)` tail of the `Imported` line.
fn imported_counts(report: &str) -> String {
    let line = report
        .lines()
        .find(|line| line.starts_with("Imported "))
        .unwrap_or_else(|| panic!("no Imported line in:\n{report}"));
    let (_, counts) = line.split_once(": ").expect("the line has a colon");
    counts.to_string()
}
