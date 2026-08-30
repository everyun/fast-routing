//! The pass trace — one record per batch pass, for reading a stop rule's mind.
//!
//! Upstream prints a line per pass ("Auto-routing pass #12 … completed in 0.42
//! seconds with the score of 972.71 (2 unrouted and 2 violations)"), and every
//! stagnation question is answered by putting that sequence next to ours. Ours
//! prints one summary for the whole run, which cannot distinguish a router that
//! stopped for the same reason at the same pass from one that stopped for a
//! different reason forty times slower.
//!
//! So this emits one record per pass, in the same shape
//! [`crate::optimizer::trace`] and [`crate::rooms::trace`] use so one parser
//! reads any of them:
//!
//! ```text
//! FRPASS pass=12, ms=418, queued=7, routed=5, not_routed=2, skipped=0, undone=0, budget_hits=0, score=972.71, incomplete=2, violations=2, flat=0, last_best=961.35, global_best=972.71, best_pass=12, history=11/972.71, restored=false
//! FRPASS stop, pass=27, reason=GlobalStagnation, score=984.08, incomplete=1, restores=2
//! ```
//!
//! The three fields that decide every question in this module:
//!
//! * `flat` — the pass-local stagnation counter *after* this pass. A run whose
//!   `flat` keeps returning to 0 is not stagnating by upstream's definition; a
//!   run whose `flat` climbs to 10 stopped on
//!   [`StopReason::PassLocalStagnation`](crate::batch::StopReason::PassLocalStagnation).
//! * `best_pass` — what the global tracker is measuring `currentPass -
//!   passOfBestScore` from, i.e. the other stop rule's whole state.
//! * `history=len/max` — what the restore check compares against. A restore
//!   fires on strict `>` against `max`, and each one resets `flat`, which is how
//!   upstream reaches pass 27 on a board whose score last moved at pass 17.
//!
//! Off unless `FR_PASS_TRACE` is set:
//!
//! | `FR_PASS_TRACE` | sink |
//! |---|---|
//! | unset or `0` | disabled |
//! | `1` / `stderr` | standard error |
//! | anything else | that path, truncated on open |
//!
//! The record costs one `format!` per **pass**, not per connection, so unlike
//! the optimizer trace it does not measurably move the wall it reports. The
//! [`is_enabled`] gate is one relaxed load when it is off.

use std::fs::File;
use std::io::{BufWriter, Stderr, Write};
use std::sync::{Mutex, OnceLock};

/// Whether the trace is on. Resolved once, from the environment.
#[inline]
pub(crate) fn is_enabled() -> bool {
    sink().is_some()
}

/// Writes one record, prefix included. No-op when the trace is off.
pub(crate) fn line(record: &str) {
    let Some(sink) = sink() else {
        return;
    };
    let Ok(mut guard) = sink.lock() else {
        return;
    };
    let _ = writeln!(guard.writer(), "FRPASS {record}");
    // A `static` sink is never dropped, so nothing would flush the tail at
    // exit; a run that ends on a job timeout is exactly the run whose last
    // records matter most.
    let _ = guard.writer().flush();
}

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

/// `None` = disabled.
fn sink() -> Option<&'static Mutex<Sink>> {
    static SINK: OnceLock<Option<Mutex<Sink>>> = OnceLock::new();
    SINK.get_or_init(|| match std::env::var("FR_PASS_TRACE") {
        Err(_) => None,
        Ok(value) if value.is_empty() || value == "0" => None,
        Ok(value) if value == "1" || value == "stderr" => {
            Some(Mutex::new(Sink::Stderr(BufWriter::new(std::io::stderr()))))
        }
        Ok(path) => match File::create(&path) {
            Ok(file) => Some(Mutex::new(Sink::File(BufWriter::new(file)))),
            Err(error) => {
                eprintln!("fr-router: cannot open pass trace {path:?}: {error}");
                None
            }
        },
    })
    .as_ref()
}
