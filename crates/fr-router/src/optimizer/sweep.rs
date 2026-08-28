//! The optimizer's item enumeration — `BatchOptimizer.ReadSortedRouteItems`
//! (release `autoroute/BatchOptimizer.java:464-558`, conformance row **#81**:
//! "character-for-character the same algorithm" in v19 and 2.3.0).
//!
//! It is **not** an iterator over a snapshot. The board mutates under it — every
//! item it hands out is about to be ripped out and re-routed — so it keeps only
//! a cursor `(x, y, layer)` and re-scans the whole item list on every call,
//! answering "the smallest key strictly greater than the cursor". Upstream calls
//! that a deliberate simplicity/correctness tradeoff; the price is O(N) per call
//! and O(N²) per pass (`batch-orchestration.md` §7.1).
//!
//! # The order
//!
//! Ascending `(x, y, layer)` of a *representative corner*:
//!
//! | class | key | filter |
//! |---|---|---|
//! | via | centre, `first_layer()` | not user-fixed |
//! | trace | `max(first_corner, last_corner)` by `(x, y)`, its layer | not shove-fixed, and not touching a non-user-fixed via |
//!
//! Two full scans per call, **vias first and traces second**, each keeping the
//! strictly-smaller candidate. Because the trace scan runs afterwards and its
//! test is a strict `<`, a trace whose key equals a via's key never displaces
//! it: "read traces last to prefer vias to traces at the same location"
//! (`:509`). The skip of traces contacting a live via is the same idea one hop
//! out — the via will drag its traces along when it is re-routed.
//!
//! # Determinism
//!
//! The scans are `ItemStore::iter`, i.e. descending id, which is upstream's
//! `UndoableObjects` order (see [`crate::batch::autoroute_items`]). Combined
//! with the strict `<` tests, an exact key tie is resolved by "whoever the scan
//! saw first", so the sweep is a pure function of the board. There is no id
//! tie-break, and adding one would change the visit order away from the
//! release's.
//!
//! The key carries `f64` coordinates because upstream's does
//! (`FloatPoint`), and they come from integer or exact-rational corners through
//! `Point::to_float`. They order the *visit sequence*; no geometry, no topology
//! and no keep/rollback decision is taken from them.

use fr_board::board::Board;
use fr_board::item::{ItemId, ItemKind};

use crate::batch::{normal_contacts, ContactScratch};

/// The sweep key: a representative corner and a layer.
///
/// `ReadSortedRouteItems` keeps this pair as `(FloatPoint minItemCoor, int
/// minItemLayer)` and compares it inline in four places; naming it lets the
/// comparison be written once and tested.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SweepKey {
    /// Representative corner, x.
    pub x: f64,
    /// Representative corner, y.
    pub y: f64,
    /// A via's `first_layer()`, or a trace's layer.
    pub layer: i32,
}

impl SweepKey {
    /// The cursor's initial value: `FloatPoint(Integer.MIN_VALUE,
    /// Integer.MIN_VALUE)` with layer `-1` (`:472-473`). Every real item is
    /// after it, including one at the origin on layer 0.
    pub fn before_everything() -> SweepKey {
        SweepKey {
            x: f64::from(i32::MIN),
            y: f64::from(i32::MIN),
            layer: -1,
        }
    }

    /// The per-call running minimum's initial value: `FloatPoint(
    /// Integer.MAX_VALUE, Integer.MAX_VALUE)` with layer `Integer.MAX_VALUE`
    /// (`:479-480`).
    pub fn after_everything() -> SweepKey {
        SweepKey {
            x: f64::from(i32::MAX),
            y: f64::from(i32::MAX),
            layer: i32::MAX,
        }
    }

    /// `self` is strictly after `cursor` — upstream's
    /// `x > min.x || x == min.x && (y > min.y || y == min.y && layer > minLayer)`
    /// (`:490-493`, `:534-537`).
    pub fn is_after(&self, cursor: &SweepKey) -> bool {
        self.x > cursor.x
            || self.x == cursor.x
                && (self.y > cursor.y || self.y == cursor.y && self.layer > cursor.layer)
    }

