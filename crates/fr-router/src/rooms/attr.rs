//! Fine-grained timers for `complete_expansion_room`.
//!
//! Off unless `FR_ROOM_ATTR` is set, same contract as [`super::trace`]:
//!
//! | `FR_ROOM_ATTR` | sink |
//! |---|---|
//! | unset or `0` | disabled |
//! | `1` / `stderr` | standard error |
//! | anything else | that path, truncated on open |
//!
//! One `Instant::now` pair per timed region when on; nothing when off. This is
//! an attribution mode — never A/B with it armed. [`flush`] writes one
//! `FRATTR` summary line so a killed run still has totals.

use std::cell::Cell;
use std::fs::File;
use std::io::{BufWriter, Stderr, Write};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

thread_local! {
    static COMPLETE_ROOM_NS: Cell<u64> = const { Cell::new(0) };
    static COMPLETE_SHAPE_NS: Cell<u64> = const { Cell::new(0) };
    static COLLECT_NS: Cell<u64> = const { Cell::new(0) };
    static COMPLETION_SORT_NS: Cell<u64> = const { Cell::new(0) };
    static COLLECT_ROOMS_NS: Cell<u64> = const { Cell::new(0) };
    static COLLECT_ITEMS_NS: Cell<u64> = const { Cell::new(0) };
    static ROOM_WALK_NS: Cell<u64> = const { Cell::new(0) };
    static ROOM_LEAVES: Cell<u64> = const { Cell::new(0) };
    static ROOM_SURVIVORS: Cell<u64> = const { Cell::new(0) };
    static CARVE_NS: Cell<u64> = const { Cell::new(0) };
    static CARVE_SURVIVORS: Cell<u64> = const { Cell::new(0) };
    static RESTRAIN_NS: Cell<u64> = const { Cell::new(0) };
    static DOORS_NS: Cell<u64> = const { Cell::new(0) };
    static NEIGHBOURS_NS: Cell<u64> = const { Cell::new(0) };
    static COMPLETE_ROOMS: Cell<u64> = const { Cell::new(0) };
    static COMPLETE_SHAPES: Cell<u64> = const { Cell::new(0) };
    static COLLECT_OBSTACLES: Cell<u64> = const { Cell::new(0) };
    static CARVE_HITS: Cell<u64> = const { Cell::new(0) };
    static RESTRAINS: Cell<u64> = const { Cell::new(0) };
    static DOOR_CALCS: Cell<u64> = const { Cell::new(0) };
    static NEIGHBOUR_CALLS: Cell<u64> = const { Cell::new(0) };
    static SAT_CALLS: Cell<u64> = const { Cell::new(0) };
    static SAT_FALLBACKS: Cell<u64> = const { Cell::new(0) };
    static SAT_NS: Cell<u64> = const { Cell::new(0) };
}

/// Whether attribution is on. Resolved once, from the environment.
#[inline]
pub(crate) fn is_enabled() -> bool {
    sink().is_some()
}

/// Start a timed region. `None` when attribution is off, so the two clock
/// reads are not made at all on the production path.
#[inline]
pub(crate) fn start() -> Option<Instant> {
    is_enabled().then(Instant::now)
}

#[inline]
fn add(cell: &'static std::thread::LocalKey<Cell<u64>>, ns: u64) {
    cell.with(|c| c.set(c.get().saturating_add(ns)));
}

#[inline]
fn add_count(cell: &'static std::thread::LocalKey<Cell<u64>>, n: u64) {
    cell.with(|c| c.set(c.get().saturating_add(n)));
}

