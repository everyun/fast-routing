//! Headless Freerouting, in Rust — the CLI driving the routing pipeline.
//!
//! # What a plain run does
//!
//! `-de board.dsn -do board.ses` runs [`fr_core::RoutingJob`] end to end: `fr_io::specctra`
//! import → [`fr_board::Board`] + `fr_rules` → **`fr_router`, `-mp` passes of it, then the
//! optimization stage** → `fr_check` referee → `fr_io` SES writer v2. The optimizer is on by
//! default, as it is upstream; `--no-optimizer` is the way off. The session carries our copper.
//!
//! | exit | meaning |
//! |---|---|
//! | 0 | the requested work finished **and nothing is left unconnected** |
//! | 1 | usage error, unreadable input, unwritable output, or an import failure |
//! | 2 | the board was routed and the `.ses` written, but incompletes remain |
//! | 4 | `--job-timeout` ran out, or a signal arrived — **the `.ses` was still written** |
//!
//! 2 is the pre-rewrite binary's contract ("routed, but not clean"), restored now that there is
//! a router behind it. The M1-only exit 3 ("cannot route yet") is gone; `bench/run_bench.py`
//! reads 2 as a routing-quality result and 1 as a tool failure.
//!
//! # Never leave the table empty
//!
//! 4 is the M2 addition, and the rule behind it is that **this binary must never be killed
//! holding a routed board it did not write**. Two things enforce it:
//!
//! * `--job-timeout SECS` arms a wall budget for the whole job. The router polls it between
//!   connections, between passes and between optimizer items and, when it is gone, stops the way
//!   a stop rule stops it: the best retained board is restored, the session is written, the
//!   referee judges it. **The budget pays for that shutdown itself**: once the board is imported
//!   the job reserves a board-sized slice of the budget for it
//!   (`fr_router::reserve_job_tail`), so the whole run — session on disk included — finishes
//!   inside `SECS` rather than a few seconds past it. A harness with a hard cap should still
//!   pass a budget under its own (`bench/run_bench.py` uses `cap - 15 s`) as a safety net, but
//!   the tool no longer relies on that margin to produce a file.
//! * `SIGINT` / `SIGTERM` set the same flag from the handler, so the run unwinds through the
//!   same path and still emits a session. A *second* signal is left to the default action, so a
//!   caller can always insist.
//!
//! Both cases exit 4 whatever the board looks like: it says "this result is what the budget
//! bought", which is the one thing a harness cannot work out for itself.
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
//! (`batch-orchestration.md` §4).
//!
//! `-us/--threads N` is live now, and it does **not** parallelise the route pass: upstream 2.3.0
//! routes strictly sequentially, its multi-threaded pass is private with no call site
//! (conformance #107, #111), and concurrent connection routing (`ARCHITECTURE.md`'s T3) is not
//! written. What `--threads` buys is the two stages that are safely parallel — the batch
//! optimizer's candidate evaluation (T1) and the referee's violation scan (T2). It defaults to
//! `1`; see the flag's help text for why that is not the machine's core count.
//!
//! `--portfolio N` races N route passes with different item orders on board clones and keeps the
//! best referee score. Off by default. It buys quality at equal wall clock, never speed.
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
/// `--job-timeout` ran out or a signal arrived; the `.ses` was written anyway.
const EXIT_DEADLINE: i32 = 4;

/// `SIGINT`, on every unix.
const SIGINT: i32 = 2;
/// `SIGTERM`, on every unix.
const SIGTERM: i32 = 15;
/// `SIG_DFL`, the default disposition.
const SIG_DFL: usize = 0;

extern "C" {
    /// `signal(2)`. Declared rather than pulled in with a crate: this
    /// workspace has no third-party dependencies, and the two calls below are
    /// the whole of what a `libc` dependency would buy.
    fn signal(signum: i32, handler: usize) -> usize;
}