    /// `self` is strictly before `best` — upstream's mirrored test
    /// (`:494-497`, `:538-541`). Ties are `false`, which is what makes the
    /// first item the scan sees win, and what makes vias beat traces.
    pub fn is_before(&self, best: &SweepKey) -> bool {
        self.x < best.x
            || self.x == best.x && (self.y < best.y || self.y == best.y && self.layer < best.layer)
    }
}

/// The stateful cursor. One instance per optimizer pass (`:218`), dropped at the
/// end of it (`:279`).
#[derive(Debug, Clone)]
pub struct SortedRouteItems {
    cursor: SweepKey,
}

impl Default for SortedRouteItems {
    fn default() -> Self {
        SortedRouteItems::new()
    }
}

impl SortedRouteItems {
    /// A cursor positioned before every item.
    pub fn new() -> SortedRouteItems {
        SortedRouteItems {
            cursor: SweepKey::before_everything(),
        }
    }

    /// `get_current_position()` (`:553-555`) — where the optimizer is working,
    /// for progress reporting.
    pub fn current_position(&self) -> SweepKey {
        self.cursor
    }

    /// `next()` (`:476-551`): the smallest key strictly after the cursor, vias
    /// preferred at equal keys, or `None` when the sweep is exhausted.
    ///
    /// The cursor advances to the returned item's key — **or to
    /// [`SweepKey::after_everything`] when nothing was found**, which is
    /// upstream's `min_item_coor = curr_min_coor` at `:548` running
    /// unconditionally. A second `next()` after exhaustion therefore also
    /// returns `None`, and the sweep is not restartable.
    ///
    /// Two items with an identical key are visited once per pass: the second is
    /// never strictly after the cursor. That is upstream's behaviour and it is
    /// harmless — the first one's reroute covers the same connection.
    pub fn next(&mut self, board: &Board, scratch: &mut ContactScratch) -> Option<ItemId> {
        let mut result: Option<ItemId> = None;
        let mut best = SweepKey::after_everything();

        // Scan 1 — vias (`:481-508`).
        for (id, item) in board.store().iter() {
            let ItemKind::Via(via) = &item.kind else {
                continue;
            };
            if item.is_user_fixed() {
                continue;
            }
            let key = SweepKey {
                x: f64::from(via.center.x),
                y: f64::from(via.center.y),
                layer: item.first_layer(),
            };
            if key.is_after(&self.cursor) && key.is_before(&best) {
                best = key;
                result = Some(id);
            }
        }

        // Scan 2 — traces (`:509-547`). Read last so that a trace can never
        // displace a via with the same key.
        let mut contacts = Vec::new();
        for (id, item) in board.store().iter() {
            let ItemKind::Trace(trace) = &item.kind else {
                continue;
            };
            if item.is_shove_fixed() {
                continue;
            }
            let first = trace.first_corner().to_float();
            let last = trace.last_corner().to_float();
            // `:518-524` — the lexicographically larger endpoint by (x, y).
            let corner = if first.x < last.x || first.x == last.x && first.y < last.y {
                last
            } else {
                first
            };
            let key = SweepKey {
                x: corner.x,
                y: corner.y,
                layer: trace.layer,
            };
            if !(key.is_after(&self.cursor) && key.is_before(&best)) {
                continue;
            }
            // `:542-547`, checked only once the key already wins: a trace that
            // touches a rippable via is left to that via's own turn.
            normal_contacts(board, id, scratch, &mut contacts);
            let touches_live_via = contacts.iter().copied().any(|contact| {
                board.store().get(contact).is_some_and(|contact_item| {
                    matches!(contact_item.kind, ItemKind::Via(_)) && !contact_item.is_user_fixed()
                })
            });
            if touches_live_via {
                continue;
            }
            best = key;
            result = Some(id);
        }

        // `:548-549` — unconditional, exhausted or not.
        self.cursor = best;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestFixture, TEST_HALF_WIDTH, TEST_NET};
    use fr_board::item::FixedState;
    use fr_board::net_nos::NetNos;
    use fr_geometry::planar::IntPoint;