#[inline]
fn elapsed_ns(started: Option<Instant>) -> u64 {
    started
        .map(|t| u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

pub(crate) fn add_complete_room(started: Option<Instant>) {
    if started.is_none() {
        return;
    }
    add(&COMPLETE_ROOM_NS, elapsed_ns(started));
    add_count(&COMPLETE_ROOMS, 1);
}

pub(crate) fn add_complete_shape(started: Option<Instant>) {
    if started.is_none() {
        return;
    }
    add(&COMPLETE_SHAPE_NS, elapsed_ns(started));
    add_count(&COMPLETE_SHAPES, 1);
}

pub(crate) fn add_collect(started: Option<Instant>, obstacles: usize) {
    if started.is_none() {
        return;
    }
    add(&COLLECT_NS, elapsed_ns(started));
    add_count(&COLLECT_OBSTACLES, obstacles as u64);
}

pub(crate) fn add_completion_sort(started: Option<Instant>) {
    if started.is_none() {
        return;
    }
    add(&COMPLETION_SORT_NS, elapsed_ns(started));
}

pub(crate) fn add_collect_rooms(started: Option<Instant>) {
    if started.is_none() {
        return;
    }
    add(&COLLECT_ROOMS_NS, elapsed_ns(started));
}

pub(crate) fn add_collect_items(started: Option<Instant>) {
    if started.is_none() {
        return;
    }
    add(&COLLECT_ITEMS_NS, elapsed_ns(started));
}

pub(crate) fn add_room_walk(started: Option<Instant>, leaves: u64) {
    if started.is_none() {
        return;
    }
    add(&ROOM_WALK_NS, elapsed_ns(started));
    add_count(&ROOM_LEAVES, leaves);
}

pub(crate) fn add_room_survivors(n: u64) {
    if !is_enabled() {
        return;
    }
    add_count(&ROOM_SURVIVORS, n);
}

pub(crate) fn add_carve(started: Option<Instant>, hits: u64) {
    if started.is_none() {
        return;
    }
    add(&CARVE_NS, elapsed_ns(started));
    add_count(&CARVE_HITS, hits);
}

pub(crate) fn add_carve_survivor() {
    if !is_enabled() {
        return;
    }
    add_count(&CARVE_SURVIVORS, 1);
}

/// One call to the SAT overlap gate, and whether it fell back to the exact
/// construction.
pub(crate) fn add_sat_gate(fallback: bool) {
    if !is_enabled() {
        return;
    }
    add_count(&SAT_CALLS, 1);
    if fallback {
        add_count(&SAT_FALLBACKS, 1);
    }
}

/// Time the SAT gate itself.
pub(crate) fn add_sat_ns(started: Option<Instant>) {
    if started.is_none() {
        return;
    }
    add(&SAT_NS, elapsed_ns(started));
}

pub(crate) fn add_restrain(started: Option<Instant>) {
    if started.is_none() {
        return;
    }
    add(&RESTRAIN_NS, elapsed_ns(started));
    add_count(&RESTRAINS, 1);
}

pub(crate) fn add_doors(started: Option<Instant>) {
    if started.is_none() {
        return;
    }
    add(&DOORS_NS, elapsed_ns(started));
    add_count(&DOOR_CALCS, 1);
}

pub(crate) fn add_neighbours(started: Option<Instant>) {
    if started.is_none() {
        return;
    }
    add(&NEIGHBOURS_NS, elapsed_ns(started));
    add_count(&NEIGHBOUR_CALLS, 1);
}

/// One `FRATTR` summary of the calling thread's totals.
pub fn flush() {
    if !is_enabled() {
        return;
    }
    let line = format!(
        "FRATTR complete_room_ns={}, complete_shape_ns={}, collect_ns={}, \
         completion_sort_ns={}, collect_rooms_ns={}, collect_items_ns={}, \
         room_walk_ns={}, room_leaves={}, room_survivors={}, \
         carve_ns={}, restrain_ns={}, doors_ns={}, neighbours_ns={}, \
         complete_rooms={}, complete_shapes={}, collect_obstacles={}, \
         carve_survivors={}, carve_hits={}, restrains={}, door_calcs={}, neighbour_calls={}, \
         sat_calls={}, sat_fallbacks={}, sat_ns={}",
        COMPLETE_ROOM_NS.with(Cell::get),
        COMPLETE_SHAPE_NS.with(Cell::get),
        COLLECT_NS.with(Cell::get),
        COMPLETION_SORT_NS.with(Cell::get),
        COLLECT_ROOMS_NS.with(Cell::get),
        COLLECT_ITEMS_NS.with(Cell::get),
        ROOM_WALK_NS.with(Cell::get),
        ROOM_LEAVES.with(Cell::get),
        ROOM_SURVIVORS.with(Cell::get),
        CARVE_NS.with(Cell::get),
        RESTRAIN_NS.with(Cell::get),
        DOORS_NS.with(Cell::get),
        NEIGHBOURS_NS.with(Cell::get),
        COMPLETE_ROOMS.with(Cell::get),
        COMPLETE_SHAPES.with(Cell::get),
        COLLECT_OBSTACLES.with(Cell::get),
        CARVE_SURVIVORS.with(Cell::get),
        CARVE_HITS.with(Cell::get),
        RESTRAINS.with(Cell::get),
        DOOR_CALCS.with(Cell::get),
        NEIGHBOUR_CALLS.with(Cell::get),
        SAT_CALLS.with(Cell::get),
        SAT_FALLBACKS.with(Cell::get),
        SAT_NS.with(Cell::get),
    );
    write_line(&line);
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

fn write_line(record: &str) {
    let Some(sink) = sink() else {
        return;
    };
    let Ok(mut guard) = sink.lock() else {
        return;
    };
    let _ = writeln!(guard.writer(), "{record}");
    let _ = guard.writer().flush();
}

fn sink() -> Option<&'static Mutex<Sink>> {
    static SINK: OnceLock<Option<Mutex<Sink>>> = OnceLock::new();
    SINK.get_or_init(|| match std::env::var("FR_ROOM_ATTR") {
        Err(_) => None,
        Ok(value) if value.is_empty() || value == "0" => None,
        Ok(value) if value == "1" || value == "stderr" => {
            Some(Mutex::new(Sink::Stderr(BufWriter::new(std::io::stderr()))))
        }
        Ok(path) => match File::create(&path) {
            Ok(file) => Some(Mutex::new(Sink::File(BufWriter::new(file)))),
            Err(error) => {
                eprintln!("fr-router: cannot open room-attr sink {path:?}: {error}");
                None
            }
        },
    })
    .as_ref()
}
