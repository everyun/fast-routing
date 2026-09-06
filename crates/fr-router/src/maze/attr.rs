//! Fine-grained timers for the maze search loop.
//!
//! Off unless `FR_MAZE_ATTR` is set, same contract as [`crate::rooms::attr`]:
//!
//! | `FR_MAZE_ATTR` | sink |
//! |---|---|
//! | unset or `0` | disabled |
//! | `1` / `stderr` | standard error |
//! | anything else | that path, truncated on open |
//!
//! One `Instant::now` pair per timed region when on; nothing when off. This is
//! an attribution mode — never A/B with it armed. [`flush`] writes one
//! `FRMAZE` summary line so a killed run still has totals.

use std::cell::Cell;
use std::fs::File;
use std::io::{BufWriter, Stderr, Write};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

thread_local! {
    static OCCUPY_NEXT_NS: Cell<u64> = const { Cell::new(0) };
    static OCCUPY_CALLS: Cell<u64> = const { Cell::new(0) };

    static QUEUE_POP_NS: Cell<u64> = const { Cell::new(0) };
    static QUEUE_POPS: Cell<u64> = const { Cell::new(0) };
    static QUEUE_STALE_POPS: Cell<u64> = const { Cell::new(0) };

    static QUEUE_PUSH_NS: Cell<u64> = const { Cell::new(0) };
    static QUEUE_PUSHES: Cell<u64> = const { Cell::new(0) };

    static ROOM_COMPLETE_NS: Cell<u64> = const { Cell::new(0) };
    static ROOM_COMPLETIONS: Cell<u64> = const { Cell::new(0) };

    static THICK_CHECK_NS: Cell<u64> = const { Cell::new(0) };

    static TARGET_DOORS_NS: Cell<u64> = const { Cell::new(0) };
    static TARGET_DOOR_CALLS: Cell<u64> = const { Cell::new(0) };

    static ROOM_DOORS_NS: Cell<u64> = const { Cell::new(0) };
    static ROOM_DOORS_CALLS: Cell<u64> = const { Cell::new(0) };

    static SECTION_SEGMENTS_NS: Cell<u64> = const { Cell::new(0) };
    static SECTION_SEGMENT_CALLS: Cell<u64> = const { Cell::new(0) };

    static EXPAND_DOOR_SECTION_NS: Cell<u64> = const { Cell::new(0) };
    static EXPAND_DOOR_SECTION_CALLS: Cell<u64> = const { Cell::new(0) };

    static HEURISTIC_NS: Cell<u64> = const { Cell::new(0) };
    static HEURISTIC_CALLS: Cell<u64> = const { Cell::new(0) };

    static DRILL_PAGES_NS: Cell<u64> = const { Cell::new(0) };
    static DRILL_PAGE_CALLS: Cell<u64> = const { Cell::new(0) };

    static DRILLS_OF_PAGE_NS: Cell<u64> = const { Cell::new(0) };
    static DRILLS_OF_PAGE_CALLS: Cell<u64> = const { Cell::new(0) };

    static EXPAND_DRILL_NS: Cell<u64> = const { Cell::new(0) };
    static EXPAND_DRILL_CALLS: Cell<u64> = const { Cell::new(0) };

    static OTHER_LAYERS_NS: Cell<u64> = const { Cell::new(0) };
    static OTHER_LAYERS_CALLS: Cell<u64> = const { Cell::new(0) };

    static CHECK_LAYER_VIA_NS: Cell<u64> = const { Cell::new(0) };
    static CHECK_LAYER_VIA_CALLS: Cell<u64> = const { Cell::new(0) };
    static CHECK_LAYER_VIA_FAILS: Cell<u64> = const { Cell::new(0) };

    static SHOVE_TRACE_NS: Cell<u64> = const { Cell::new(0) };
    static SHOVE_TRACE_CALLS: Cell<u64> = const { Cell::new(0) };
    static SHOVE_TRACE_FAILS: Cell<u64> = const { Cell::new(0) };
}

/// Whether attribution is on. Resolved once, from the environment.
#[inline]
pub(crate) fn is_enabled() -> bool {
    sink().is_some()
}

/// Start a timed region. `None` when attribution is off, so the clock
/// read is not made at all on the production path.
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

pub(crate) fn add_occupy_next(started: Option<Instant>) {
    if started.is_none() {
        return;
    }
    add(&OCCUPY_NEXT_NS, elapsed_ns(started));
    add_count(&OCCUPY_CALLS, 1);
}

