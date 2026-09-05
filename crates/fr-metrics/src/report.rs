//! The `FRMETRIC` report: what the bench harness greps out of a router run.
//!
//! Format contract (also documented in `bench/tools/README.md`, and parsed by
//! `bench/tools/frlab_parse.py`): every line is
//!
//! ```text
//! FRMETRIC {"kind":"<kind>", …}
//! ```
//!
//! — the literal prefix [`FRMETRIC_PREFIX`], then exactly one JSON object on
//! exactly one line. Four kinds are emitted:
//!
//! | kind | fields |
//! |---|---|
//! | `counters` | one `u64` per [`Counter`](crate::Counter), by name |
//! | `stage` | `pass`, `stage`, `nanos`, `calls` |
//! | `pass` | `pass`, `unrouted`, `duration_ms`, `ripup_cost` |
//! | `alloc` | `total_bytes`, `peak_bytes`, `live_bytes`, `allocations`, `deallocations` |
//!
//! JSON is hand-rolled: the workspace has no serde dependency and this crate
//! stays dependency-free. Every key and every string value is a compile-time
//! `[a-z_]+` identifier (asserted in the `counters`/`timers` unit tests), and
//! every other value is a `u64`, so no escaping is possible or needed.

use std::fmt::Write as _;
use std::io::Write as _;

use crate::alloc_stats::AllocStats;
use crate::counters::{COUNTER_COUNT, COUNTER_NAMES};
use crate::timers::{PassRecord, StageRecord};

/// The prefix every report line carries, for `grep FRMETRIC`.
pub const FRMETRIC_PREFIX: &str = "FRMETRIC ";

/// A whole run's metrics: counters summed over threads, plus the stage and pass
/// histories and (optionally) allocation totals.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct MetricsReport {
    /// Counter totals indexed by `Counter as usize`.
    pub counters: [u64; COUNTER_COUNT],
    /// `(pass, stage)` timings, sorted by pass then stage.
    pub stages: Vec<StageRecord>,
    /// Upstream-shaped pass history, sorted by pass.
    pub passes: Vec<PassRecord>,
    /// Allocation totals; `None` unless the `alloc-stats` feature is on.
    pub alloc: Option<AllocStats>,
}

impl MetricsReport {
    /// Collects the current state of the process.
    ///
    /// Folds any pending stage deltas first, so calling it twice in a row yields
    /// the same report.
    #[must_use]
    pub fn collect() -> Self {
        let (stages, passes) = crate::timers::histories();
        Self {
            counters: crate::counters::snapshot(),
            stages,
            passes,
            alloc: crate::alloc_stats::stats(),
        }
    }

    /// True when every number in the report is zero — the normal state of a
    /// build without the `metrics` feature, and of a freshly
    /// [`reset`](crate::reset) process.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.counters.iter().all(|&value| value == 0)
            && self.stages.is_empty()
            && self.passes.is_empty()
            && self.alloc.unwrap_or_default() == AllocStats::default()
    }

    /// One counter's total.
    #[must_use]
    pub fn counter(&self, counter: crate::Counter) -> u64 {
        self.counters[counter as usize]
    }

    /// The whole report as a single-line JSON object.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut out = String::with_capacity(512);
        out.push_str("{\"counters\":");
        self.write_counters_object(&mut out);
        out.push_str(",\"stages\":[");
        for (index, record) in self.stages.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            write_stage_object(&mut out, record);
        }
        out.push_str("],\"passes\":[");
        for (index, record) in self.passes.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            write_pass_object(&mut out, record);
        }
        out.push(']');
        match &self.alloc {
            Some(alloc) => {
                out.push_str(",\"alloc\":");
                write_alloc_object(&mut out, alloc);
            }
            None => out.push_str(",\"alloc\":null"),
        }
        out.push('}');
        out
    }

    /// The report as `FRMETRIC …` lines, one JSON object each (no trailing
    /// newlines).
    #[must_use]
    pub fn to_frmetric_lines(&self) -> Vec<String> {
        let mut lines = Vec::with_capacity(2 + self.stages.len() + self.passes.len());

        let mut counters = String::with_capacity(384);
        counters.push_str(FRMETRIC_PREFIX);
        counters.push_str("{\"kind\":\"counters\",");
        self.write_counter_fields(&mut counters);
        counters.push('}');
        lines.push(counters);

        for record in &self.stages {
            let mut line = String::with_capacity(96);
            line.push_str(FRMETRIC_PREFIX);
            line.push_str("{\"kind\":\"stage\",");
            write_stage_fields(&mut line, record);
            line.push('}');
            lines.push(line);
        }

        for record in &self.passes {
            let mut line = String::with_capacity(96);
            line.push_str(FRMETRIC_PREFIX);
            line.push_str("{\"kind\":\"pass\",");
            write_pass_fields(&mut line, record);
            line.push('}');
            lines.push(line);
        }

        if let Some(alloc) = &self.alloc {
            let mut line = String::with_capacity(160);
            line.push_str(FRMETRIC_PREFIX);
            line.push_str("{\"kind\":\"alloc\",");
            write_alloc_fields(&mut line, alloc);
            line.push('}');
            lines.push(line);
        }

        lines
    }

    /// Writes [`to_frmetric_lines`](Self::to_frmetric_lines), newline-terminated.
    ///
    /// # Errors
    ///
    /// Propagates the sink's write errors.
    pub fn write_frmetric(&self, sink: &mut impl std::io::Write) -> std::io::Result<()> {
        for line in self.to_frmetric_lines() {
            writeln!(sink, "{line}")?;
        }
        Ok(())
    }

    fn write_counters_object(&self, out: &mut String) {
        out.push('{');
        self.write_counter_fields(out);
        out.push('}');
    }

    fn write_counter_fields(&self, out: &mut String) {
        for (index, name) in COUNTER_NAMES.into_iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            let _ = write!(out, "\"{name}\":{}", self.counters[index]);
        }
    }
}

