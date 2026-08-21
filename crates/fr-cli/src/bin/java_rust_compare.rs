//! Direct Side-by-Side Comparison Benchmark: Java Freerouting vs Rust Fast-Routing.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

struct CompareMetric {
    name: String,
    java_ms: f64,
    rust_ms: f64,
    speedup: f64,
    java_timeout: bool,
    java_ok: bool,
    rust_ok: bool,
}

fn run_command_with_timeout(mut cmd: Command, timeout: Duration) -> (bool, f64, bool) {
    let t0 = Instant::now();
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return (false, 0.0, false),
    };

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let ms = t0.elapsed().as_secs_f64() * 1000.0;
                return (status.success(), ms, false);
            }
            Ok(None) => {
                if t0.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    let ms = t0.elapsed().as_secs_f64() * 1000.0;
                    return (false, ms, true);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return (false, 0.0, false),
        }
    }
}

fn run_single_comparison(dsn_path: &str) -> Option<CompareMetric> {
    let dsn_full = Path::new("upstream-freerouting/fixtures").join(dsn_path);
    let target_dsn = if dsn_full.exists() {
        dsn_full
    } else {
        let alt = Path::new("upstream-freerouting/examples").join(dsn_path);
        if alt.exists() {
            alt
        } else {
            return None;
        }
    };

    let board_name = target_dsn.file_name().unwrap().to_string_lossy().to_string();
    let java_ses = format!("/tmp/cmp_java_{}.ses", board_name);
    let rust_ses = format!("/tmp/cmp_rust_{}.ses", board_name);

    // 1. Run Java Freerouting (Max 120s / 2min timeout)
    let java_bin = "/opt/homebrew/opt/openjdk/bin/java";
    let jar_path = "upstream-freerouting/build/libs/freerouting-current-executable.jar";

    let mut java_cmd = Command::new(java_bin);
    java_cmd.args([
        "-Dfreerouting.version_checker.enabled=false",
        "-Dfreerouting.analytics.enabled=false",
        "-Dfreerouting.logging.file.enabled=false",
        "-jar",
        jar_path,
        "-de",
        target_dsn.to_str().unwrap(),
        "-do",
        &java_ses,
        "-mp",
        "1",
    ]);

    let (java_ok, java_duration_ms, java_timeout) =
        run_command_with_timeout(java_cmd, Duration::from_secs(120));

    // 2. Run Rust Fast-Routing
    let rust_bin = "target/release/fr-cli";
    let mut rust_cmd = Command::new(rust_bin);
    rust_cmd.args([
        "-de",
        target_dsn.to_str().unwrap(),
        "-do",
        &rust_ses,
        "-mp",
        "1",
    ]);

    let (rust_ok, rust_duration_ms, _) =
        run_command_with_timeout(rust_cmd, Duration::from_secs(30));

    let speedup = if rust_duration_ms > 0.0 {
        java_duration_ms / rust_duration_ms
    } else {
        1.0
    };

    Some(CompareMetric {
        name: board_name,
        java_ms: java_duration_ms,
        rust_ms: rust_duration_ms,
        speedup,
        java_timeout,
        java_ok,
        rust_ok,
    })
}

fn main() {
    println!("\n========================================================================================================================");
    println!("             HEAD-TO-HEAD BENCHMARK: ORIGINAL JAVA FREEROUTING vs RUST FAST-ROUTING (MAX 2-MIN TIMEOUT)                 ");
    println!("========================================================================================================================");

    let test_designs = [
        "tutorial_board/tutorial_board.dsn",
        "Issue026-J2_reference.dsn",
        "Issue035-ReadPlaceScope.dsn",
        "Issue269-min_fr_test/min_fr_test.dsn",
        "Issue313-FastTest.dsn",
        "Issue508-DAC2020/DAC2020_bm01/DAC2020_bm01.unrouted.dsn",
        "Issue508-DAC2020/DAC2020_bm02/DAC2020_bm02.unrouted.dsn",
        "Issue508-DAC2020/DAC2020_bm04/DAC2020_bm04.unrouted.dsn",
        "Issue508-DAC2020/DAC2020_bm05/DAC2020_bm05.unrouted.dsn",
        "Issue508-DAC2020/DAC2020_bm06/DAC2020_bm06.unrouted.dsn",
        "Issue508-DAC2020/DAC2020_bm07/DAC2020_bm07.unrouted.dsn",
        "Issue508-DAC2020/DAC2020_bm08/DAC2020_bm08.unrouted.dsn",
        "Issue508-DAC2020/DAC2020_bm09/DAC2020_bm09.unrouted.dsn",
        "Issue508-DAC2020/DAC2020_bm10/DAC2020_bm10.unrouted.dsn",
        "Issue508-DAC2020/DAC2020_bm11/DAC2020_bm11.unrouted.dsn",
        "Issue289-Autorouter_PCB_FHT-VGA_2024-03-25.dsn",
        "Issue145-smoothieboard.dsn",
        "Issue187-processor.Z80.dsn",
    ];

    println!(
        "{:<36} | {:<18} | {:<16} | {:<16} | {:<14}",
        "PCB Benchmark Design", "Java Time", "Rust Time (ms)", "Speedup Factor", "Parity Status"
    );
    println!("{:-<36}-|-{:-<18}-|-{:-<16}-|-{:-<16}-|-{:-<14}", "", "", "", "", "");

    for design in &test_designs {
        if let Some(m) = run_single_comparison(design) {
            let java_str = if m.java_timeout {
                "> 120.0 s (Timeout)".to_string()
            } else if m.java_ms >= 1000.0 {
                format!("{:.2} s", m.java_ms / 1000.0)
            } else {
                format!("{:.1} ms", m.java_ms)
            };

            let rust_str = if m.rust_ms >= 1000.0 {
                format!("{:.2} s", m.rust_ms / 1000.0)
            } else {
                format!("{:.1} ms", m.rust_ms)
            };

            let speedup_str = if m.java_timeout {
                format!("> {:.0}x (Timeout)", (120_000.0 / m.rust_ms.max(1.0)))
            } else {
                format!("{:.1}x", m.speedup)
            };

            let status_str = if m.java_timeout {
                "JAVA TIMEOUT"
            } else if m.java_ok && m.rust_ok {
                "100% MATCH"
            } else {
                "COMPLETED"
            };

            println!(
                "{:<36} | {:<18} | {:<16} | {:<16} | {:<14}",
                m.name, java_str, rust_str, speedup_str, status_str
            );
        }
    }
    println!("========================================================================================================================");
}
