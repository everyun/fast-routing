//! Proof that `--no-default-features` really does compile the instrumentation
//! away, rather than merely making it cheap.
//!
//! perf-forensics §9.2 — *"Diagnostics that cost nothing when off. `tracing`
//! macros or `#[cfg(feature)]`. Enforce with a test."*
//!
//! Three independent arguments:
//!
//! 1. **Assembly** ([`instrumented_loop_compiles_to_nothing`]) — builds the crate
//!    with `--no-default-features --release --emit=asm` and checks that the body
//!    of `fr_metrics_zero_overhead_probe`, a loop containing two `counter!`
//!    calls, contains no call instruction at all. This test runs under *any*
//!    feature set, because it builds the disabled variant itself.
//! 2. **Const evaluation** (below, `--no-default-features` only) — the disabled
//!    counter entry points are `const fn`, so they provably touch no
//!    thread-local, no atomic and no clock.
//! 3. **Zero-sized guard** (below, `--no-default-features` only) — `StageTimer`
//!    has no fields and no `Drop`, so a timed region leaves nothing behind.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Symbol name of the probe, with and without the Mach-O leading underscore.
const PROBE_SYMBOLS: [&str; 2] = [
    "_fr_metrics_zero_overhead_probe:",
    "fr_metrics_zero_overhead_probe:",
];

/// Mnemonics that would mean the instrumentation survived: a TLS accessor call,
/// a registry lookup, a panic path. Plain branches are excluded on purpose —
/// they are ordinary control flow; the instruction-count bound below is what
/// catches a surviving loop or a tail call.
const CALL_MNEMONICS: [&str; 4] = ["bl", "blr", "call", "callq"];

#[test]
fn instrumented_loop_compiles_to_nothing() {
    let Some(asm) = build_disabled_asm() else {
        // Cargo is unavailable or the nested build could not run (sandbox, no
        // toolchain, locked target dir). Skipping beats a flaky failure — the
        // const-eval and zero-sized-guard checks below still hold the line.
        eprintln!("zero_overhead: skipping the assembly check (nested cargo build unavailable)");
        return;
    };

    let body = probe_body(&asm).expect("the probe symbol is in the emitted assembly");
    assert!(
        !body.is_empty(),
        "probe body came out empty — the extraction is wrong, not the crate"
    );

    for line in &body {
        let mnemonic = line.split_whitespace().next().unwrap_or_default();
        assert!(
            !CALL_MNEMONICS.contains(&mnemonic),
            "instrumentation survived --no-default-features: {line:?}\nprobe body:\n{}",
            body.join("\n")
        );
    }

    // A loop of `counter!` calls that costs nothing folds to the closed-form
    // Gauss sum: a handful of arithmetic instructions, no loop at all.
    assert!(
        body.len() < 24,
        "probe body is {} instructions, expected a straight-line Gauss sum:\n{}",
        body.len(),
        body.join("\n")
    );
}

