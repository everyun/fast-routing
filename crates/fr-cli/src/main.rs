//! Headless Freerouting, in Rust — the drop-in CLI, driving the M2a pipeline.
//!
//! # What a plain run does
//!
//! `-de board.dsn -do board.ses` runs [`fr_core::RoutingJob`] v2 end to end: `fr_io::specctra`
//! import → [`fr_board::Board`] + `fr_rules` → **`fr_router`, `-mp` passes of it** → `fr_check`
//! referee → `fr_io` SES writer v2. The session carries our copper. The pre-rewrite
//! `DsnDocument` → `BasicBoard` → `fr-autoroute` chain is not reachable from here at all; it
//! survives as `fr_core::legacy` for its own tests until M2 deletes it.
//!
//! | exit | meaning |
//! |---|---|
//! | 0 | the requested work finished **and nothing is left unconnected** |
//! | 1 | usage error, unreadable input, unwritable output, or an import failure |
//! | 2 | the board was routed and the `.ses` written, but incompletes remain |
//!
//! 2 is the pre-rewrite binary's contract ("routed, but not clean"), restored now that there is
//! a router behind it. The M1-only exit 3 ("cannot route yet") is gone; `bench/run_bench.py`
//! reads 2 as a routing-quality result and 1 as a tool failure.
//!
//! `--import-only`, `--golden-dump` and `--drc-report` never route and always exit 0 on success:
//! their contract is "this is what the *input* looks like", and the gates depend on it.
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
//! Every pre-existing flag is preserved. `-mp/--max-passes` is live now: it is the pass cap the
//! batch driver runs to, and pass *n* routes at `start_ripup_costs * n`
//! (`batch-orchestration.md` §4). `-us/--threads` is still inert, and honestly so: upstream
//! 2.3.0 routes strictly sequentially — its multi-threaded pass is private with no call site
//! (conformance #107, #111).
//!
//! `--settle-trace FILE` turns on the differential harness's record stream: one
//! `FRMETRIC settle <door> <section> <g> <f>` line per settled maze element, the same shape the
//! lab jar emits under `-Dfrlab.settle_trace=1`, for `bench/tools/frlab_parse.py`.

use std::env;
use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::process;
use std::time::Instant;

use fr_core::drc_report::{self, ReportOptions, DRC_IMPORT_OPTIONS};
use fr_core::{JobError, JobReport, MazeOptions, RouterStage, RoutingJob};
use fr_io::golden_dump;
use fr_io::specctra::ImportOptions;

/// Routed, `.ses` written, but the board still has incompletes.
const EXIT_INCOMPLETE: i32 = 2;
/// Usage, IO and import failures.
const EXIT_ERROR: i32 = 1;