/// The handler. **Everything it does must be async-signal-safe**:
/// [`fr_core::request_job_stop`] is one relaxed atomic store, and `signal` and
/// `raise` are both on the POSIX safe list.
///
/// The first signal is a request: the router polls the flag between
/// connections, stops, and the normal path judges the board and writes the
/// session. The disposition is then reset, so a second signal does what the
/// caller plainly meant — a run wedged somewhere that does not poll (a very
/// long referee pass, say) must still be killable without `SIGKILL`.
extern "C" fn request_stop_on_signal(signum: i32) {
    fr_core::request_job_stop();
    unsafe {
        signal(signum, SIG_DFL);
    }
}

/// Installs [`request_stop_on_signal`] for `SIGINT` and `SIGTERM`.
fn install_signal_handlers() {
    let handler = request_stop_on_signal as extern "C" fn(i32) as usize;
    unsafe {
        signal(SIGINT, handler);
        signal(SIGTERM, handler);
    }
}

/// `--job-timeout SECS`, in seconds. `0` is a budget that is already spent —
/// documented, because "no timeout" would be the other plausible reading and
/// the difference is a 300-second bench run.
fn parse_job_timeout(value: &str) -> std::time::Duration {
    match value.parse::<f64>() {
        Ok(seconds) if seconds.is_finite() && seconds >= 0.0 => {
            std::time::Duration::from_secs_f64(seconds)
        }
        _ => fail(&format!(
            "--job-timeout {value}: expected a non-negative number of seconds \
             (0 arms a budget that is already spent)"
        )),
    }
}

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
    --fanout                    Run the SMD pin-escape pre-pass first (ON by default, matching
                                upstream DefaultSettings.fanout.enabled). Kept as an explicit
                                flag so scripts that already pass it stay valid.
    --no-fanout                 Skip the SMD pin-escape pre-pass.
    --no-optimizer              Skip the post-routing optimization stage (it is ON by default,
                                as it is upstream: router.optimizer.enabled defaults to true)
    --deterministic             Never read the wall clock for budget decisions: the
                                per-connection allowance is spent in effort-based virtual time
    --job-timeout <SECS>        Wall budget for the whole job (Default: none). When it runs out
                                the router stops like a stop rule does — best board restored,
                                .ses written, referee run — and the exit code is 4. The budget
                                covers that shutdown too: a board-sized reserve is taken off it
                                after the import, so the run ends inside SECS. 0 arms a budget
                                that is already spent.

--drc-report OPTIONS
    --unit <mm|mil|inch|um>     Report unit (Default: mm, which is what the oracles use)
    --router-overrides          Import with the routing-path post-load overrides instead of
                                the -drc ones (board_edge class synthesis on)
    --emit-boxes                Also write one BOX line per item to stderr
    --pin-frame-fix             Deprecated no-op; the importer now places pads in the pin frame

DIAGNOSTICS
    --settle-trace <FILE>       Write one `FRMETRIC settle` line per settled maze element, the
                                lab jar's `-Dfrlab.settle_trace=1` format (`1`/`stderr` = stderr)

PARALLELISM (see also `docs/ARCHITECTURE.md`, "Parallelism")
    -us, --threads <N>          Workers for the optimizer's candidate evaluation and the
                                referee's violation scan. NOT the route pass, which is
                                sequential upstream and here. `0` = pick from the machine.
                                (Default: 1 — the parallel optimizer is reproducible at a
                                fixed N but is not the same heuristic as the sequential one,
                                so it is opt-in rather than a silent default.)
    --portfolio <N>             Race N route passes with different item orders on board
                                clones and keep the best referee score. Arm 0 is always the
                                default order, so the result is never worse than N=1 on the
                                same budget. Costs N cores for the same wall clock.
                                (Default: 1)

    -v,  --version              Print version information
    -h,  --help                 Print help

EXIT CODES
    0  finished, nothing left unconnected   2  routed and written, incompletes remain
    1  usage / IO / import failure          4  job timeout or signal; the .ses was still written
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
    portfolio: Option<usize>,
    fanout: bool,
    no_optimizer: bool,
    deterministic: bool,
    job_timeout: Option<std::time::Duration>,
    settle_trace: Option<String>,
}

