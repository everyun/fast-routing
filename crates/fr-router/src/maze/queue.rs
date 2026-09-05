//! The maze frontier: a lazy-deletion binary heap over POD keys plus a
//! parallel payload arena.
//!
//! Upstream is a `TreeSet<MazeListElement>` ordered by
//! `sorting_value, expansion_value, door.get_id_no(), section_no_of_door`, with
//! ties on all four dropped as duplicates (`MazeListElement.java:81-113`; the
//! id tie-breaks are shipped in 2.3.0 — conformance appendix D1). Pops are
//! `it.next(); it.remove()`.
//!
//! `maze-engine.md` §13 asks for a `BinaryHeap` of 32-byte POD keys with lazy
//! deletion instead, and the payload (backtrack pointer, `shape_entry`, flags)
//! in a side arena addressed by push index. That is what this is. Two
//! consequences worth stating because they are behaviour, not just layout:
//!
//! * **No dedup.** The `TreeSet` silently dropped an element equal on all four
//!   keys. A heap cannot, so a duplicate is pushed and then discarded on pop
//!   because its section is already occupied — the same outcome, one extra pop.
//!   [`FrontierStats::duplicate_pops`] counts them.
//! * **Ordering is total and identical.** [`FrontierKey`] compares `f`, then
//!   `g`, then `object_id`, then `section`, all with the same signs; the
//!   `BinaryHeap` is a max-heap so [`FrontierKey`]'s `Ord` is reversed at the
//!   wrapper.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use fr_geometry::planar::FloatLine;

use crate::maze::nodes::Adjustment;
use crate::rooms::{ExpandableRef, RoomId};

/// The ordering key of one frontier element — 24 bytes, `Copy`, no payload.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrontierKey {
    /// `f = g + h`, upstream's `sorting_value`.
    pub f: f64,
    /// `g`, upstream's `expansion_value`.
    pub g: f64,
    /// The expandable object's arena id — upstream's `door.get_id_no()`, but a
    /// real monotonic id instead of the collidable `min·31 + max` hash
    /// (`ExpansionDoor.java:189-195`).
    pub object_id: u32,
    /// The section index inside that object.
    pub section: u16,
}

impl Eq for FrontierKey {}

impl Ord for FrontierKey {
    /// `MazeListElement.compareTo` (`:81-113`), all four keys, ascending.
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        if self.f < other.f {
            Ordering::Less
        } else if self.f > other.f {
            Ordering::Greater
        } else if self.g < other.g {
            Ordering::Less
        } else if self.g > other.g {
            Ordering::Greater
        } else {
            self.object_id
                .cmp(&other.object_id)
                .then_with(|| self.section.cmp(&other.section))
        }
    }
}

impl PartialOrd for FrontierKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A heap entry: the key, reversed for the max-heap, plus the payload slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HeapEntry {
    key: FrontierKey,
    payload: u32,
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reversed: `BinaryHeap` pops the maximum, we want the minimum.
        other
            .key
            .cmp(&self.key)
            // Payload index is the push order; it never decides against a
            // distinct key, and for identical keys it makes the pop order the
            // push order, which is what the `TreeSet`'s dedup effectively did.
            .then_with(|| other.payload.cmp(&self.payload))
    }
}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The non-POD half of a frontier element (`MazeListElement`'s remaining
/// fields), stored out of line so the heap moves 32 bytes per sift.
#[derive(Debug, Clone, Copy)]
pub struct FrontierPayload {
    /// The object being entered.
    pub door: ExpandableRef,
    /// The section of that object.
    pub section_no_of_door: u16,
    /// The predecessor object, `None` for a start seed.
    pub backtrack_door: Option<ExpandableRef>,
    /// The section of the predecessor.
    pub section_no_of_backtrack_door: u16,
    /// The room that will be expanded when this element is popped. `None` for
    /// a drill element, which expands specially.
    pub next_room: Option<RoomId>,
    /// Where on the door the path crosses; costs are midpoint to midpoint.
    pub shape_entry: FloatLine,
    /// The path so far rips an obstacle room.
    pub room_ripped: bool,
    /// Shove adjustment state.
    pub adjustment: Adjustment,
    /// Ripup/shove checks already done — the delayed-occupation requeue sets
    /// this and the re-pop short-circuits straight to door enumeration.
    pub already_checked: bool,
    /// Toll paid at this door; `> 0` only when this door caused the ripup.
    pub ripup_cost: i32,
}

/// One popped element: key plus payload, reassembled.
#[derive(Debug, Clone, Copy)]
pub struct FrontierElement {
    /// The ordering key.
    pub key: FrontierKey,
    /// The payload.
    pub payload: FrontierPayload,
}

impl FrontierElement {
    /// `expansion_value`.
    #[inline]
    pub fn g(&self) -> f64 {
        self.key.g
    }

    /// `sorting_value`.
    #[inline]
    pub fn f(&self) -> f64 {
        self.key.f
    }
}

/// Push/pop counts, for the metrics report and for the no-dedup note above.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrontierStats {
    /// Elements pushed.
    pub pushes: u64,
    /// Elements popped, including stale ones.
    pub pops: u64,
    /// Pops discarded because the section was already occupied.
    pub duplicate_pops: u64,
}

/// The lazy-deletion best-first frontier.
#[derive(Debug, Default)]
pub struct Frontier {
    heap: BinaryHeap<HeapEntry>,
    payloads: Vec<FrontierPayload>,
    stats: FrontierStats,
}

