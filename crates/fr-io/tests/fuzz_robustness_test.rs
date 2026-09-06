//! Fuzzing and robustness test suite for Specctra DSN / SES parsers and serializers.
//!
//! Executes 100,000+ deterministic mutations against seed DSN/SES fixtures to ensure:
//! 1. No panics across any DsnReader / SpecctraReader / SesReader / SesWriter / GoldenDump paths.
//! 2. No infinite loops or resource deadlocks.
//! 3. Graceful error reporting (clear Result::Err / ReadOutcome::ParseError / ReadOutcome::NotADsnFile).
//! 4. Minimum regression unit tests for all discovered edge cases.

use std::path::PathBuf;

use fr_geometry::planar::IntPoint;
use fr_io::specctra::{
    ast::*,
    import, parse,
    ses_common::SesLibrary,
    ses_read::{read_session, SesTarget},
    ses_write::{write_session, SesInput},
    strip_session_copper,
    transform::{derive_scale_factor, ScaleFactor},
};

// =============================================================================================
// Seed discovery and fallback synthetic seeds
// =============================================================================================

fn find_fixture_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(env_dir) = std::env::var_os("FR_FIXTURE_DIR") {
        dirs.push(PathBuf::from(env_dir));
    }
    dirs.push(PathBuf::from("/tmp/freerouting-upstream/fixtures"));
    if let Some(home) = std::env::var_os("HOME") {
        let home_path = PathBuf::from(home);
        dirs.push(home_path.join("fr-ref/freerouting-upstream/fixtures"));
    }
    dirs.push(PathBuf::from("bench/fixtures-oos"));
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dirs.push(manifest_dir.join("../../bench/fixtures-oos"));
    dirs.push(manifest_dir.join("../../../bench/fixtures-oos"));
    dirs
}

fn collect_seed_files() -> Vec<(String, Vec<u8>)> {
    let mut seeds = Vec::new();
    let dirs = find_fixture_dirs();

    for dir in dirs {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("dsn") {
                    if let Ok(bytes) = std::fs::read(&path) {
                        let name = path.file_name().unwrap().to_string_lossy().to_string();
                        if !seeds.iter().any(|(n, _)| n == &name) {
                            seeds.push((name, bytes));
                        }
                    }
                }
            }
        }
    }

    // Always ensure at least base synthetic seeds are present
    if seeds.is_empty() {
        seeds.push((
            "synthetic_minimal.dsn".to_string(),
            MINIMAL_DSN.as_bytes().to_vec(),
        ));
        seeds.push((
            "synthetic_complex.dsn".to_string(),
            COMPLEX_DSN.as_bytes().to_vec(),
        ));
    }
    seeds
}

