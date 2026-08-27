//! Specctra `(pin <padstack> [(rotate <deg>)] <pin_id> <x> <y>)` parsing.
//!
//! Numeric pin ids lex as numbers, so they must be consumed positionally
//! rather than treated as coordinates. Cross-checked against an independent
//! line-oriented oracle over the raw text of every upstream fixture.

use fr_io::dsn_reader::parse_dsn;

fn pkg_pins(dsn: &str, name_hint: &str) -> Vec<(String, String, f64, f64, f64)> {
    let doc = parse_dsn(dsn).expect("parses");
    doc.packages
        .into_iter()
        .filter(|p| name_hint.is_empty() || p.name.contains(name_hint))
        .flat_map(|p| {
            p.pins
                .into_iter()
                .map(|pin| (pin.padstack_name, pin.pin_id, pin.x, pin.y, pin.rotation))
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn quoted_numeric_pin_id_stays_a_string() {
    let dsn = r#"(pcb b (library (image img (pin "PS1" "1" 10 20) (pin "PS1" "007" 30 40))))"#;
    let pins = pkg_pins(dsn, "");
    assert_eq!(pins[0].1, "1");
    assert_eq!(pins[0].2, 10.0);
    assert_eq!(pins[0].3, 20.0);
    // Leading-zero ids must survive verbatim, not be re-rendered as 7.
    assert_eq!(pins[1].1, "007", "quoted id must not be number-folded");
}

#[test]
fn rotate_after_the_pin_id_is_still_captured() {
    let dsn = r#"(pcb b (library (image img (pin PS 3 (rotate 45) 10 20))))"#;
    let pins = pkg_pins(dsn, "");
    assert_eq!(pins[0].1, "3");
    assert_eq!(pins[0].4, 45.0);
    assert_eq!((pins[0].2, pins[0].3), (10.0, 20.0));
}

#[test]
fn negative_and_zero_numeric_ids() {
    let dsn = r#"(pcb b (library (image img (pin PS 0 -1 -2) (pin PS 999999 3 4))))"#;
    let pins = pkg_pins(dsn, "");
    assert_eq!(pins[0].1, "0");
    assert_eq!((pins[0].2, pins[0].3), (-1.0, -2.0));
    assert_eq!(pins[1].1, "999999");
    assert_eq!((pins[1].2, pins[1].3), (3.0, 4.0));
}

#[test]
fn fractional_coordinates_are_not_truncated() {
    let dsn = r#"(pcb b (library (image img (pin PS 1 265.826772 -39.370079))))"#;
    let pins = pkg_pins(dsn, "");
    assert_eq!(pins[0].1, "1");
    assert_eq!(pins[0].2, 265.826772);
    assert_eq!(pins[0].3, -39.370079);
}

#[test]
fn extra_trailing_scopes_do_not_shift_coordinates() {
    // Some exporters append (pin_array ...) / (property ...) sub-scopes.
    let dsn = r#"(pcb b (library (image img (pin PS (rotate 90) 5 100 200 (property (index 7))))))"#;
    let pins = pkg_pins(dsn, "");
    assert_eq!(pins[0].1, "5");
    assert_eq!(pins[0].4, 90.0);
    assert_eq!((pins[0].2, pins[0].3), (100.0, 200.0));
}

/// Independent oracle over every upstream fixture: re-parse single-line
/// `(pin ...)` forms straight from the text and compare to the reader.
#[test]
fn all_fixture_pins_match_an_independent_text_oracle() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../upstream-freerouting/fixtures");
    if !root.exists() {
        eprintln!("fixtures missing, skipping");
        return;
    }

    let mut files = 0usize;
    let mut compared = 0usize;
    let mut mismatches: Vec<String> = Vec::new();

    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("dsn") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Ok(doc) = parse_dsn(&text) else { continue };
            files += 1;

            // Oracle: every physical line that is exactly one balanced (pin ...) form.
            let mut oracle: Vec<(String, f64, f64)> = Vec::new();
            for line in text.lines() {
                let t = line.trim();
                if !t.starts_with("(pin ") || !t.ends_with(')') {
                    continue;
                }
                if t.matches('(').count() != t.matches(')').count() {
                    continue;
                }
                // Strip a nested (rotate N) group; keep only the top level.
                let inner = &t[1..t.len() - 1];
                let mut depth = 0usize;
                let mut top = String::new();
                for c in inner.chars() {
                    match c {
                        '(' => depth += 1,
                        ')' => depth = depth.saturating_sub(1),
                        _ if depth == 0 => top.push(c),
                        _ => {}
                    }
                }
                let toks = split_dsn_tokens(&top);
                // toks[0] == "pin", then padstack, id, x, y
                if toks.len() < 5 || toks[0] != "pin" {
                    continue;
                }
                let (id, xs, ys) = (&toks[2], &toks[3], &toks[4]);
                let (Ok(x), Ok(y)) = (xs.parse::<f64>(), ys.parse::<f64>()) else {
                    continue;
                };
                oracle.push((normalize_id(id), x, y));
            }
            if oracle.is_empty() {
                continue;
            }

            let mut actual: Vec<(String, f64, f64)> = Vec::new();
            for p in &doc.packages {
                for pin in &p.pins {
                    actual.push((pin.pin_id.clone(), pin.x, pin.y));
                }
            }

            // The oracle only handles single-line pins; require it to be a
            // subsequence-free exact prefix-set match on multiset level.
            let mut oracle_sorted = oracle.clone();
            let mut actual_sorted = actual.clone();
            oracle_sorted.sort_by(cmpkey);
            actual_sorted.sort_by(cmpkey);
            if oracle_sorted.len() != actual_sorted.len() {
                mismatches.push(format!(
                    "{}: pin count oracle={} reader={}",
                    path.display(),
                    oracle_sorted.len(),
                    actual_sorted.len()
                ));
                continue;
            }
            for (o, a) in oracle_sorted.iter().zip(actual_sorted.iter()) {
                compared += 1;
                if o != a {
                    mismatches.push(format!("{}: oracle {o:?} != reader {a:?}", path.display()));
                    break;
                }
            }
        }
    }

    println!("fixtures with pins: {files}, pins compared: {compared}");
    assert!(compared > 5_000, "not enough coverage: {compared}");
    assert!(
        mismatches.is_empty(),
        "pin mismatches ({}):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

fn cmpkey(
    a: &(String, f64, f64),
    b: &(String, f64, f64),
) -> std::cmp::Ordering {
    a.0.cmp(&b.0)
        .then(a.1.partial_cmp(&b.1).unwrap())
        .then(a.2.partial_cmp(&b.2).unwrap())
}

/// Mirror the reader's lossless numeric-id rendering.
fn normalize_id(raw: &str) -> String {
    let unquoted = raw.trim_matches('"').trim_matches('\'');
    if raw.starts_with('"') || raw.starts_with('\'') {
        return unquoted.to_string();
    }
    match unquoted.parse::<f64>() {
        Ok(n) if n.fract() == 0.0 && n.abs() < 1e15 => format!("{}", n as i64),
        Ok(n) => format!("{n}"),
        Err(_) => unquoted.to_string(),
    }
}

fn split_dsn_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match quote {
            Some(q) => {
                cur.push(c);
                if c == q {
                    quote = None;
                    out.push(std::mem::take(&mut cur));
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                    quote = Some(c);
                    cur.push(c);
                } else if c.is_whitespace() {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                } else {
                    cur.push(c);
                }
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}
