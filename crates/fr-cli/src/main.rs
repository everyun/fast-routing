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
//! `--import-only`, `--golden-dump` and `--drc-report` (and `-drc`) never route and always exit 0 on success:
//! their contract is "this is what the *input* looks like", and the gates depend on it.
//!
//! # Drop-in Freerouting 2.3.0 CLI compatibility
//!
//! Upstream flags like `-mt <threads>`, `-oit <percent>`, `-dr <rules>`, `-drc <file>`,
//! `-inc <classes>`, `-dct <ms>`, `-di <dir>`, `-l <locale>`, `-dl`, `-da`, `-ll <level>`,
//! `-host <tag>`, `--user_data_path=<dir>`, and `--router.<key>=<val>` are accepted for
//! drop-in toolchain compatibility (KiCad Freerouting plugin, batch scripts, etc.).

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
/// `--job-timeout` ran out or a signal arrived; the `.ses` was still written.
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

/// `--job-timeout SECS`, in seconds. `0` is a budget that is already spent.
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

/// Optimizer improvement threshold in percent (`-oit`), e.g. `1.0` -> `0.01`.
fn parse_oit(value: &str) -> f32 {
    match value.parse::<f32>() {
        Ok(pct) if pct.is_finite() && pct >= 0.0 => pct / 100.0,
        Ok(_) => 0.0,
        Err(_) => fail(&format!(
            "-oit {value}: expected a non-negative number for improvement threshold in percent"
        )),
    }
}