pub(crate) fn add_queue_pop(started: Option<Instant>, is_stale: bool) {
    if started.is_none() {
        return;
    }
    add(&QUEUE_POP_NS, elapsed_ns(started));
    add_count(&QUEUE_POPS, 1);
    if is_stale {
        add_count(&QUEUE_STALE_POPS, 1);
    }
}

pub(crate) fn add_queue_push(started: Option<Instant>) {
    if started.is_none() {
        return;
    }
    add(&QUEUE_PUSH_NS, elapsed_ns(started));
    add_count(&QUEUE_PUSHES, 1);
}

pub(crate) fn add_room_complete(started: Option<Instant>) {
    if started.is_none() {
        return;
    }
    add(&ROOM_COMPLETE_NS, elapsed_ns(started));
    add_count(&ROOM_COMPLETIONS, 1);
}

pub(crate) fn add_thick_check(started: Option<Instant>) {
    if started.is_none() {
        return;
    }
    add(&THICK_CHECK_NS, elapsed_ns(started));
}

pub(crate) fn add_target_doors(started: Option<Instant>) {
    if started.is_none() {
        return;
    }
    add(&TARGET_DOORS_NS, elapsed_ns(started));
    add_count(&TARGET_DOOR_CALLS, 1);
}

pub(crate) fn add_room_doors(started: Option<Instant>) {
    if started.is_none() {
        return;
    }
    add(&ROOM_DOORS_NS, elapsed_ns(started));
    add_count(&ROOM_DOORS_CALLS, 1);
}

pub(crate) fn add_section_segments(started: Option<Instant>) {
    if started.is_none() {
        return;
    }
    add(&SECTION_SEGMENTS_NS, elapsed_ns(started));
    add_count(&SECTION_SEGMENT_CALLS, 1);
}

pub(crate) fn add_expand_door_section(started: Option<Instant>) {
    if started.is_none() {
        return;
    }
    add(&EXPAND_DOOR_SECTION_NS, elapsed_ns(started));
    add_count(&EXPAND_DOOR_SECTION_CALLS, 1);
}

pub(crate) fn add_heuristic(started: Option<Instant>) {
    if started.is_none() {
        return;
    }
    add(&HEURISTIC_NS, elapsed_ns(started));
    add_count(&HEURISTIC_CALLS, 1);
}

pub(crate) fn add_drill_pages(started: Option<Instant>) {
    if started.is_none() {
        return;
    }
    add(&DRILL_PAGES_NS, elapsed_ns(started));
    add_count(&DRILL_PAGE_CALLS, 1);
}

pub(crate) fn add_drills_of_page(started: Option<Instant>) {
    if started.is_none() {
        return;
    }
    add(&DRILLS_OF_PAGE_NS, elapsed_ns(started));
    add_count(&DRILLS_OF_PAGE_CALLS, 1);
}

pub(crate) fn add_expand_drill(started: Option<Instant>) {
    if started.is_none() {
        return;
    }
    add(&EXPAND_DRILL_NS, elapsed_ns(started));
    add_count(&EXPAND_DRILL_CALLS, 1);
}

pub(crate) fn add_other_layers(started: Option<Instant>) {
    if started.is_none() {
        return;
    }
    add(&OTHER_LAYERS_NS, elapsed_ns(started));
    add_count(&OTHER_LAYERS_CALLS, 1);
}

pub(crate) fn add_check_layer_via(started: Option<Instant>, success: bool) {
    if started.is_none() {
        return;
    }
    add(&CHECK_LAYER_VIA_NS, elapsed_ns(started));
    add_count(&CHECK_LAYER_VIA_CALLS, 1);
    if !success {
        add_count(&CHECK_LAYER_VIA_FAILS, 1);
    }
}

pub(crate) fn add_shove_trace(started: Option<Instant>, success: bool) {
    if started.is_none() {
        return;
    }
    add(&SHOVE_TRACE_NS, elapsed_ns(started));
    add_count(&SHOVE_TRACE_CALLS, 1);
    if !success {
        add_count(&SHOVE_TRACE_FAILS, 1);
    }
}

fn read(cell: &'static std::thread::LocalKey<Cell<u64>>) -> u64 {
    cell.with(|c| c.get())
}