    #[test]
    fn the_cursor_starts_before_a_via_at_the_origin_on_layer_zero() {
        let origin = SweepKey {
            x: 0.0,
            y: 0.0,
            layer: 0,
        };
        assert!(origin.is_after(&SweepKey::before_everything()));
        assert!(origin.is_before(&SweepKey::after_everything()));
    }

    #[test]
    fn the_key_order_is_x_then_y_then_layer() {
        let base = SweepKey {
            x: 10.0,
            y: 10.0,
            layer: 1,
        };
        let bigger_x = SweepKey { x: 11.0, ..base };
        let bigger_y = SweepKey { y: 11.0, ..base };
        let bigger_layer = SweepKey { layer: 2, ..base };
        assert!(bigger_x.is_after(&base) && base.is_before(&bigger_x));
        assert!(bigger_y.is_after(&base) && base.is_before(&bigger_y));
        assert!(bigger_layer.is_after(&base) && base.is_before(&bigger_layer));
        // x dominates y dominates layer.
        let smaller_x_bigger_rest = SweepKey {
            x: 9.0,
            y: 99.0,
            layer: 9,
        };
        assert!(smaller_x_bigger_rest.is_before(&base));
    }

    #[test]
    fn a_tie_is_neither_after_nor_before() {
        let key = SweepKey {
            x: 3.0,
            y: 4.0,
            layer: 5,
        };
        assert!(!key.is_after(&key));
        assert!(!key.is_before(&key));
    }

    /// Two unfixed traces on layer 0, no vias: the sweep must visit both, in
    /// ascending order of their larger endpoint.
    #[test]
    fn the_sweep_visits_every_unfixed_trace_in_ascending_key_order() {
        let mut fixture = TestFixture::two_layer_with_pads();
        let left = fixture
            .board
            .insert_trace_points(
                &[IntPoint::new(-2500, 500), IntPoint::new(-2000, 500)],
                0,
                TEST_HALF_WIDTH,
                NetNos::of(TEST_NET),
                1,
                FixedState::Unfixed,
                None,
            )
            .expect("the left wire inserts");
        let right = fixture
            .board
            .insert_trace_points(
                &[IntPoint::new(2000, 500), IntPoint::new(2500, 500)],
                0,
                TEST_HALF_WIDTH,
                NetNos::of(TEST_NET),
                1,
                FixedState::Unfixed,
                None,
            )
            .expect("the right wire inserts");

        let mut scratch = ContactScratch::new();
        let mut sweep = SortedRouteItems::new();
        let mut seen = Vec::new();
        while let Some(id) = sweep.next(&fixture.board, &mut scratch) {
            seen.push(id);
        }
        assert_eq!(seen, vec![left, right], "ascending larger-endpoint x");
    }

    #[test]
    fn a_shove_fixed_trace_is_never_handed_out() {
        let mut fixture = TestFixture::two_layer_with_pads();
        fixture
            .board
            .insert_trace_points(
                &[IntPoint::new(-2500, 500), IntPoint::new(-2000, 500)],
                0,
                TEST_HALF_WIDTH,
                NetNos::of(TEST_NET),
                1,
                FixedState::ShoveFixed,
                None,
            )
            .expect("the wire inserts");
        let mut scratch = ContactScratch::new();
        let mut sweep = SortedRouteItems::new();
        assert_eq!(sweep.next(&fixture.board, &mut scratch), None);
    }

    #[test]
    fn the_sweep_stops_and_stays_stopped() {
        let fixture = TestFixture::two_layer_with_pads();
        let mut scratch = ContactScratch::new();
        let mut sweep = SortedRouteItems::new();
        assert_eq!(sweep.next(&fixture.board, &mut scratch), None);
        assert_eq!(sweep.next(&fixture.board, &mut scratch), None);
        assert_eq!(sweep.current_position(), SweepKey::after_everything());
    }
}