impl Frontier {
    /// An empty frontier.
    pub fn new() -> Frontier {
        Frontier::default()
    }

    /// Drops every queued element and its payloads, keeping the allocations.
    pub fn clear(&mut self) {
        self.heap.clear();
        self.payloads.clear();
        self.stats = FrontierStats::default();
    }

    /// Number of elements still queued (stale ones included).
    #[inline]
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    /// True when nothing is queued.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    /// Push counts so far.
    #[inline]
    pub fn stats(&self) -> FrontierStats {
        self.stats
    }

    /// Inserts one element.
    pub fn push(&mut self, key: FrontierKey, payload: FrontierPayload) {
        let started = crate::maze::attr::start();
        let slot = self.payloads.len() as u32;
        self.payloads.push(payload);
        self.heap.push(HeapEntry { key, payload: slot });
        self.stats.pushes += 1;
        fr_metrics::counters::bump::queue_pushes(1);
        crate::maze::attr::add_queue_push(started);
    }

    /// Pops the cheapest entry (key, payload index), or `None` when exhausted.
    #[inline]
    pub fn pop_raw(&mut self) -> Option<(FrontierKey, u32)> {
        let entry = self.heap.pop()?;
        self.stats.pops += 1;
        fr_metrics::counters::bump::queue_pops(1);
        Some((entry.key, entry.payload))
    }

    /// Returns a reference to the payload at `slot`.
    #[inline]
    pub fn payload(&self, slot: u32) -> &FrontierPayload {
        &self.payloads[slot as usize]
    }

    /// Pops the cheapest element, or `None` when the frontier is exhausted.
    ///
    /// Lazy deletion is the caller's job: `occupy_next_element` keeps popping
    /// until it finds an element whose section is unoccupied, calling
    /// [`note_stale_pop`](Self::note_stale_pop) for the ones it discards.
    pub fn pop(&mut self) -> Option<FrontierElement> {
        let (key, slot) = self.pop_raw()?;
        Some(FrontierElement {
            key,
            payload: self.payloads[slot as usize],
        })
    }

    /// Records that the last pop was discarded as stale.
    #[inline]
    pub fn note_stale_pop(&mut self) {
        self.stats.duplicate_pops += 1;
        fr_metrics::counters::bump::stale_pops(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rooms::{DoorId, ExpandableRef};
    use fr_geometry::planar::FloatPoint;

    fn payload(object: u32) -> FrontierPayload {
        FrontierPayload {
            door: ExpandableRef::Door(DoorId(object)),
            section_no_of_door: 0,
            backtrack_door: None,
            section_no_of_backtrack_door: 0,
            next_room: None,
            shape_entry: FloatLine::new(FloatPoint::new(0.0, 0.0), FloatPoint::new(0.0, 0.0)),
            room_ripped: false,
            adjustment: Adjustment::None,
            already_checked: false,
            ripup_cost: 0,
        }
    }

    fn key(f: f64, g: f64, object_id: u32, section: u16) -> FrontierKey {
        FrontierKey {
            f,
            g,
            object_id,
            section,
        }
    }

    #[test]
    fn pops_in_ascending_f_then_g_then_id_then_section() {
        let mut frontier = Frontier::new();
        frontier.push(key(10.0, 5.0, 7, 1), payload(7));
        frontier.push(key(10.0, 5.0, 7, 0), payload(7));
        frontier.push(key(10.0, 4.0, 9, 0), payload(9));
        frontier.push(key(9.0, 9.0, 1, 0), payload(1));
        frontier.push(key(10.0, 5.0, 3, 0), payload(3));

        let order: Vec<(f64, f64, u32, u16)> = std::iter::from_fn(|| frontier.pop())
            .map(|e| (e.key.f, e.key.g, e.key.object_id, e.key.section))
            .collect();
        assert_eq!(
            order,
            vec![
                (9.0, 9.0, 1, 0),
                (10.0, 4.0, 9, 0),
                (10.0, 5.0, 3, 0),
                (10.0, 5.0, 7, 0),
                (10.0, 5.0, 7, 1),
            ]
        );
    }

    #[test]
    fn identical_keys_pop_in_push_order() {
        let mut frontier = Frontier::new();
        let mut first = payload(4);
        first.section_no_of_backtrack_door = 11;
        let mut second = payload(4);
        second.section_no_of_backtrack_door = 22;
        frontier.push(key(1.0, 1.0, 4, 0), first);
        frontier.push(key(1.0, 1.0, 4, 0), second);
        assert_eq!(
            frontier.pop().unwrap().payload.section_no_of_backtrack_door,
            11
        );
        assert_eq!(
            frontier.pop().unwrap().payload.section_no_of_backtrack_door,
            22
        );
    }

    #[test]
    fn stats_track_pushes_pops_and_stale_pops() {
        let mut frontier = Frontier::new();
        frontier.push(key(1.0, 1.0, 1, 0), payload(1));
        frontier.push(key(2.0, 2.0, 2, 0), payload(2));
        frontier.pop();
        frontier.note_stale_pop();
        frontier.pop();
        let stats = frontier.stats();
        assert_eq!(stats.pushes, 2);
        assert_eq!(stats.pops, 2);
        assert_eq!(stats.duplicate_pops, 1);
    }
}