/// Flush aggregated counters to the sink.
pub fn flush() {
    let Some(sink) = sink() else {
        return;
    };
    let occupy_ns = read(&OCCUPY_NEXT_NS);
    let occupy_calls = read(&OCCUPY_CALLS);
    let pop_ns = read(&QUEUE_POP_NS);
    let pops = read(&QUEUE_POPS);
    let stale_pops = read(&QUEUE_STALE_POPS);
    let push_ns = read(&QUEUE_PUSH_NS);
    let pushes = read(&QUEUE_PUSHES);
    let room_complete_ns = read(&ROOM_COMPLETE_NS);
    let room_completions = read(&ROOM_COMPLETIONS);
    let thick_check_ns = read(&THICK_CHECK_NS);
    let target_doors_ns = read(&TARGET_DOORS_NS);
    let room_doors_ns = read(&ROOM_DOORS_NS);
    let section_segments_ns = read(&SECTION_SEGMENTS_NS);
    let section_segments_calls = read(&SECTION_SEGMENT_CALLS);
    let expand_door_section_ns = read(&EXPAND_DOOR_SECTION_NS);
    let expand_door_section_calls = read(&EXPAND_DOOR_SECTION_CALLS);
    let heuristic_ns = read(&HEURISTIC_NS);
    let heuristic_calls = read(&HEURISTIC_CALLS);
    let drill_pages_ns = read(&DRILL_PAGES_NS);
    let drills_of_page_ns = read(&DRILLS_OF_PAGE_NS);
    let expand_drill_ns = read(&EXPAND_DRILL_NS);
    let other_layers_ns = read(&OTHER_LAYERS_NS);
    let check_layer_via_ns = read(&CHECK_LAYER_VIA_NS);
    let check_layer_via_calls = read(&CHECK_LAYER_VIA_CALLS);
    let check_layer_via_fails = read(&CHECK_LAYER_VIA_FAILS);
    let shove_trace_ns = read(&SHOVE_TRACE_NS);
    let shove_trace_calls = read(&SHOVE_TRACE_CALLS);
    let shove_trace_fails = read(&SHOVE_TRACE_FAILS);

    let summary = format!(
        "FRMAZE occupy_ns={occupy_ns} occupy_calls={occupy_calls} \
         queue_pop_ns={pop_ns} pops={pops} stale_pops={stale_pops} \
         queue_push_ns={push_ns} pushes={pushes} \
         room_complete_ns={room_complete_ns} room_completions={room_completions} \
         thick_check_ns={thick_check_ns} \
         target_doors_ns={target_doors_ns} \
         room_doors_ns={room_doors_ns} \
         section_segments_ns={section_segments_ns} section_segments_calls={section_segments_calls} \
         expand_door_section_ns={expand_door_section_ns} expand_door_section_calls={expand_door_section_calls} \
         heuristic_ns={heuristic_ns} heuristic_calls={heuristic_calls} \
         drill_pages_ns={drill_pages_ns} drills_of_page_ns={drills_of_page_ns} \
         expand_drill_ns={expand_drill_ns} other_layers_ns={other_layers_ns} \
         check_layer_via_ns={check_layer_via_ns} check_layer_via_calls={check_layer_via_calls} check_layer_via_fails={check_layer_via_fails} \
         shove_trace_ns={shove_trace_ns} shove_trace_calls={shove_trace_calls} shove_trace_fails={shove_trace_fails}\n"
    );

    let mut guard = match sink.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    let _ = match &mut *guard {
        Sink::Stderr(s) => s.write_all(summary.as_bytes()),
        Sink::File(f) => f.write_all(summary.as_bytes()).and_then(|_| f.flush()),
    };
}

enum Sink {
    Stderr(Stderr),
    File(BufWriter<File>),
}

fn sink() -> Option<&'static Mutex<Sink>> {
    static SINK: OnceLock<Option<Mutex<Sink>>> = OnceLock::new();
    SINK.get_or_init(|| {
        let val = std::env::var("FR_MAZE_ATTR").ok()?;
        if val.is_empty() || val == "0" {
            return None;
        }
        if val == "1" || val.eq_ignore_ascii_case("stderr") {
            return Some(Mutex::new(Sink::Stderr(std::io::stderr())));
        }
        let file = File::create(&val).ok()?;
        Some(Mutex::new(Sink::File(BufWriter::new(file))))
    })
    .as_ref()
}
