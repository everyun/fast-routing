//! Headless Freerouting, in Rust — the drop-in CLI, now driving the M1 pipeline.
//!
//! # What changed in M1, and what it means for a caller
//!
//! The default path is [`fr_core::RoutingJob`] v2: `fr_io::specctra` import →
//! [`fr_board::Board`] + `fr_rules` → **no router** → `fr_check` referee → `fr_io` SES writer v2.
//! The pre-rewrite `DsnDocument` → `BasicBoard` → `fr-autoroute` chain is not reachable from
//! here at all; it survives as `fr_core::legacy` for its own tests until M2 deletes it.
//!
//! So a plain `-de board.dsn -do board.ses` invocation still reads the design, still judges it,
//! and still writes a valid session — but the session carries the wiring the *input* had, not a
//! route. Pretending otherwise would be the worst possible failure mode for a benchmark
//! harness, so the run says so on stderr and exits **3**:
//!
//! | exit | meaning |
//! |---|---|
//! | 0 | the requested work finished (`--import-only`, `--golden-dump`, `--drc-report`, `--version`, `--help`) |
//! | 1 | usage error, unreadable input, unwritable output, or an import failure |
//! | 2 | *(reserved — the pre-rewrite binary used it for "routed, but not clean")* |
//! | 3 | **routing was requested and this build cannot route yet**; import + check + SES passthrough ran and the `.ses` was written |
//!
//! 3 is deliberately distinct from 1: `bench/run_bench.py` classifies a crash and an
//! honest "not implemented" differently, and a board that cannot be routed yet must not be
//! confused with a board that broke the tool.
//!
//! # The gate flags are first class now
//!
//! `docs/ROADMAP.md` G1.1 says the exchange format is one "which `fr-cli` must emit".
//! Until M1 it was emitted by `cargo run -p fr-io --example golden_emit`, and the G1.2a JSON by
//! `cargo run -p fr-check --example drc_report`, so both gates depended on example binaries that
//! nothing shipped. Both are flags here now:
//!
//! ```sh
//! fr-cli -de board.dsn --import-only
//! fr-cli -de board.dsn --golden-dump board.golden.json
//! fr-cli -de board.dsn --drc-report board.drc.json --unit mm --pin-frame-fix
//! ```
//!
//! # Flags
//!
//! Every pre-existing flag is preserved, including the ones that no longer do anything:
//! `-mp/--max-passes` and `-us/--threads` are accepted and echoed, because a harness that
//! passes them must not start failing on an unknown-argument error. They are marked as inert in
//! `--help` rather than silently ignored.

use std::env;
use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::process;
use std::time::Instant;

use fr_core::drc_report::{self, ReportOptions, DRC_IMPORT_OPTIONS};
use fr_core::{JobError, JobReport, RoutingJob};
use fr_io::golden_dump;
use fr_io::specctra::ImportOptions;

/// `--import-only` and the two emitters exit 0; a routing request exits with this.
const EXIT_ROUTER_NOT_IMPLEMENTED: i32 = 3;
/// Usage, IO and import failures.
const EXIT_ERROR: i32 = 1;

const ROUTER_NOTICE: &str =
    "router rewrite in progress — running import+check+SES passthrough (M1); routing lands in M2";