/// Optimizer / worker threads clamped `[0, 1024]` (`-mt`).
fn parse_threads_mt(value: &str) -> usize {
    match value.parse::<i64>() {
        Ok(t) if t < 0 => 0,
        Ok(t) if t > 1024 => 1024,
        Ok(t) => usize::try_from(t).unwrap_or(1024),
        Err(_) => fail(&format!(
            "-mt {value}: expected a whole number of threads (0..=1024)"
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
    -di <DIR>                   GUI input directory; resolves relative design file paths
    -dr <FILE>                  Rules sidecar (.rules) file path (rules in .dsn take precedence)

MODES (default: route)
    --import-only               Import and judge the board; write the .ses only if -do was given
    --golden-dump <FILE>        Emit the `fast-routing.golden-dump/1` model dump (gate G1.1)
    -drc, --drc-report [FILE]   Emit DRC violation JSON and exit without routing (gate G1.2a)

ROUTING OPTIONS
    -mp, --max-passes <N>       Ripup-and-reroute passes (Default: 1). Pass n rips at 100*n.
                                Must be >= 0; a negative value is an error (upstream runs it
                                uncapped instead — see docs/PARITY-DIVERGENCES.md)
    -oit <PCT>                  Optimizer improvement threshold in percent (Default: 1.0 = 1%)
    --fanout                    Run the SMD pin-escape pre-pass first (ON by default, matching
                                upstream DefaultSettings.fanout.enabled).
    --no-fanout                 Skip the SMD pin-escape pre-pass.
    --no-optimizer              Skip the post-routing optimization stage (ON by default).
    --deterministic             Never read the wall clock for budget decisions: the
                                per-connection allowance is spent in effort-based virtual time
    --job-timeout <SECS>        Wall budget for the whole job (Default: none). When it runs out
                                the router stops like a stop rule does — best board restored,
                                .ses written, referee run — and the exit code is 4.
    --router.<PROP>=<VAL>       Generic router settings setter (e.g. --router.max_passes=10,
                                --router.optimizer.enabled=false, --router.fanout.enabled=true)

PARALLELISM (see also `docs/ARCHITECTURE.md`, "Parallelism")
    -mt, --threads <N>          Workers for the optimizer's candidate evaluation and the
                                referee's violation scan. Clamped [0, 1024]. 0 = automatic.
                                (Default: 1)
    -us <STRATEGY|N>            Optimizer update strategy (greedy|global|hybrid) or thread count
    --portfolio <N>             Race N route passes with different item orders on board clones
                                and keep the best referee score. (Default: 1)

COMPATIBILITY & HEADLESS OPTIONS
    -inc <CLASSES>              Ignore net classes (comma-separated; headless no-op)
    -dct <MS>                   GUI dialog confirmation timeout in ms (headless no-op)
    -l <LOCALE>                 Locale setting (headless CLI output is English)
    -dl                         Disable file log (file logging is off by default)
    -da                         Disable analytics (telemetry-free)
    -ll <LEVEL>                 Console log level (DEBUG, INFO, WARN, ERROR)
    -host <TAG>                 Host tag for analytics
    -is <STRATEGY>              Item selection strategy (seq|rand|prio)
    -hr <RATIO>                 Optimizer hybrid ratio
    --user_data_path <DIR>      User data directory (stateless)

--drc-report OPTIONS
    --unit <mm|mil|inch|um>     Report unit (Default: mm, which is what the oracles use)
    --router-overrides          Import with routing-path post-load overrides
    --emit-boxes                Also write one BOX line per item to stderr
    --pin-frame-fix             Deprecated no-op

DIAGNOSTICS
    --settle-trace <FILE>       Write one `FRMETRIC settle` line per settled maze element

    -v,  --version              Print version information
    -h,  -help, --help          Print help

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
    rules_file: Option<String>,
    input_directory: Option<String>,
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
    improvement_threshold: Option<f32>,
    deterministic: bool,
    job_timeout: Option<std::time::Duration>,
    settle_trace: Option<String>,
    ignore_net_classes: Option<String>,
    dialog_timeout: Option<usize>,
    disable_file_logging: bool,
    disable_analytics: bool,
    locale: Option<String>,
    log_level: Option<String>,
    host: Option<String>,
    board_update_strategy: Option<String>,
    item_selection_strategy: Option<String>,
    hybrid_ratio: Option<String>,
}

/// A non-negative count flag (`--threads`, `--portfolio`, `-dct`), rejected rather than
/// silently ignored when it is not one.
fn parse_count(flag: &str, value: &str) -> usize {
    match value.parse::<usize>() {
        Ok(count) => count,
        Err(_) => fail(&format!(
            "{flag} {value}: expected a non-negative whole number"
        )),
    }
}

fn fail(message: &str) -> ! {
    eprintln!("fr-cli: {message}");
    process::exit(EXIT_ERROR)
}

/// `-mp <N>`, with the negative value rejected instead of silently ignored.
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
    install_signal_handlers();

    let argv: Vec<String> = env::args().collect();
    if argv.len() <= 1 {
        print_usage();
        process::exit(EXIT_ERROR);
    }

    let mut args = Args {
        unit: "mm".to_string(),
        fanout: true,
        ..Args::default()
    };
    let mut index = 1;

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
        let arg = &argv[index];

        // Handle key=value format for long options e.g. --router.max_passes=10, --user_data_path=/tmp, --compare-boards=...
        if let Some(rest) = arg.strip_prefix("--") {
            if let Some((key, val)) = rest.split_once('=') {
                if key == "user_data_path" {
                    index += 1;
                    continue;
                }
                if key == "compare-boards" {
                    fail("--compare-boards is unsupported; fast-routing is a headless router engine, not a board comparison GUI tool (use fr-check or kicad-cli instead)");
                }
                if let Some(prop) = key.strip_prefix("router.") {
                    match prop {
                        "max_passes" => args.max_passes = Some(parse_max_passes(val)),
                        "max_threads" => args.threads = Some(parse_threads_mt(val)),
                        "optimizer.max_threads" => args.threads = Some(parse_threads_mt(val)),
                        "optimizer.improvement_threshold" => {
                            args.improvement_threshold = Some(parse_oit(val))
                        }
                        "optimizer.enabled" => match val.to_lowercase().as_str() {
                            "false" | "0" | "off" => args.no_optimizer = true,
                            "true" | "1" | "on" => args.no_optimizer = false,
                            _ => fail(&format!(
                                "--router.optimizer.enabled={val}: expected boolean"
                            )),
                        },
                        "fanout.enabled" => match val.to_lowercase().as_str() {
                            "false" | "0" | "off" => args.fanout = false,
                            "true" | "1" | "on" => args.fanout = true,
                            _ => fail(&format!("--router.fanout.enabled={val}: expected boolean")),
                        },
                        "job_timeout" => {
                            args.job_timeout = Some(parse_job_timeout(val));
                        }
                        "enabled" => match val.to_lowercase().as_str() {
                            "false" | "0" | "off" => args.import_only = true,
                            "true" | "1" | "on" => {}
                            _ => fail(&format!("--router.enabled={val}: expected boolean")),
                        },
                        other => {
                            eprintln!("note: --router.{other}={val} accepted");
                        }
                    }
                    index += 1;
                    continue;
                }
                if let Some(prop) = key.strip_prefix("debug.") {
                    eprintln!("note: debug switch --debug.{prop}={val} parsed");
                    index += 1;
                    continue;
                }
                if key.starts_with("logging.")
                    || key.starts_with("gui.")
                    || key.starts_with("mcp_server.")
                    || key.starts_with("profile.")
                {
                    index += 1;
                    continue;
                }
            }
        }

        match argv[index].as_str() {
            "-de" | "--design" => {
                let first = next!("-de");
                let mut files = Vec::new();
                for part in first.split('+') {
                    let trimmed = part.trim();
                    if !trimmed.is_empty() {
                        files.push(trimmed.to_string());
                    }
                }
                while index + 1 < argv.len() && !argv[index + 1].starts_with('-') {
                    index += 1;
                    for part in argv[index].split('+') {
                        let trimmed = part.trim();
                        if !trimmed.is_empty() {
                            files.push(trimmed.to_string());
                        }
                    }
                }
                for file in files {
                    let lower = file.to_lowercase();
                    if lower.ends_with(".dsn") {
                        args.design = Some(file);
                    } else if lower.ends_with(".ses") || lower.ends_with(".json") {
                        if args.output.is_none() {
                            args.output = Some(file);
                        }
                    } else if lower.ends_with(".rules") {
                        args.rules_file = Some(file);
                    } else if args.design.is_none() {
                        args.design = Some(file);
                    }
                }
            }
            "-di" => {
                let dir_or_file = next!("-di");
                if dir_or_file.to_lowercase().ends_with(".dsn") && args.design.is_none() {
                    args.design = Some(dir_or_file);
                } else {
                    args.input_directory = Some(dir_or_file);
                }
            }
            "-do" | "--output" => args.output = Some(next!("-do")),
            "-dr" => {
                let rf = next!("-dr");
                args.rules_file = Some(rf);
                eprintln!(
                    "note: -dr sidecar rules file is accepted; rules in .dsn take precedence"
                );
            }
            "-drc" => {
                if index + 1 < argv.len() && !argv[index + 1].starts_with('-') {
                    index += 1;
                    args.drc_report = Some(argv[index].clone());
                } else {
                    args.drc_report = Some(String::new());
                }
                args.import_only = true;
            }
            "--drc-report" => args.drc_report = Some(next!("--drc-report")),
            "--golden-dump" => args.golden_dump = Some(next!("--golden-dump")),
            "--import-only" => args.import_only = true,
            "--unit" => args.unit = next!("--unit"),
            "--pin-frame-fix" => eprintln!(
                "note: --pin-frame-fix is a no-op and is deprecated; the importer now places \
                 pads in the pin frame itself (fr-io specctra::build::placed_pad_shapes)"
            ),
            "--router-overrides" => args.router_overrides = true,
            "--emit-boxes" => args.emit_boxes = true,
            "-mp" | "--max-passes" => args.max_passes = Some(parse_max_passes(&next!("-mp"))),
            "-mt" => args.threads = Some(parse_threads_mt(&next!("-mt"))),
            "-oit" => args.improvement_threshold = Some(parse_oit(&next!("-oit"))),
            "-us" => {
                let val = next!("-us");
                if let Ok(count) = val.parse::<usize>() {
                    args.threads = Some(count);
                } else {
                    let op = val.to_lowercase();
                    if matches!(op.as_str(), "greedy" | "global" | "hybrid") {
                        args.board_update_strategy = Some(op);
                    } else {
                        fail(&format!(
                            "-us {val}: expected worker count (e.g. 4) or board update strategy (greedy|global|hybrid)"
                        ));
                    }
                }
            }
            "--threads" => args.threads = Some(parse_count("--threads", &next!("--threads"))),
            "-is" => {
                let val = next!("-is");
                args.item_selection_strategy = Some(val);
            }
            "-hr" => {
                let val = next!("-hr");
                args.hybrid_ratio = Some(val);
            }
            "-inc" => {
                let val = next!("-inc");
                args.ignore_net_classes = Some(val);
            }
            "-dct" => {
                let val = next!("-dct");
                args.dialog_timeout = Some(parse_count("-dct", &val));
            }
            "-l" => {
                let val = next!("-l");
                args.locale = Some(val);
            }
            "-dl" => args.disable_file_logging = true,
            "-da" => args.disable_analytics = true,
            "-ll" => {
                let val = next!("-ll");
                args.log_level = Some(val);
            }
            "-host" => {
                let val = next!("-host");
                args.host = Some(val);
            }
            "--portfolio" => {
                args.portfolio = Some(parse_count("--portfolio", &next!("--portfolio")))
            }
            "--fanout" => args.fanout = true,
            "--no-fanout" => args.fanout = false,
            "--no-optimizer" => args.no_optimizer = true,
            "--deterministic" => args.deterministic = true,
            "--job-timeout" => args.job_timeout = Some(parse_job_timeout(&next!("--job-timeout"))),
            "--settle-trace" => args.settle_trace = Some(next!("--settle-trace")),
            "--user_data_path" => {
                let _dir = next!("--user_data_path");
            }
            "--compare-boards" => {
                fail("--compare-boards is unsupported; fast-routing is a headless router engine, not a board comparison GUI tool (use fr-check or kicad-cli instead)");
            }
            "-v" | "--version" => {
                println!("fast-routing v{}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "-h" | "-help" | "--help" => {
                print_usage();
                return;
            }
            other => fail(&format!("unknown argument '{other}' (try --help)")),
        }
        index += 1;
    }

    let Some(mut input_path) = args.design.clone() else {
        fail("missing required input design file (-de <file.dsn>)");
    };

    if let Some(dir) = &args.input_directory {
        let p = Path::new(&input_path);
        if !p.is_absolute() && !p.exists() {
            let combined = Path::new(dir).join(&input_path);
            if combined.exists() {
                input_path = combined.to_string_lossy().to_string();
            }
        }
    }

    if let Some(out) = &mut args.drc_report {
        if out.is_empty() {
            *out = default_drc(&input_path);
        }
    }

    if let Some(path) = &args.settle_trace {
        env::set_var("FR_SETTLE_TRACE", path);
    }
    if let Some(budget) = args.job_timeout {
        fr_core::arm_job_deadline(budget);
    }

    let job = match RoutingJob::from_path(&input_path) {
        Ok(job) => job,
        Err(error) => fail(&error.to_string()),
    };

    if args.golden_dump.is_some() || args.drc_report.is_some() {
        emit_reports(&job, &args, &input_path);
        return;
    }

    let session_path = if args.import_only {
        args.output.clone()
    } else {
        Some(
            args.output
                .clone()
                .unwrap_or_else(|| default_ses(&input_path)),
        )
    };

    let job = if args.import_only {
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
                .with_portfolio(args.portfolio.unwrap_or(1))
                .with_improvement_threshold(args.improvement_threshold),
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

    if std::env::var_os("FR_METRICS_REPORT").is_some() {
        for line in fr_metrics::report().to_frmetric_lines() {
            eprintln!("{line}");
        }
    }
    print_summary(&report, started);
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }

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
    if fr_core::job_budget_spent() {
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

/// `<input>.dsn` → `<input>.drc.json`; anything else gains `.drc.json`.
fn default_drc(input: &str) -> String {
    match input.strip_suffix(".dsn") {
        Some(stem) => format!("{stem}.drc.json"),
        None => format!("{input}.drc.json"),
    }
}

fn describe(error: &JobError) -> String {
    match error {
        JobError::Import(_) => format!("import failed: {error}"),
        JobError::Read(..) | JobError::Write(..) => error.to_string(),
    }
}

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
