//! The room trace — our side of the room-decomposition differential harness.
//!
//! The lab jar (`/tmp/fr-lab-release/freerouting-lab.jar`) emits, at
//! `--logging.console.level=TRACE`, one record per `complete_expansion_room`
//! call and one per room that survives door calculation
//! (`AutorouteEngine.java:394-500`):
//!
//! ```text
//! TRACE  COMPLETE_ROOM input, net=76, layer=0, room_bounds=…, contained_bounds=…, …
//! TRACE  COMPLETE_ROOM added, net=76, layer=0, bounds=[(496767,-451803)..(514153,-339621)]
//! ```
//!
//! We emit the same two records with our own tool token so a single parser
//! (`bench/tools/room_stats.py`) can turn either side into the same table —
//! rooms per connection, room area distribution, sliver count. That table is
//! the only direct read on *"does our free-space decomposition have the same
//! granularity as upstream's?"*, which every corner-density and pull-tight
//! number downstream is a function of.
//!
//! Off unless `FR_ROOM_TRACE` is set, mirroring
//! [`fr_metrics::settle_trace`]'s contract:
//!
//! | `FR_ROOM_TRACE` | sink |
//! |---|---|
//! | unset or `0` | disabled |
//! | `1` / `stderr` | standard error |
//! | anything else | that path, truncated on open |
//!
//! This is a diagnostic mode: it formats one line per completed room. Never
//! benchmark with it on.

use std::fs::File;
use std::io::{BufWriter, Stderr, Write};
use std::sync::{Mutex, OnceLock};

use fr_geometry::planar::Simplex;

/// Whether the trace is on. Resolved once, from the environment.
#[inline]
pub(crate) fn is_enabled() -> bool {
    sink().is_some()
}

/// `COMPLETE_ROOM input` — one per [`complete_expansion_room`] call that got
/// past the liveness gate.
///
/// [`complete_expansion_room`]: crate::engine::AutorouteEngine::complete_expansion_room
pub(crate) fn input(
    net: i32,
    layer: i32,
    room: &Simplex,
    contained: &Simplex,
    from_door: Option<&Simplex>,
) {
    write_line(&format!(
        "FRROOM input, net={net}, layer={layer}, room_bounds={}, contained_bounds={}, from_door_bounds={}",
        bounds_of(room),
        bounds_of(contained),
        match from_door {
            Some(shape) => bounds_of(shape),
            None => "null".to_string(),
        },
    ));
}

/// `COMPLETE_ROOM added` — one per room that survived door calculation and
/// joined the complete list.
pub(crate) fn added(net: i32, layer: i32, shape: &Simplex) {
    write_line(&format!(
        "FRROOM added, net={net}, layer={layer}, bounds={}, area={:.1}, corners={}",
        bounds_of(shape),
        shape.area(),
        shape.border_line_count(),
    ));
}

/// One `divide_large_room` split (`ShapeSearchTree.java:1057-1090`): how many
/// sections a board-sized room was cut into, and how many of those sections
/// still meet the contained shape.
///
/// The second number is the interesting one: upstream keeps a section whose
/// contained shape is *empty*, and lets it die later; anything we do to keep
/// such a section alive changes the room count on exactly the boards this
/// function exists for.
pub(crate) fn divide(sections: usize, with_contained: usize) {
    write_line(&format!(
        "FRROOM divide, sections={sections}, with_contained={with_contained}"
    ));
}

/// One `try_remove_edge` attempt and its verdict — the analogue of Java's
/// `ROOM_EDGE_REMOVE start` / `… applied` pair
/// (`SortedRoomNeighbours.java:348-380`).
///
/// This is the only mechanism that makes a completed room *maximal*, so the
/// applied-over-attempted ratio is the first thing to check when our rooms come
/// out smaller than upstream's.
pub(crate) fn edge(net: i32, layer: i32, candidates: usize, old_area: f64, new_area: f64) {
    write_line(&format!(
        "FRROOM edge, net={net}, layer={layer}, candidates={candidates}, \
         old_area={old_area:.1}, new_area={new_area:.1}, \
         applied={}",
        candidates == 1 && new_area > old_area,
    ));
}

/// One located connection: how many backtrack hops it crossed and how many
/// corners the locator put down.
///
/// The Java side of this record is `compare_trace_insert_segment_raw`
/// (`InsertFoundConnectionAlgo.java:161`), one line per inserted segment, so
/// `corners` here is the count `bench/tools/room_stats.py` reconstructs from a
/// run of those. That count is the number every pull-tight cost is multiplied
/// by, which is why it — not the room total — is the headline.
pub(crate) fn located(net: i32, hops: usize, corners: usize, items: usize) {
    write_line(&format!(
        "FRROOM located, net={net}, hops={hops}, corners={corners}, items={items}"
    ));
}

/// `describe_shape_bounds` (`AutorouteEngine.java`): `[(lx,ly)..(rx,uy)]`, or
/// `null` for a shape that has no bounding box.
fn bounds_of(shape: &Simplex) -> String {
    if shape.is_empty() {
        return "empty".to_string();
    }
    if !shape.is_bounded() {
        return "unbounded".to_string();
    }
    let b = shape.bounding_box();
    format!("[({},{})..({},{})]", b.ll.x, b.ll.y, b.ur.x, b.ur.y)
}

fn write_line(record: &str) {
    let Some(sink) = sink() else {
        return;
    };
    let Ok(mut guard) = sink.lock() else {
        return;
    };
    let _ = writeln!(guard.writer(), "{record}");
    // A `static` sink is never dropped, so nothing would flush the tail of the
    // buffer at exit and the last few thousand records would be lost. This is a
    // diagnostic mode — one `write` syscall per completed room is the cheapest
    // way to make every run's trace complete without a flush hook in the CLI.
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
    SINK.get_or_init(|| match std::env::var("FR_ROOM_TRACE") {
        Err(_) => None,
        Ok(value) if value.is_empty() || value == "0" => None,
        Ok(value) if value == "1" || value == "stderr" => {
            Some(Mutex::new(Sink::Stderr(BufWriter::new(std::io::stderr()))))
        }
        Ok(path) => match File::create(&path) {
            Ok(file) => Some(Mutex::new(Sink::File(BufWriter::new(file)))),
            Err(error) => {
                eprintln!("fr-router: cannot open room trace {path:?}: {error}");
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
