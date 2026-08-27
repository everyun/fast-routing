//! The settle trace — our side of the M2 differential harness.
//!
//! The lab jar (`/tmp/fr-lab-release/freerouting-lab.jar`, built from the
//! release revision `2d4de019`) emits one line per settled maze element when it
//! is run with `-Dfrlab.settle_trace=1`:
//!
//! ```text
//! FRLAB settle <door_id> <section_no> <expansion_value> <sorting_value>
//! ```
//!
//! We emit the identical record with our own tool token, so that one parser and
//! one diff (`bench/tools/frlab_parse.py`) work on both sides:
//!
//! ```text
//! FRMETRIC settle <door_id> <section_no> <g> <f>
//! ```
//!
//! maze-engine.md §"Suggested Rust module cut": *"validate the Rust engine
//! against the Java one by diffing the settle order `(door_id, section, g, f)`
//! on identical boards with the deterministic tie-breaks enabled."* The first
//! two fields are the topological identity and must match exactly; `g`/`f` are
//! `f64` and are compared with a tolerance — ARCHITECTURE: f64 never decides
//! topology, so a pure `g`/`f` drift is a weaker signal than an order swap.
//!
//! Off unless `FR_SETTLE_TRACE` is set (mirroring `-Dfrlab.settle_trace`):
//!
//! | `FR_SETTLE_TRACE` | sink |
//! |---|---|
//! | unset or `0` | disabled — [`settle`] returns before formatting anything |
//! | `1` / `stderr` | standard error |
//! | anything else | treated as a file path, truncated on open |
//!
//! This is a diagnostic mode, not a production path: it formats and writes one
//! buffered line per expansion. Call [`flush`] before the process exits.

/// Records one settled maze element.
///
/// `door_id` and `section` are the topological identity of the settle;
/// `g` is the expansion value and `f` the sorting value of the frontier element
/// (`MazeListElement.expansion_value` / `.sorting_value`).
#[cfg(feature = "metrics")]
#[inline]
pub fn settle(door_id: u64, section: u32, g: f64, f: f64) {
    imp::settle(door_id, section, g, f);
}

/// Records one settled maze element. Compiled out without the `metrics`
/// feature — an empty `const fn`, see `tests/zero_overhead.rs`.
#[cfg(not(feature = "metrics"))]
#[inline(always)]
pub const fn settle(door_id: u64, section: u32, g: f64, f: f64) {
    imp::settle(door_id, section, g, f);
}

/// Whether the trace is on. `false` without the `metrics` feature.
#[cfg(feature = "metrics")]
#[must_use]
pub fn is_enabled() -> bool {
    imp::is_enabled()
}

/// Whether the trace is on. `false` without the `metrics` feature.
#[cfg(not(feature = "metrics"))]
#[must_use]
pub const fn is_enabled() -> bool {
    imp::is_enabled()
}

/// Flushes the trace sink. No-op when the trace is off.
#[cfg(feature = "metrics")]
pub fn flush() {
    imp::flush();
}

/// Flushes the trace sink. No-op when the trace is off.
#[cfg(not(feature = "metrics"))]
pub const fn flush() {
    imp::flush();
}

#[cfg(feature = "metrics")]
mod imp {
    use std::fs::File;
    use std::io::{BufWriter, Stderr, Write};
    use std::sync::{Mutex, OnceLock};

    enum Sink {
        Stderr(BufWriter<Stderr>),
        File(BufWriter<File>),
    }

    impl Sink {
        fn writer(&mut self) -> &mut dyn Write {
            match self {
                Self::Stderr(writer) => writer,
                Self::File(writer) => writer,
            }
        }
    }

    /// `None` = disabled. Resolved once, from the environment.
    fn sink() -> Option<&'static Mutex<Sink>> {
        static SINK: OnceLock<Option<Mutex<Sink>>> = OnceLock::new();
        SINK.get_or_init(|| match std::env::var("FR_SETTLE_TRACE") {
            Err(_) => None,
            Ok(value) if value.is_empty() || value == "0" => None,
            Ok(value) if value == "1" || value == "stderr" => {
                Some(Mutex::new(Sink::Stderr(BufWriter::new(std::io::stderr()))))
            }
            Ok(path) => match File::create(&path) {
                Ok(file) => Some(Mutex::new(Sink::File(BufWriter::new(file)))),
                Err(error) => {
                    eprintln!("fr-metrics: cannot open settle trace {path:?}: {error}");
                    None
                }
            },
        })
        .as_ref()
    }

    #[inline]
    pub fn is_enabled() -> bool {
        sink().is_some()
    }

    #[inline]
    pub fn settle(door_id: u64, section: u32, g: f64, f: f64) {
        let Some(sink) = sink() else {
            return;
        };
        let Ok(mut guard) = sink.lock() else {
            return;
        };
        // `{:?}` is the shortest round-trip form, so `0.0` stays `0.0` the way
        // `Double.toString` prints it; the parser compares numerically anyway.
        let _ = writeln!(guard.writer(), "FRMETRIC settle {door_id} {section} {g:?} {f:?}");
    }

    pub fn flush() {
        if let Some(sink) = sink() {
            if let Ok(mut guard) = sink.lock() {
                let _ = guard.writer().flush();
            }
        }
    }
}

#[cfg(not(feature = "metrics"))]
mod imp {
    #[inline(always)]
    pub const fn settle(_door_id: u64, _section: u32, _g: f64, _f: f64) {}

    #[must_use]
    pub const fn is_enabled() -> bool {
        false
    }

    pub const fn flush() {}
}