fn print_usage() {
    eprintln!(
        r#"fast-routing (Freerouting in Rust) v{}

USAGE:
    fast-routing -de <input.dsn> [OPTIONS]

INPUT / OUTPUT
    -de, --design <FILE>        Input Specctra .dsn design file path (Required)
    -do, --output <FILE>        Output Specctra .ses session file path (Default: <input>.ses)

MODES (default: route)
    --import-only               Import and judge the board; write the .ses only if -do was given
    --golden-dump <FILE>        Emit the `fast-routing.golden-dump/1` model dump (gate G1.1)
    --drc-report <FILE>         Emit the `java -drc` shaped violation JSON (gate G1.2a)

ROUTING OPTIONS
    -mp, --max-passes <N>       Ripup-and-reroute passes (Default: 1). Pass n rips at 100*n.
                                Must be >= 0; a negative value is an error (upstream runs it
                                uncapped instead — see docs/PARITY-DIVERGENCES.md)
    --fanout                    Run the SMD pin-escape pre-pass first (off by default in M2a)
    --deterministic             Never read the wall clock for budget decisions: the
                                per-connection allowance is spent in effort-based virtual time

--drc-report OPTIONS
    --unit <mm|mil|inch|um>     Report unit (Default: mm, which is what the oracles use)
    --router-overrides          Import with the routing-path post-load overrides instead of
                                the -drc ones (board_edge class synthesis on)
    --emit-boxes                Also write one BOX line per item to stderr
    --pin-frame-fix             Deprecated no-op; the importer now places pads in the pin frame

DIAGNOSTICS
    --settle-trace <FILE>       Write one `FRMETRIC settle` line per settled maze element, the
                                lab jar's `-Dfrlab.settle_trace=1` format (`1`/`stderr` = stderr)

ACCEPTED BUT INERT (kept so existing harnesses keep parsing)
    -us, --threads <N>          Worker threads; upstream 2.3.0 routes single-threaded

    -v,  --version              Print version information
    -h,  --help                 Print help

EXIT CODES
    0  finished, nothing left unconnected   2  routed and written, incompletes remain
    1  usage / IO / import failure
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
    fanout: bool,
    deterministic: bool,
    settle_trace: Option<String>,
}

fn fail(message: &str) -> ! {
    eprintln!("fr-cli: {message}");
    process::exit(EXIT_ERROR)
}

/// `-mp <N>`, with the negative value rejected instead of silently ignored.
///
/// **A declared divergence** (`docs/PARITY-DIVERGENCES.md`). Upstream accepts a
/// negative `max_passes`, and it is a latent bug: the entry point's guard
/// `getRunRouter() && (maxPasses == null || maxPasses >= 0)` sends a negative
/// value down the *fanout-only* branch, which sets `maxPasses = 0`; but
/// `BatchAutorouter.runBatchLoop:894` recomputes the same guard and `:931`
/// requires `maxPasses > 0` for the cap to apply — so the router re-enables
/// itself and runs with **no pass cap at all** until a stagnation rule fires
/// (conformance appendix §1.2, which names it "the cautionary example").
///
/// Refusing the input is the smallest fix that cannot surprise anyone: `-mp -1`
/// has no sensible meaning, nobody can be relying on 'unbounded' from it, and
/// the alternative — silently clamping — hides a typo in a long run. A
/// non-numeric value is rejected for the same reason; the pre-existing
/// `.parse().ok()` turned `-mp tow` into a silent single-pass run.
fn parse_max_passes(value: &str) -> usize {
    match value.parse::<i64>() {
        Ok(passes) if passes < 0 => fail(&format!(
            "-mp {passes}: the pass count must be >= 0 (upstream runs a negative one uncapped; \
             see docs/PARITY-DIVERGENCES.md)"
        )),
        Ok(passes) => usize::try_from(passes).unwrap_or(usize::MAX),
        Err(_) => fail(&format!("-mp {value}: expected a whole number of passes")),
    }
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
            "-mp" | "--max-passes" => args.max_passes = Some(parse_max_passes(&next!("-mp"))),
            "-us" | "--threads" => args.threads = next!("-us").parse().ok(),
            "--fanout" => args.fanout = true,
            "--deterministic" => args.deterministic = true,
            "--settle-trace" => args.settle_trace = Some(next!("--settle-trace")),
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

    if let Some(threads) = args.threads {
        eprintln!(
            "note: -us {threads} is accepted but inert — upstream 2.3.0 routes single-threaded"
        );
    }
    // Must be set before anything asks the sink, which resolves the environment once.
    if let Some(path) = &args.settle_trace {
        env::set_var("FR_SETTLE_TRACE", path);
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

    // `--import-only` keeps the M1 contract: no router stage, so the board the referee judges
    // and the session carries is exactly the imported one.
    let job = if args.import_only {
        job
    } else {
        let passes = args.max_passes.unwrap_or(1).max(1);
        let passes = i32::try_from(passes).unwrap_or(i32::MAX);
        job.with_stage(RouterStage::Maze(
            MazeOptions::with_passes(passes)
                .with_fanout(args.fanout)
                .with_deterministic(args.deterministic),
        ))
    };

    let started = Instant::now();
    let report = match job.run() {
        Ok(report) => report,
        Err(error) => fail(&describe(&error)),
    };
    fr_metrics::settle_trace::flush();
    // `FR_METRICS_REPORT=1` dumps the `FRMETRIC` counter/stage/pass report to stderr. It is
    // env-gated rather than a flag because it is measurement scaffolding for `bench/tools/`,
    // not part of the drop-in CLI contract; the report is all-zero without the `metrics`
    // feature, so the gate costs one `var_os` per run.
    if std::env::var_os("FR_METRICS_REPORT").is_some() {
        for line in fr_metrics::report().to_frmetric_lines() {
            eprintln!("{line}");
        }
    }
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
    // The old contract, now that there is a router behind it: 0 when the board came out fully
    // connected, 2 when it did not. The referee's count is the one that decides, not the
    // router's own — they are computed by different code over the same board, and disagreeing
    // with the judge in our favour is the one failure mode a harness cannot detect.
    if report.stats.incomplete_count > 0 {
        eprintln!(
            "{} of {} connections still incomplete",
            report.stats.incomplete_count, report.stats.max_connections
        );
        process::exit(EXIT_INCOMPLETE);
    }
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
    if let Some(router) = &report.router {
        println!(
            "Router: {} passes, {} routed, {} failed, {} ripped, {} pins fanned out",
            router.passes, router.routed, router.not_routed, router.ripped, router.fanouted,
        );
        println!(
            "Router stopped on {} ({} board restore{}{}{})",
            router.stop_reason,
            router.restores,
            if router.restores == 1 { "" } else { "s" },
            if router.best_board_restored {
                ", finished on the best board seen"
            } else {
                ""
            },
            if router.connections_hitting_budget > 0 {
                format!(
                    ", {} connections hit their budget",
                    router.connections_hitting_budget
                )
            } else {
                String::new()
            },
        );
    }
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
