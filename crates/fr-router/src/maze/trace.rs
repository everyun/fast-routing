//! The connection trace — where one maze attempt's wall actually goes.
//!
//! The lab jar reports a maze attempt as a per-pass expansion histogram
//! (`FRLAB expansions_conn n=… p50=… p99=… max=…`, `bench/tools/frlab_parse.py`),
//! and wave-4 read our side of it off a whole-run counter total. Neither can
//! answer the two questions a tail pass raises — *how many attempts fail, and
//! what does a failing one cost?* — because a counter total cannot be split by
//! outcome and a histogram cannot see the work that is not an expansion.
//!
//! So this emits one record per attempt, split at the first pop:
//!
//! ```text
//! FRCONN net=76, outcome=exhausted, start_items=3, start_rooms=6, init_rooms=41, init_us=2913, expansions=118, search_rooms=96, search_us=402
//! ```
//!
//! * `init_rooms` / `init_us` — rooms completed and time spent in
//!   `MazeSearchAlgo.init` (`:1213-1329`): the destination boxes, the start
//!   rooms and the frontier seeding.
//! * `expansions` / `search_rooms` / `search_us` — the `occupy_next_element`
//!   loop.
//! * `outcome` — `routed`, or the [`SearchFailure`](super::search::SearchFailure)
//!   that ended it.
//!
//! # What it said the first time it was run
//!
//! On `Issue039-bug-design` at the G2.1 budget (285 s, 740 attempts) it retired
//! the wave-4 premise that a tail pass burns its budget on failing searches
//! that should give up faster:
//!
//! * **`init` is 1.1 s of 274 s** — 0.4 % of the attempt, 3 rooms of ~354. The
//!   start-room completion is not the cost, and neither is anything else that
//!   happens before the first pop.
//! * **2 of 740 attempts fail the maze search at all.** The pass's
//!   `not_routed` column counts insertions that left no copper
//!   (`BatchRouter::discount_routes_that_left_no_copper`), not searches that
//!   found no path — so "420 ms per failing connection" was timing *successful*
//!   searches whose insertion was then discarded. There is no unreachability
//!   pruning to add here: the frontier is almost never exhausted, so it is not
//!   being over-explored before it gives up.
//! * What is left is a flat per-expansion cost — 116 µs against the lab jar's
//!   36 µs on the same board, at a *lower* expansion count per attempt (3188 vs
//!   3362) — and a `/usr/bin/sample` of the loop puts ~86 % of it in
//!   `complete_expansion_room`, i.e. in the free-space decomposition rather
//!   than in the maze.
//!
//! That sample needs reading with care, which is the other reason this record
//! exists: `MazeSearch::get_instance` is 2316 bytes long and the profile
//! attributes 15 343 of 29 015 samples to `get_instance + 3440`, an offset that
//! lands inside `occupy_next_element`. `init_us` is what settled it.
//!
//! Off unless `FR_CONN_TRACE` is set, on the contract
//! [`crate::rooms::trace`] and [`crate::batch::trace`] already use:
//!
//! | `FR_CONN_TRACE` | sink |
//! |---|---|
//! | unset or `0` | disabled |
//! | `1` / `stderr` | standard error |
//! | anything else | that path, truncated on open |
//!
//! One `format!` and one `write` per *connection*, not per expansion, so unlike
//! the settle trace it does not measurably move the wall it reports. The
//! [`is_enabled`] gate is one relaxed load when it is off, and the two
//! `Instant::now()` calls it guards are not made at all.

use std::fs::File;
use std::io::{BufWriter, Stderr, Write};
use std::sync::{Mutex, OnceLock};

/// One attempt's numbers, filled in as the attempt runs.
///
/// Constructed only when the trace is on; [`MazeSearch`](super::search::MazeSearch)
/// holds it as an `Option` so an untraced run carries one `None`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ConnRecord {
    /// The net being routed.
    pub net: i32,
    /// Start items handed to `init`.
    pub start_items: usize,
    /// Incomplete start rooms `init` created.
    pub start_rooms: usize,
    /// Complete rooms the room database gained during `init`.
    pub init_rooms: usize,
    /// Microseconds spent in `init`.
    pub init_us: u128,
}

/// `FRCONN` — one record per maze attempt, emitted when the attempt ends.
pub(crate) fn connection(
    record: &ConnRecord,
    outcome: &str,
    expansions: u64,
    search_rooms: usize,
    search_us: u128,
) {
    write_line(&format!(
        "FRCONN net={}, outcome={outcome}, start_items={}, start_rooms={}, \
         init_rooms={}, init_us={}, expansions={expansions}, \
         search_rooms={search_rooms}, search_us={search_us}",
        record.net, record.start_items, record.start_rooms, record.init_rooms, record.init_us,
    ));
}

/// Whether the trace is on. Resolved once, from the environment.
#[inline]
pub(crate) fn is_enabled() -> bool {
    sink().is_some()
}

fn write_line(record: &str) {
    let Some(sink) = sink() else {
        return;
    };
    let Ok(mut guard) = sink.lock() else {
        return;
    };
    let _ = writeln!(guard.writer(), "{record}");
    // A `static` sink is never dropped, so nothing flushes the tail at exit.
    // One `write` syscall per *connection* is far below the record's own noise
    // floor, and it makes a killed run's trace complete up to the kill — which
    // is the run a budget-bound board actually produces.
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
    SINK.get_or_init(|| match std::env::var("FR_CONN_TRACE") {
        Err(_) => None,
        Ok(value) if value.is_empty() || value == "0" => None,
        Ok(value) if value == "1" || value == "stderr" => {
            Some(Mutex::new(Sink::Stderr(BufWriter::new(std::io::stderr()))))
        }
        Ok(path) => match File::create(&path) {
            Ok(file) => Some(Mutex::new(Sink::File(BufWriter::new(file)))),
            Err(error) => {
                eprintln!("fr-router: cannot open connection trace {path:?}: {error}");
                None
            }
        },
    })
    .as_ref()
}

/// Flushes the trace sink. No-op when the trace is off.
pub fn flush() {
    if let Some(sink) = sink() {
        if let Ok(mut guard) = sink.lock() {
            let _ = guard.writer().flush();
        }
    }
}
