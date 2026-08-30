//! The optimizer trace — where one optimizer stage's wall clock actually went.
//!
//! The stage reports one number per run ("50 items, 0 improved"), which is
//! exactly the number that cannot tell a stage that visited 50 cheap items from
//! one that visited 50 items each of which re-routed a connection under
//! upstream's whole 100-second per-connection allowance. Both look the same from
//! outside and they differ by three orders of magnitude.
//!
//! So this emits three records, in the same shape [`crate::rooms::trace`] uses
//! so that one parser can read either:
//!
//! ```text
//! FROPT conn, pass=1, net=33, state=Routed, ripped=0, budget_ms=100000, used_ms=43239, hit_budget=false, incomplete=1->15
//! FROPT inner, inner=1, queued=2, routed=1, not_routed=0, skipped=1, budget_hits=0, incomplete=1->8, ms=43291
//! FROPT item, item=ItemId(4218), kind=drill, ripped=16, key_before_ms=0, reroute_ms=43299, key_after_ms=0, improved=false
//! ```
//!
//! Two of the fields carry most of the signal:
//!
//! * `incomplete=a->b` on a `conn` record — what routing **one** connection did
//!   to the whole board's open-connection count. On a saturated board the
//!   answer is not "one fewer": `Issue103-Board-Routed` reads `1->15`, i.e. the
//!   insertion shattered fourteen neighbours, and that is what makes the
//!   optimizer's inner escalation a treadmill.
//! * `budget_hits` —
//!   [`PassOutcome::connections_hitting_budget`](crate::batch::PassOutcome::connections_hitting_budget),
//!   how many of the pass's connections spent their whole allowance instead of
//!   finding an answer. A stage whose items all report `budget_hits=1` is not
//!   slow because it is doing a lot of useful work; it is slow because each item
//!   is paying a timeout.
//!
//! # The trace is not free, and it is not free in the column it reports
//!
//! Each `conn` record brackets the connection with two ratsnest counts, so a
//! traced run's `used_ms` / `ms` are larger than the same run's untraced ones by
//! that much. Compare traced runs with traced runs.
//!
//! Off unless `FR_OPT_TRACE` is set:
//!
//! | `FR_OPT_TRACE` | sink |
//! |---|---|
//! | unset or `0` | disabled |
//! | `1` / `stderr` | standard error |
//! | anything else | that path, truncated on open |
//!
//! A diagnostic mode: it formats one line per item and per inner pass, and the
//! [`is_enabled`] gate is one relaxed load when it is off. Never benchmark with
//! it on.

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
    let _ = writeln!(guard.writer(), "FROPT {record}");
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
    SINK.get_or_init(|| match std::env::var("FR_OPT_TRACE") {
        Err(_) => None,
        Ok(value) if value.is_empty() || value == "0" => None,
        Ok(value) if value == "1" || value == "stderr" => {
            Some(Mutex::new(Sink::Stderr(BufWriter::new(std::io::stderr()))))
        }
        Ok(path) => match File::create(&path) {
            Ok(file) => Some(Mutex::new(Sink::File(BufWriter::new(file)))),
            Err(error) => {
                eprintln!("fr-router: cannot open optimizer trace {path:?}: {error}");
                None
            }
        },
    })
    .as_ref()
}