const MINIMAL_DSN: &str = r#"(pcb minimal
  (parser (string_quote ") (space_in_quoted_tokens on))
  (resolution um 10)
  (structure
    (layer F.Cu (type signal))
    (layer B.Cu (type signal))
    (boundary (rect pcb 0 0 10000 10000))
    (rule (width 200) (clearance 200))
  )
  (library)
  (network)
  (wiring)
)"#;

const COMPLEX_DSN: &str = r#"(pcb complex_test
  (parser (string_quote ") (host_cad "KiCad") (host_version "8.0"))
  (resolution mm 1000)
  (structure
    (layer F.Cu (type signal))
    (layer In1.Cu (type power))
    (layer B.Cu (type signal))
    (boundary (path pcb 0 0 0 50 0 50 50 0 50 0 0))
    (via "Via[0-2]_800:400_um")
    (rule (width 0.25) (clearance 0.2))
    (keepout "ko1" (rect signal 10 10 20 20))
    (plane GND (rect In1.Cu 0 0 50 50))
    (autoroute_settings
      (autoroute on)
      (postroute on)
      (vias on)
      (via_costs 50)
    )
  )
  (placement
    (component "R_0805"
      (place "R1" 15.5 20.0 front 90.0)
      (place "R2" 25.0 30.5 back 180.0)
    )
  )
  (library
    (padstack "Round[A]Pad_600_um"
      (shape (circle F.Cu 0.6))
      (shape (circle B.Cu 0.6))
      (attach on)
    )
    (padstack "Via[0-2]_800:400_um"
      (shape (circle F.Cu 0.8))
      (shape (circle In1.Cu 0.8))
      (shape (circle B.Cu 0.8))
      (attach on)
    )
    (image "R_0805"
      (pin "Round[A]Pad_600_um" 1 -1.0 0.0)
      (pin "Round[A]Pad_600_um" 2 1.0 0.0)
      (outline (path signal 0.1 -1.5 -1.0 1.5 -1.0 1.5 1.0 -1.5 1.0 -1.5 -1.0))
    )
  )
  (network
    (net "GND"
      (pins R1-1 R2-1)
    )
    (net "VCC"
      (pins R1-2 R2-2)
    )
    (class "power" "GND" "VCC"
      (circuit (use_via "Via[0-2]_800:400_um"))
      (rule (width 0.5) (clearance 0.3))
    )
  )
  (wiring
    (wire (path F.Cu 0.25 15.5 20.0 15.5 25.0) (net GND) (type protect))
    (via "Via[0-2]_800:400_um" 15.5 25.0 (net GND) (type protect))
  )
)"#;

// =============================================================================================
// Fast Deterministic PRNG and Mutation Engine
// =============================================================================================

struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Rng {
            state: if seed == 0 { 0x853c49e6748fea9b } else { seed },
        }
    }

    fn next_u64(&mut self) -> u64 {
        // SplitMix64
        self.state = self.state.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }

    fn next_range(&mut self, upper: usize) -> usize {
        if upper <= 1 {
            0
        } else {
            (self.next_u64() % (upper as u64)) as usize
        }
    }

    fn next_byte(&mut self) -> u8 {
        self.next_u64() as u8
    }

    fn choose<'a, T>(&mut self, slice: &'a [T]) -> &'a T {
        &slice[self.next_range(slice.len())]
    }
}

const EXTREME_NUMBERS: &[&str] = &[
    "NaN",
    "Infinity",
    "-Infinity",
    "+Infinity",
    "1e308",
    "-1e308",
    "1e-308",
    "-1e-308",
    "1e99999",
    "-1e99999",
    "0",
    "0.0",
    "-0.0",
    "+0.0",
    "0000000.0000000",
    "1e25",
    "33554432",
    "33554432.0",
    "-33554432.0",
    "2147483647",
    "-2147483648",
    "9223372036854775807",
    "-9223372036854775808",
    "999999999999999999999999999999999999999999",
    "-999999999999999999999999999999999999999999",
    "0.000000000000000000000000000000000000000001",
    "1.7976931348623157e+308",
    "2.2250738585072014e-308",
];

const UNICODE_IDENTIFIERS: &[&str] = &[
    "Паяльная_станция",
    "Соединители:PinSocket",
    "Предохранители:Цилиндр_5х20",
    "网络_GND_电源",
    "コネクタ_ピン",
    "لوحة_الكترونية",
    "Müller_Groß_100µm",
    "🚀_Trace_45°_⚡",
    "Net-~{RESET}_#1",
    "/Sheet/SubSheet/D+",
    "12V_HIGH",
    "007_BOND",
    "",
    "   ",
    "\"\"",
    "''",
];

