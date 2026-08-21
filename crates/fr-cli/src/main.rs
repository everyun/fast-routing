//! Drop-in command-line replacement for headless Freerouting.
//!
//! Supports standard Freerouting CLI flags:
//! - `-de <path>` / `--design <path>`: input .dsn file.
//! - `-do <path>` / `--output <path>`: output .ses file.
//! - `-mp <passes>` / `--max-passes <passes>`: maximum autorouter passes.
//! - `-us <strategy>` / `--thread-strategy`: thread allocation (multi-core default).
//! - `-v` / `--version`: print version.

use fr_autoroute::BatchRouterSettings;
use fr_core::RoutingJob;
use std::env;
use std::fs;
use std::process;
use std::time::Instant;

fn print_usage() {
    eprintln!(
        r#"fast-routing (Freerouting in Rust) v{}

USAGE:
    fast-routing -de <input.dsn> [OPTIONS]

OPTIONS:
    -de, --design <FILE>        Input Specctra .dsn design file path (Required)
    -do, --output <FILE>        Output Specctra .ses session file path (Default: <input>.ses)
    -mp, --max-passes <N>       Maximum autorouter passes (Default: 10)
    -us, --threads <N>          Parallel CPU threads for Rayon pool (Default: all logical cores)
    -v,  --version              Print version information
    -h,  --help                 Print help
"#,
        env!("CARGO_PKG_VERSION")
    );
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() <= 1 {
        print_usage();
        process::exit(1);
    }

    let mut input_dsn = None;
    let mut output_ses = None;
    let mut max_passes = 10;
    let mut threads = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-de" | "--design" | "-di" => {
                if i + 1 < args.len() {
                    input_dsn = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            "-do" | "--output" => {
                if i + 1 < args.len() {
                    output_ses = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            "-mp" | "--max-passes" => {
                if i + 1 < args.len() {
                    if let Ok(p) = args[i + 1].parse::<usize>() {
                        max_passes = p;
                    }
                    i += 1;
                }
            }
            "-us" | "--threads" => {
                if i + 1 < args.len() {
                    if let Ok(t) = args[i + 1].parse::<usize>() {
                        threads = Some(t);
                    }
                    i += 1;
                }
            }
            "-v" | "--version" => {
                println!("fast-routing v{}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "-h" | "--help" => {
                print_usage();
                return;
            }
            _ => {}
        }
        i += 1;
    }

    let input_path = match input_dsn {
        Some(path) => path,
        None => {
            eprintln!("Error: Missing required input design file (-de <file.dsn>)");
            process::exit(1);
        }
    };

    let ses_output_path = output_ses.unwrap_or_else(|| {
        if input_path.ends_with(".dsn") {
            format!("{}.ses", &input_path[..input_path.len() - 4])
        } else {
            format!("{}.ses", input_path)
        }
    });

    if let Some(num_threads) = threads {
        rayon::ThreadPoolBuilder::new()
            .num_threads(num_threads)
            .build_global()
            .ok();
        println!("Configured Rayon with {} worker threads.", num_threads);
    } else {
        println!(
            "Running with Rayon multi-core parallelism ({} logical CPU cores available).",
            rayon::current_num_threads()
        );
    }

    println!("Loading DSN: {}", input_path);
    let dsn_content = match fs::read_to_string(&input_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to read input file {}: {}", input_path, e);
            process::exit(1);
        }
    };

    let start_time = Instant::now();
    let mut job = RoutingJob::new(&dsn_content);
    job.router_settings = BatchRouterSettings {
        max_passes,
        ..Default::default()
    };

    println!("Starting multi-pass autorouter (max {} passes)...", max_passes);
    match job.execute() {
        Ok(result) => {
            let duration = start_time.elapsed();
            println!(
                "Routing complete in {:.2}s: {}/{} nets routed, {} vias, DRC violations: {}",
                duration.as_secs_f64(),
                result.statistics.unrouted_net_count == 0,
                result.statistics.unrouted_net_count,
                result.statistics.via_count,
                result.statistics.clearance_violation_count
            );

            if let Err(e) = fs::write(&ses_output_path, &result.ses_content) {
                eprintln!("Failed to write SES session file {}: {}", ses_output_path, e);
                process::exit(1);
            }
            println!("Exported Specctra SES session file: {}", ses_output_path);

            if !result.is_clean {
                eprintln!("Warning: Routing completed with remaining incompletes or violations.");
                process::exit(2);
            }
        }
        Err(e) => {
            eprintln!("Autoroute error: {}", e);
            process::exit(1);
        }
    }
}