fn write_stage_object(out: &mut String, record: &StageRecord) {
    out.push('{');
    write_stage_fields(out, record);
    out.push('}');
}

fn write_stage_fields(out: &mut String, record: &StageRecord) {
    let _ = write!(
        out,
        "\"pass\":{},\"stage\":\"{}\",\"nanos\":{},\"calls\":{}",
        record.pass,
        record.stage.name(),
        record.nanos,
        record.calls
    );
}

fn write_pass_object(out: &mut String, record: &PassRecord) {
    out.push('{');
    write_pass_fields(out, record);
    out.push('}');
}

fn write_pass_fields(out: &mut String, record: &PassRecord) {
    let _ = write!(
        out,
        "\"pass\":{},\"unrouted\":{},\"duration_ms\":{},\"ripup_cost\":{}",
        record.pass, record.unrouted, record.duration_ms, record.ripup_cost
    );
}

fn write_alloc_object(out: &mut String, alloc: &AllocStats) {
    out.push('{');
    write_alloc_fields(out, alloc);
    out.push('}');
}

fn write_alloc_fields(out: &mut String, alloc: &AllocStats) {
    let _ = write!(
        out,
        "\"total_bytes\":{},\"peak_bytes\":{},\"live_bytes\":{},\"allocations\":{},\"deallocations\":{}",
        alloc.total_bytes, alloc.peak_bytes, alloc.live_bytes, alloc.allocations, alloc.deallocations
    );
}

/// Collects and prints the report to stdout as `FRMETRIC` lines.
///
/// Cheap to leave in a release build: with the `metrics` feature off the report
/// is empty and only the counters line — all zeros — is printed.
pub fn emit_to_stdout() {
    let report = MetricsReport::collect();
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = report.write_frmetric(&mut lock);
    let _ = lock.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timers::Stage;
    use crate::Counter;

    fn sample() -> MetricsReport {
        let mut counters = [0u64; COUNTER_COUNT];
        counters[Counter::Expansions as usize] = 8718;
        counters[Counter::TreeQueries as usize] = 42;
        MetricsReport {
            counters,
            stages: vec![StageRecord {
                pass: 1,
                stage: Stage::Route,
                nanos: 1_020_000_000,
                calls: 1,
            }],
            passes: vec![PassRecord {
                pass: 1,
                unrouted: 2,
                duration_ms: 1020,
                ripup_cost: 100,
            }],
            alloc: Some(AllocStats {
                total_bytes: 1024,
                peak_bytes: 512,
                live_bytes: 256,
                allocations: 8,
                deallocations: 4,
            }),
        }
    }

    #[test]
    fn lines_are_prefixed_single_line_json() {
        for line in sample().to_frmetric_lines() {
            assert!(line.starts_with(FRMETRIC_PREFIX), "bad prefix: {line}");
            assert!(!line.contains('\n'));
            let body = &line[FRMETRIC_PREFIX.len()..];
            assert!(body.starts_with('{') && body.ends_with('}'), "{body}");
            assert!(body.contains("\"kind\":\""));
        }
    }

    #[test]
    fn one_line_per_record_plus_counters_and_alloc() {
        let lines = sample().to_frmetric_lines();
        assert_eq!(lines.len(), 4);
        assert!(lines[0].contains("\"kind\":\"counters\""));
        assert!(lines[0].contains("\"expansions\":8718"));
        assert!(lines[1].contains("\"kind\":\"stage\",\"pass\":1,\"stage\":\"route\""));
        assert!(lines[2].contains("\"kind\":\"pass\",\"pass\":1,\"unrouted\":2"));
        assert!(lines[3].contains("\"kind\":\"alloc\",\"total_bytes\":1024"));
    }

    #[test]
    fn whole_report_json_is_one_balanced_line() {
        let json = sample().to_json();
        assert!(!json.contains('\n'));
        let mut depth = 0i32;
        for byte in json.bytes() {
            match byte {
                b'{' | b'[' => depth += 1,
                b'}' | b']' => depth -= 1,
                _ => {}
            }
            assert!(depth >= 0, "unbalanced json: {json}");
        }
        assert_eq!(depth, 0, "unbalanced json: {json}");
        assert!(json.contains("\"counters\":{\"expansions\":8718"));
        assert!(json.contains("\"alloc\":{"));
    }

    #[test]
    fn empty_report_has_null_alloc_and_is_empty() {
        let report = MetricsReport::default();
        assert!(report.is_empty());
        assert!(report.to_json().contains("\"alloc\":null"));
        assert_eq!(report.to_frmetric_lines().len(), 1);
    }

    #[test]
    fn write_frmetric_terminates_every_line() {
        let mut buffer = Vec::new();
        sample()
            .write_frmetric(&mut buffer)
            .expect("write to a Vec");
        let text = String::from_utf8(buffer).expect("ascii json");
        assert!(text.ends_with('\n'));
        assert_eq!(text.lines().count(), 4);
    }
}