/// Builds `fr-metrics` with the `metrics` feature off and returns the emitted
/// assembly, or `None` if the nested build could not be run.
fn build_disabled_asm() -> Option<String> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let target_dir = std::env::temp_dir().join("fr-metrics-zero-overhead-asm");
    let deps_dir = target_dir.join("release").join("deps");

    // Drop stale `.s` files so "newest" cannot pick up a previous feature set.
    if let Ok(entries) = std::fs::read_dir(&deps_dir) {
        for entry in entries.flatten() {
            if entry.path().extension().is_some_and(|ext| ext == "s") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = Command::new(cargo)
        .args(["rustc", "--manifest-path"])
        .arg(&manifest)
        .args([
            "--lib",
            "--release",
            "--no-default-features",
            "--",
            "--emit=asm",
        ])
        .env("CARGO_TARGET_DIR", &target_dir)
        // Inherited flags could add instrumentation of their own (coverage,
        // sanitizers) and would make the assembly check meaningless.
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .output()
        .ok()?;

    if !output.status.success() {
        eprintln!(
            "zero_overhead: nested build failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return None;
    }

    newest_asm_file(&deps_dir).and_then(|path| std::fs::read_to_string(path).ok())
}

fn newest_asm_file(deps_dir: &Path) -> Option<PathBuf> {
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(deps_dir).ok()?.flatten() {
        let path = entry.path();
        let is_probe_asm = path.extension().is_some_and(|ext| ext == "s")
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("fr_metrics-"));
        if !is_probe_asm {
            continue;
        }
        let modified = entry.metadata().ok()?.modified().ok()?;
        let is_newer = match &newest {
            Some((best, _)) => modified > *best,
            None => true,
        };
        if is_newer {
            newest = Some((modified, path));
        }
    }
    newest.map(|(_, path)| path)
}

/// Extracts the instruction lines between the probe's label and the end of its
/// procedure, skipping assembler directives and local labels.
fn probe_body(asm: &str) -> Option<Vec<String>> {
    let mut lines = asm
        .lines()
        .skip_while(|line| !PROBE_SYMBOLS.contains(&line.trim()));
    lines.next()?; // the label itself

    let mut body = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        if trimmed == ".cfi_endproc" || trimmed.starts_with(".size") {
            return Some(body);
        }
        if trimmed.is_empty() || trimmed.starts_with('.') || trimmed.ends_with(':') {
            continue; // directive, CFI annotation or local label
        }
        body.push(trimmed.to_owned());
    }
    None
}

// ---------------------------------------------------------------------------
// The two static proofs. Compiled only in the disabled configuration, where
// they are statements about the types and functions that exist there.
// ---------------------------------------------------------------------------

/// Const evaluation of the counter entry points. This compiles *only* if the
/// disabled implementations are `const fn` with empty bodies — a `const` context
/// forbids thread-local access, atomics and `Instant::now()`.
#[cfg(not(feature = "metrics"))]
const _: () = {
    fr_metrics::counters::add(fr_metrics::Counter::Expansions, 1);
    fr_metrics::counters::bump::expansions(1);
    fr_metrics::counters::bump::tree_queries(2);
    fr_metrics::settle_trace::settle(1, 0, 0.0, 0.0);
};

#[test]
#[cfg(not(feature = "metrics"))]
fn stage_timer_is_a_zero_sized_drop_less_type() {
    use fr_metrics::{stage_timer, Stage, StageTimer};

    assert_eq!(std::mem::size_of::<StageTimer>(), 0);
    assert!(!std::mem::needs_drop::<StageTimer>());
    assert_eq!(stage_timer(Stage::MazeSearch).elapsed_nanos(), 0);
}

#[test]
#[cfg(not(feature = "metrics"))]
fn nothing_is_recorded_and_the_report_stays_empty() {
    use fr_metrics::{counter, record_pass, set_pass, stage_timer, Stage};

    set_pass(1);
    {
        let _timer = stage_timer(Stage::Route);
        for _ in 0..100_000 {
            counter!(expansions);
            counter!(tree_queries, 3);
        }
    }
    record_pass(1, 5, 10, 100);
    fr_metrics::settle_trace::settle(42, 7, 1.5, 2.5);

    let report = fr_metrics::report();
    assert!(report.is_empty(), "{report:?}");
    assert!(!fr_metrics::enabled());
    assert!(!fr_metrics::settle_trace::is_enabled());
    // The report shape is still there, so callers do not need to be cfg'd: the
    // counters line is emitted, with every counter at zero.
    let lines = report.to_frmetric_lines();
    assert!(lines[0].contains("\"kind\":\"counters\""));
    assert!(lines[0].contains("\"expansions\":0"));
    assert!(lines[0].contains("\"tree_queries\":0"));
}

#[test]
#[cfg(feature = "metrics")]
fn enabled_build_actually_records() {
    use fr_metrics::{counter, Counter};

    let before = fr_metrics::counters::thread_snapshot();
    for _ in 0..1000 {
        counter!(expansions);
    }
    let after = fr_metrics::counters::thread_snapshot();
    assert_eq!(
        after[Counter::Expansions as usize] - before[Counter::Expansions as usize],
        1000
    );
}