fn mutate_bytes(seed: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mode = rng.next_range(12);
    let mut data = seed.to_vec();

    match mode {
        0 => {
            // Truncation
            if !data.is_empty() {
                let cut = rng.next_range(data.len());
                data.truncate(cut);
            }
        }
        1 => {
            // UTF-8 BOM injection
            data.insert(0, 0xBF);
            data.insert(0, 0xBB);
            data.insert(0, 0xEF);
        }
        2 => {
            // CRLF / CR / mixed newline mutation
            let s = String::from_utf8_lossy(&data).to_string();
            let sub_mode = rng.next_range(3);
            let replaced = match sub_mode {
                0 => s.replace('\n', "\r\n"),
                1 => s.replace('\n', "\r"),
                _ => s.replace("\r\n", "\n").replace('\n', "\r\n"),
            };
            data = replaced.into_bytes();
        }
        3 => {
            // Extreme number injection
            let num = *rng.choose(EXTREME_NUMBERS);
            let pos = rng.next_range(data.len().max(1));
            data.splice(pos..pos, num.as_bytes().iter().copied());
        }
        4 => {
            // Unicode / Non-ASCII identifier injection
            let ident = *rng.choose(UNICODE_IDENTIFIERS);
            let pos = rng.next_range(data.len().max(1));
            data.splice(pos..pos, ident.as_bytes().iter().copied());
        }
        5 => {
            // Scope deletion (structure, boundary, layer, network, wiring, library, placement)
            let s = String::from_utf8_lossy(&data).to_string();
            let targets = [
                "structure",
                "boundary",
                "layer",
                "network",
                "wiring",
                "library",
                "placement",
                "resolution",
                "parser",
                "autoroute_settings",
            ];
            let target = *rng.choose(&targets);
            if let Some(idx) = s.find(target) {
                let end = s[idx..].find(')').map(|p| idx + p + 1).unwrap_or(s.len());
                let mutated = format!("{}{}", &s[..idx], &s[end..]);
                data = mutated.into_bytes();
            }
        }
        6 => {
            // Scope duplication
            let s = String::from_utf8_lossy(&data).to_string();
            let targets = [
                "structure",
                "layer",
                "net",
                "wire",
                "via",
                "padstack",
                "image",
            ];
            let target = *rng.choose(&targets);
            if let Some(idx) = s.find(target) {
                let end = s[idx..].find(')').map(|p| idx + p + 1).unwrap_or(s.len());
                let scope_text = &s[idx..end];
                let mutated = format!("{}{}{}", &s[..end], scope_text, &s[end..]);
                data = mutated.into_bytes();
            }
        }
        7 => {
            // Parentheses corruption (unmatched parens, deep nesting)
            let sub = rng.next_range(4);
            match sub {
                0 => data.extend_from_slice(b"(((((((((((("),
                1 => data.extend_from_slice(b"))))))))))))"),
                2 => {
                    let pos = rng.next_range(data.len().max(1));
                    data.splice(pos..pos, b"((((((((((((".iter().copied());
                }
                _ => {
                    // Deep nesting up to depth 600
                    let depth = 600;
                    let mut deep = Vec::new();
                    deep.extend_from_slice(b"(pcb test ");
                    deep.extend(std::iter::repeat_n(b'(', depth));
                    deep.extend(std::iter::repeat_n(b')', depth));
                    deep.push(b')');
                    data = deep;
                }
            }
        }
        8 => {
            // Quote corruption (unclosed quotes, standalone quotes)
            let pos = rng.next_range(data.len().max(1));
            data.insert(pos, b'"');
        }
        9 => {
            // Random byte replacement / bitflip
            if !data.is_empty() {
                let mutations = rng.next_range(10) + 1;
                for _ in 0..mutations {
                    let idx = rng.next_range(data.len());
                    data[idx] = rng.next_byte();
                }
            }
        }
        10 => {
            // Random byte insertions
            let mutations = rng.next_range(10) + 1;
            for _ in 0..mutations {
                let idx = rng.next_range(data.len().max(1));
                data.insert(idx, rng.next_byte());
            }
        }
        _ => {
            // Random byte deletions
            if data.len() > 5 {
                let mutations = rng.next_range(10) + 1;
                for _ in 0..mutations {
                    if data.len() > 1 {
                        let idx = rng.next_range(data.len());
                        data.remove(idx);
                    }
                }
            }
        }
    }
    data
}