fn print_usage() {
    eprintln!(
        r#"fast-routing (Freerouting in Rust) v{}

USAGE:
    fast-routing -de <input.dsn> [OPTIONS]

INPUT / OUTPUT
    -de, --design <FILE>        Input Specctra .dsn design file path (Required)
    -do, --output <FILE>        Output Specctra .ses session file path (Default: <input>.ses)

MODES (default: route — which in this build is import + check + SES passthrough, exit 3)
    --import-only               Import and judge the board; write the .ses only if -do was given
    --golden-dump <FILE>        Emit the `fast-routing.golden-dump/1` model dump (gate G1.1)
    --drc-report <FILE>         Emit the `java -drc` shaped violation JSON (gate G1.2a)

--drc-report OPTIONS
    --unit <mm|mil|inch|um>     Report unit (Default: mm, which is what the oracles use)
    --router-overrides          Import with the routing-path post-load overrides instead of
                                the -drc ones (board_edge class synthesis on)
    --emit-boxes                Also write one BOX line per item to stderr
    --pin-frame-fix             Deprecated no-op; the importer now places pads in the pin frame

ACCEPTED BUT INERT UNTIL M2 (kept so existing harnesses keep parsing)
    -mp, --max-passes <N>       Autorouter passes
    -us, --threads <N>          Rayon worker threads

    -v,  --version              Print version information
    -h,  --help                 Print help

EXIT CODES
    0  the requested work finished          2  (reserved)
    1  usage / IO / import failure          3  routing requested; not implemented yet (M1)
"#,
        env!("CARGO_PKG_VERSION")
    );
}

/// Everything the argument scan produced.
#[derive(Default)]
struct Args {
    design: Option<String>,
    output: Option<String>,
    golden_dump: Option<String>,
    drc_report: Option<String>,
    import_only: bool,
    unit: String,
    router_overrides: bool,
    emit_boxes: bool,
    max_passes: Option<usize>,
    threads: Option<usize>,
}

fn fail(message: &str) -> ! {
    eprintln!("fr-cli: {message}");
    process::exit(EXIT_ERROR)
}

fn main() {
    let argv: Vec<String> = env::args().collect();
    if argv.len() <= 1 {
        print_usage();
        process::exit(EXIT_ERROR);
    }

    let mut args = Args {
        unit: "mm".to_string(),
        ..Args::default()
    };
    let mut index = 1;
    // `next` consumes the value of a `--flag <value>` pair, or fails with the flag's name —
    // the pre-rewrite scan silently dropped a trailing flag, which turned a typo into a
    // "missing required input" message about the wrong argument.
    macro_rules! next {
        ($flag:expr) => {{
            index += 1;
            match argv.get(index) {
                Some(value) => value.clone(),
                None => fail(&format!("{} needs a value", $flag)),
            }
        }};
    }
    while index < argv.len() {
        match argv[index].as_str() {
            "-de" | "--design" | "-di" => args.design = Some(next!("-de")),
            "-do" | "--output" => args.output = Some(next!("-do")),
            "--golden-dump" => args.golden_dump = Some(next!("--golden-dump")),
            "--drc-report" => args.drc_report = Some(next!("--drc-report")),
            "--import-only" => args.import_only = true,
            "--unit" => args.unit = next!("--unit"),
            // Was an experiment that measured a bug in `specctra::build::placed_pad_shapes`.
            // The bug is fixed, so the correction is now a *displacement*; the flag stays only
            // because `bench/tools/drc_compare.py` still passes it.
            "--pin-frame-fix" => eprintln!(
                "note: --pin-frame-fix is a no-op and is deprecated; the importer now places \
                 pads in the pin frame itself (fr-io specctra::build::placed_pad_shapes)"
            ),
            "--router-overrides" => args.router_overrides = true,
            "--emit-boxes" => args.emit_boxes = true,
            "-mp" | "--max-passes" => args.max_passes = next!("-mp").parse().ok(),
            "-us" | "--threads" => args.threads = next!("-us").parse().ok(),
            "-v" | "--version" => {
                println!("fast-routing v{}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "-h" | "--help" => {
                print_usage();
                return;
            }
            other => fail(&format!("unknown argument '{other}' (try --help)")),
        }
        index += 1;
    }

    let Some(input_path) = args.design.clone() else {
        fail("missing required input design file (-de <file.dsn>)");
    };

    if let Some(passes) = args.max_passes {
        eprintln!("note: -mp {passes} is accepted but inert until M2 — nothing routes yet");
    }
    if let Some(threads) = args.threads {
        eprintln!("note: -us {threads} is accepted but inert until M2 — nothing routes yet");
    }

    let job = match RoutingJob::from_path(&input_path) {
        Ok(job) => job,
        Err(error) => fail(&error.to_string()),
    };

    // The two emitters are their own modes: they import with their own options and never touch
    // the referee or the session writer. Both may be requested in one run.
    if args.golden_dump.is_some() || args.drc_report.is_some() {
        emit_reports(&job, &args, &input_path);
        return;
    }

    let started = Instant::now();
    let report = match job.run() {
        Ok(report) => report,
        Err(error) => fail(&describe(&error)),
    };
    print_summary(&report, started);
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }

    // `--import-only` writes a session only when one was asked for; the default path always
    // does, because a caller that passed no `-do` still expects `<input>.ses`.
    let session_path = if args.import_only {
        args.output.clone()
    } else {
        Some(args.output.clone().unwrap_or_else(|| default_ses(&input_path)))
    };
    if let Some(path) = &session_path {
        if let Err(error) = fs::write(path, &report.ses) {
            fail(&format!("write {path}: {error}"));
        }
        println!("Exported Specctra SES session file: {path}");
    }

    if args.import_only {
        return;
    }
    eprintln!("{ROUTER_NOTICE}");
    process::exit(EXIT_ROUTER_NOT_IMPLEMENTED);
}