/// A non-negative count flag (`--threads`, `--portfolio`), rejected rather than
/// silently ignored when it is not one — the same rule `-mp` follows.
fn parse_count(flag: &str, value: &str) -> usize {
    match value.parse::<usize>() {
        Ok(count) => count,
        Err(_) => fail(&format!(
            "{flag} {value}: expected a non-negative whole number of workers"
        )),
    }
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
    // First thing of all, before a single argument is looked at: the promise is that this
    // binary never dies holding work it did not write, and the only window where that cannot
    // be honoured is the one before the handler exists. It is this short on purpose — an
    // operator's Ctrl-C is the commonest way to lose a routed board.
    install_signal_handlers();

    let argv: Vec<String> = env::args().collect();
    if argv.len() <= 1 {
        print_usage();
        process::exit(EXIT_ERROR);
    }

    let mut args = Args {
        unit: "mm".to_string(),
        // Match `DefaultSettings.fanout.enabled = true`. The library
        // `MazeOptions` default stays off so in-process tests keep the cheap
        // no-escape path; the drop-in CLI is the Java-compatible entry.
        fanout: true,
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
            "-us" | "--threads" => args.threads = Some(parse_count("--threads", &next!("-us"))),
            "--portfolio" => {
                args.portfolio = Some(parse_count("--portfolio", &next!("--portfolio")))
            }
            "--fanout" => args.fanout = true,
            "--no-fanout" => args.fanout = false,
            "--no-optimizer" => args.no_optimizer = true,
            "--deterministic" => args.deterministic = true,
            "--job-timeout" => args.job_timeout = Some(parse_job_timeout(&next!("--job-timeout"))),
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

    // Must be set before anything asks the sink, which resolves the environment once.
    if let Some(path) = &args.settle_trace {
        env::set_var("FR_SETTLE_TRACE", path);
    }
    // Armed before the file is even read, so the budget covers the import too — on the large
    // fixtures that is seconds, and a budget that only started at the router would overrun by
    // exactly them.
    if let Some(budget) = args.job_timeout {
        fr_core::arm_job_deadline(budget);
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

    // Where the session goes, decided *before* the run: the job writes it itself, before it
    // judges the board, so that a run killed at a harness cap still leaves one (see
    // `fr_core::pipeline`'s "The session is written before the board is judged").
    // `--import-only` writes a session only when one was asked for; the default path always
    // does, because a caller that passed no `-do` still expects `<input>.ses`.
    let session_path = if args.import_only {
        args.output.clone()
    } else {
        Some(
            args.output
                .clone()
                .unwrap_or_else(|| default_ses(&input_path)),
        )
    };

    // `--import-only` keeps the M1 contract: no router stage, so the board the referee judges
    // and the session carries is exactly the imported one.
    let job = if args.import_only {
        // No router stage, so nothing sets the job's referee budget from a
        // `MazeOptions`; `--threads` still applies to the referee's T2 scan,
        // which is the only parallel work an import-only run has.
        job.with_referee_threads(MazeOptions::default().with_threads(args.threads).threads())
    } else {
        let passes = args.max_passes.unwrap_or(1).max(1);
        let passes = i32::try_from(passes).unwrap_or(i32::MAX);
        job.with_stage(RouterStage::Maze(
            MazeOptions::with_passes(passes)
                .with_fanout(args.fanout)
                .with_optimizer(!args.no_optimizer)
                .with_deterministic(args.deterministic)
                .with_threads(args.threads)
                .with_portfolio(args.portfolio.unwrap_or(1)),
        ))
    };
    let job = match &session_path {
        Some(path) => job.with_session_path(path),
        None => job,
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

    // The file itself is already on disk — the job wrote it before the referee ran. This line
    // only reports it, and it stays where it always was in the output so that a harness parsing
    // stdout sees the same summary it always did.
    if let Some(path) = &session_path {
        println!("Exported Specctra SES session file: {path}");
    }

    if args.import_only {
        return;
    }
    if report.stats.incomplete_count > 0 {
        eprintln!(
            "{} of {} connections still incomplete",
            report.stats.incomplete_count, report.stats.max_connections
        );
    }
    // 4 outranks 2, and is checked after the session is on disk: it says "the budget ran out,
    // and *this* is the board it bought". The router's own stop reason is not what decides —
    // the budget can also be spent in the import, the optimizer or the referee, and a caller
    // that asked for one wants to know it was spent however the run ended.
    if fr_core::job_budget_spent() {
        // The reserve is named because it is the difference between "the budget was too short"
        // and "the budget was spent shutting down", and only the caller can act on either.
        eprintln!(
            "job budget spent — the session above is the best board the run held \
             ({} of {} connections incomplete; {:.2}s of the budget was reserved for the \
             shutdown)",
            report.stats.incomplete_count,
            report.stats.max_connections,
            report.tail_reserve.as_secs_f64(),
        );
        process::exit(EXIT_DEADLINE);
    }
    // The old contract, now that there is a router behind it: 0 when the board came out fully
    // connected, 2 when it did not. The referee's count is the one that decides, not the
    // router's own — they are computed by different code over the same board, and disagreeing
    // with the judge in our favour is the one failure mode a harness cannot detect.
    if report.stats.incomplete_count > 0 {
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
        JobError::Read(..) | JobError::Write(..) => error.to_string(),
    }
}

/// The three-or-four line run summary.
///
/// `started` is the whole invocation's clock. It is reported on its own line
/// and **not** on the "Imported" one, which says import and has to mean it: the
/// pre-existing line printed the end-to-end elapsed time there, so every routed
/// run claimed the importer had cost what the router did. `bench/run_bench.py`
/// reads `Total` for the tool time and falls back to the old line.
///
/// The same rule applies to the item counts on that line, and they were breaking
/// it the same way: they came from [`JobReport::census`], which is taken after
/// the router stage, so "Imported … 4 617 items" on a routed board counted
/// copper the importer never read. They now come from
/// [`JobReport::import_census`], and a routed run gets a second line for the
/// board it finished with — the delta between the two is the copper we added.
fn print_summary(report: &JobReport, started: Instant) {
    println!(
        "Imported {} in {:.2}s: {} layers, {} items ({} pins, {} traces, {} vias)",
        report.pcb_name,
        report.import_duration.as_secs_f64(),
        report.layer_count,
        report.import_census.total(),
        report.import_census.pins,
        report.import_census.traces,
        report.import_census.vias,
    );
    if let Some(router) = &report.router {
        println!(
            // Signed, because the delta is genuinely two-way: a pass that normalizes and
            // removes tails can leave a pre-routed board with *fewer* trace items than it
            // found (`Issue103-Board-Routed`, -1).
            "Board: {} items ({} pins, {} traces, {} vias); router delta {:+} traces, {:+} vias",
            report.census.total(),
            report.census.pins,
            report.census.traces,
            report.census.vias,
            report.census.traces as i64 - report.import_census.traces as i64,
            report.census.vias as i64 - report.import_census.vias as i64,
        );
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
        if let Some(portfolio) = &router.portfolio {
            println!(
                "Portfolio: {} arms, arm {} won ({}), incompletes per arm {:?}",
                portfolio.arms,
                portfolio.winner,
                if portfolio.shuffle_won {
                    "a shuffled order"
                } else {
                    "the default order"
                },
                portfolio.arm_incompletes,
            );
        }
        if let Some(optimizer) = &router.optimizer {
            println!(
                "Optimizer: {} passes, {} of {} items improved, score {:.2} -> {:.2}, stopped on {}",
                optimizer.passes,
                optimizer.items_improved,
                optimizer.items_optimized,
                optimizer.score_before,
                optimizer.score_after,
                optimizer.exit,
            );
        }
    }
    println!(
        "Referee: {} clearance violations, {} of {} connections incomplete, score {:.2}/1000",
        report.stats.clearance_violation_count,
        report.stats.incomplete_count,
        report.stats.max_connections,
        report.score.normalized,
    );
    println!("Total {:.2}s", started.elapsed().as_secs_f64());
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