// =============================================================================================
// The 100,000 Fuzzing Test
// =============================================================================================

/// Bounded fuzzing run for standard test pipeline (2,000 iterations or 30s timeout).
#[test]
fn test_fuzz_bounded_mutations_no_panic() {
    run_fuzz_loop(2_000, 30);
}

/// Full 100,000 mutations fuzzing run (execute via `cargo test --release -- --ignored` or ci/run-local.sh --long).
#[test]
#[ignore = "long test: 100,000 fuzz mutations across all parsers & serializers"]
fn test_fuzz_100k_mutations_no_panic() {
    run_fuzz_loop(100_000, 3600);
}

fn run_fuzz_loop(total_iterations: usize, timeout_secs: u64) {
    let seeds = collect_seed_files();
    assert!(!seeds.is_empty(), "Need at least 1 seed fixture");

    let start_time = std::time::Instant::now();
    let max_duration = std::time::Duration::from_secs(timeout_secs);
    let mut rng = Rng::new(0x2026_0905_1337_cafe);

    let mut import_ok_count = 0usize;
    let mut import_err_count = 0usize;
    let mut ses_roundtrips = 0usize;
    let mut iterations_done = 0usize;

    for i in 0..total_iterations {
        if start_time.elapsed() >= max_duration {
            println!("Fuzzing reached time limit of {timeout_secs}s at iteration {i}");
            break;
        }

        let seed_idx = rng.next_range(seeds.len());
        let (ref _name, ref seed_bytes) = seeds[seed_idx];
        let mutated = mutate_bytes(seed_bytes, &mut rng);

        // 1. Specctra parse (AST-only)
        let _ = parse(&mutated);

        // 2. Specctra full import
        let import_result = import(&mutated);

        // 3. Strip session copper
        let stripped = strip_session_copper(&mutated);
        let _ = parse(&stripped);

        // 4. Pre-rewrite DsnReader if UTF-8
        if let Ok(s) = std::str::from_utf8(&mutated) {
            let _ = fr_io::parse_dsn(s);
        }

        match import_result {
            Ok(design) => {
                import_ok_count += 1;
                // If design parsed successfully, exercise SES writer and golden dump
                let session_str = write_session(&SesInput::from_design(&design, "fuzz_test.dsn"));
                assert!(!session_str.is_empty());

                let _ = fr_io::golden_dump::emit(&design, "fuzz_test.dsn");

                // Exercise SES reader
                if let Ok(mut target_design) = import(&stripped) {
                    let library = SesLibrary::from_design(&target_design);
                    let target = SesTarget::for_design(&mut target_design, &library);
                    let _ = read_session(session_str.as_bytes(), target);
                    ses_roundtrips += 1;
                }
            }
            Err(_) => {
                import_err_count += 1;
            }
        }

        // Periodically also fuzz SES reader with arbitrary bytes
        if i % 10 == 0 {
            if let Ok(mut template) = import(MINIMAL_DSN.as_bytes()) {
                let library = SesLibrary::from_design(&template);
                let target = SesTarget::for_design(&mut template, &library);
                let _ = read_session(&mutated, target);
            }
        }

        iterations_done += 1;
        if (i + 1) % 10_000 == 0 {
            println!(
                "Progress: {}/{} iterations in {:.1}s ({} valid, {} err, {} ses)",
                i + 1,
                total_iterations,
                start_time.elapsed().as_secs_f64(),
                import_ok_count,
                import_err_count,
                ses_roundtrips
            );
        }
    }

    println!(
        "Fuzzing complete: {}/{} iterations in {:.2}s. {} valid DSNs, {} rejected cleanly, {} SES roundtrips exercised.",
        iterations_done, total_iterations, start_time.elapsed().as_secs_f64(), import_ok_count, import_err_count, ses_roundtrips
    );
    assert_eq!(import_ok_count + import_err_count, iterations_done);
}