/// `<input>.dsn` → `<input>.ses`; anything else just gains `.ses`.
fn default_ses(input: &str) -> String {
    match input.strip_suffix(".dsn") {
        Some(stem) => format!("{stem}.ses"),
        None => format!("{input}.ses"),
    }
}

fn describe(error: &JobError) -> String {
    match error {
        JobError::Import(_) => format!("import failed: {error}"),
        JobError::Read(..) => error.to_string(),
    }
}

fn print_summary(report: &JobReport, started: Instant) {
    println!(
        "Imported {} in {:.2}s: {} layers, {} items ({} pins, {} traces, {} vias)",
        report.pcb_name,
        started.elapsed().as_secs_f64(),
        report.layer_count,
        report.census.total(),
        report.census.pins,
        report.census.traces,
        report.census.vias,
    );
    println!(
        "Referee: {} clearance violations, {} of {} connections incomplete, score {:.2}/1000",
        report.stats.clearance_violation_count,
        report.stats.incomplete_count,
        report.stats.max_connections,
        report.score.normalized,
    );
}

/// `--golden-dump` and `--drc-report`.
///
/// Each re-imports with the options its oracle was generated under, rather than sharing one
/// board: `-drc` runs the post-load overrides *off* and the `-do` dump path runs them *on*
/// ([`DRC_IMPORT_OPTIONS`]), so a shared import would silently be wrong for one of them.
fn emit_reports(job: &RoutingJob, args: &Args, input_path: &str) {
    let source = Path::new(input_path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();

    if let Some(out) = &args.golden_dump {
        let design = match job.import() {
            Ok(design) => design,
            Err(error) => fail(&describe(&error)),
        };
        if let Err(error) = fs::write(out, golden_dump::emit(&design, &source)) {
            fail(&format!("write {out}: {error}"));
        }
        println!("Wrote golden dump: {out}");
    }

    if let Some(out) = &args.drc_report {
        // Validated before the import, which on the larger fixtures is seconds of work to
        // throw away over a typo.
        let Some(report_options) = ReportOptions::default().with_unit_name(&args.unit) else {
            fail(&format!("unknown unit '{}'", args.unit));
        };
        let options = if args.router_overrides {
            ImportOptions::default()
        } else {
            DRC_IMPORT_OPTIONS
        };
        let design = match job.import_with(options) {
            Ok(design) => design,
            Err(error) => fail(&describe(&error)),
        };
        if args.emit_boxes {
            let mut stderr = std::io::stderr().lock();
            let _ = drc_report::emit_boxes(&design, &mut stderr);
            let _ = stderr.flush();
        }
        let json = drc_report::report(&design, &source, &report_options);
        if let Err(error) = fs::write(out, json) {
            fail(&format!("write {out}: {error}"));
        }
        println!("Wrote DRC report: {out}");
    }
}