// =============================================================================================
// Minimum Regression Tests for Edge Cases & Fixed Panics
// =============================================================================================

#[test]
fn test_regression_infinite_loop_extreme_scale_factor() {
    // derive_scale_factor with Infinity and NaN must terminate immediately without hanging
    let factor_inf = derive_scale_factor([f64::INFINITY, 0.0, 1000.0, 1000.0], 100);
    assert_eq!(factor_inf, ScaleFactor::Degenerate);

    let factor_nan = derive_scale_factor([f64::NAN, 0.0, 1000.0, 1000.0], 100);
    assert_eq!(factor_nan, ScaleFactor::Degenerate);

    let factor_huge = derive_scale_factor([1e300, 1e300, 1e300, 1e300], 100);
    assert!(matches!(factor_huge, ScaleFactor::Ok(_)));
}

#[test]
fn test_regression_side_of_i64_overflow() {
    // Extreme coordinate differences spanning i32::MIN to i32::MAX
    let from = IntPoint::new(i32::MIN, i32::MIN);
    let to = IntPoint::new(i32::MAX, i32::MAX);
    let point = IntPoint::new(i32::MAX, i32::MIN);

    let corners = vec![from, to, point];
    let normalized = normalize_polygon_corners(&corners);
    assert!(!normalized.is_empty());
    let hull = convex_hull(&corners);
    assert!(!hull.is_empty());
}

#[test]
fn test_regression_utf8_bom_support() {
    let bom_dsn = format!("\u{feff}{MINIMAL_DSN}");
    let res = import(bom_dsn.as_bytes());
    assert!(res.is_ok(), "BOM-prefixed DSN should import cleanly");

    let doc = fr_io::parse_dsn(&bom_dsn);
    assert!(doc.is_ok(), "BOM-prefixed DsnReader should parse cleanly");
}

#[test]
fn test_regression_deeply_nested_sexpr() {
    // 2000 nested open parens should not stack overflow
    let mut deep = String::new();
    deep.push_str("(pcb deep ");
    for _ in 0..2000 {
        deep.push('(');
    }
    for _ in 0..2000 {
        deep.push(')');
    }
    deep.push(')');
    let _ = fr_io::parse_dsn(&deep);
    let _ = import(deep.as_bytes());
}

#[test]
fn test_regression_infinite_rotation_normalization() {
    let dsn = br#"(pcb rot_test
      (resolution um 10)
      (structure
        (layer F.Cu (type signal))
        (boundary (rect pcb 0 0 1000 1000))
      )
      (placement
        (component "P"
          (place "P1" 10 10 front 1e308)
          (place "P2" 20 20 front -1e308)
          (place "P3" 30 30 front 1e9999)
        )
      )
      (library
        (image "P")
      )
    )"#;
    let res = import(dsn);
    assert!(
        res.is_ok(),
        "Infinite and extreme rotation values must normalize without infinite loop"
    );
}

#[test]
fn test_regression_unterminated_quoted_string_at_eof() {
    let dsn = b"(pcb test (structure (boundary (rect pcb 0 0 100 100))) (wiring (wire (path \"";
    let _ = import(dsn);
    let _ = parse(dsn);
}

#[test]
fn test_regression_degenerate_wire_traces() {
    let dsn = br#"(pcb test
      (resolution um 10)
      (structure
        (layer F.Cu (type signal))
        (boundary (rect pcb 0 0 1000 1000))
      )
      (wiring
        (wire (path F.Cu 100 500 500 500 500))
        (wire (path F.Cu 100 500 500))
        (wire (path F.Cu 100))
      )
    )"#;
    let res = import(dsn);
    assert!(
        res.is_ok(),
        "Degenerate wire paths should be skipped with warnings"
    );
}
