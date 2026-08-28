//! The board's **connection points**, indexed for exact-point lookup.
//!
//! Freerouting's electrical connectivity is not overlap-based. Two items are
//! "normal contacts" only when they meet at an *exact* integer point
//! (`drc-connectivity.md` §4.3): a trace contributes its two end corners and
//! nothing in between, a pin or via contributes its centre, and a conduction
//! area contributes containment rather than a point. There is no tolerance, and
//! v2 deliberately deleted the one tolerant variant it had because it caused
//! false cycle detection.
//!
//! So every point-anchored contact question — the connectivity closure, the
//! tail test, and the DRC tie-pin exemption — reduces to "which anchors sit at
//! exactly this point". [`AnchorIndex`] answers that in `O(log n)` from a single
//! array sorted by [`Point::compare_xy`], which is exact across the integer and
//! rational point representations. No hashing, so no hasher-order dependence
//! and no `BigInt` allocation on the lookup path for the integer-point case that
//! covers every real board.

use fr_board::item::{ItemId, ItemKind};
use fr_geometry::planar::{IntPoint, Point};

use crate::view::BoardView;

/// Which connection point of its item an anchor is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AnchorKind {
    /// `Trace.firstCorner()`.
    TraceFirst,
    /// `Trace.lastCorner()`.
    TraceLast,
    /// `DrillItem.getCenter()` — a pin or a via.
    DrillCenter,
}

impl AnchorKind {
    /// `0` for a trace's first end, `1` for its last end; the index into the
    /// per-trace end-contact counters that decide `Trace.isTail()`.
    #[inline]
    pub(crate) fn trace_end(self) -> Option<usize> {
        match self {
            AnchorKind::TraceFirst => Some(0),
            AnchorKind::TraceLast => Some(1),
            AnchorKind::DrillCenter => None,
        }
    }
}

/// One connection point of one item.
#[derive(Debug, Clone)]
pub struct Anchor {
    /// Dense index of the owning item within the view's item list, i.e. its
    /// position in `view.item_ids()`.
    pub item: u32,
    /// Which of the item's connection points this is.
    pub kind: AnchorKind,
    /// The exact point.
    pub point: Point,
}

/// Every connection point on the board, sorted for exact lookup.
#[derive(Debug, Clone, Default)]
pub struct AnchorIndex {
    anchors: Vec<Anchor>,
}

impl AnchorIndex {
    /// Collects the connection points of every connectable item.
    ///
    /// Conduction areas contribute no anchor: they connect by *containing* a
    /// foreign anchor, which is a shape query, not a point lookup.
    pub fn build<V: BoardView + ?Sized>(view: &V) -> AnchorIndex {
        let mut anchors: Vec<Anchor> = Vec::new();
        for (dense, id) in view.item_ids().iter().enumerate() {
            let item = view.item(*id);
            let dense = dense as u32;
            match &item.kind {
                ItemKind::Trace(t) => {
                    anchors.push(Anchor {
                        item: dense,
                        kind: AnchorKind::TraceFirst,
                        point: t.first_corner(),
                    });
                    anchors.push(Anchor {
                        item: dense,
                        kind: AnchorKind::TraceLast,
                        point: t.last_corner(),
                    });
                }
                ItemKind::Pin(p) => anchors.push(Anchor {
                    item: dense,
                    kind: AnchorKind::DrillCenter,
                    point: Point::Int(p.center),
                }),
                ItemKind::Via(v) => anchors.push(Anchor {
                    item: dense,
                    kind: AnchorKind::DrillCenter,
                    point: Point::Int(v.center),
                }),
                _ => {}
            }
        }
        // Exact geometric order first, then the item's position in the view's
        // (deterministic) item order, then the kind — so the array is a stable
        // function of the view alone.
        anchors.sort_by(|a, b| {
            a.point
                .compare_xy(&b.point)
                .cmp(&0)
                .then(a.item.cmp(&b.item))
                .then(a.kind.cmp(&b.kind))
        });
        AnchorIndex { anchors }
    }

    /// All anchors, in index order.
    pub fn all(&self) -> &[Anchor] {
        &self.anchors
    }

    /// The anchors sitting at exactly `point`, as a contiguous slice.
    pub fn at(&self, point: &Point) -> &[Anchor] {
        let start = self
            .anchors
            .partition_point(|a| a.point.compare_xy(point) < 0);
        let len = self.anchors[start..]
            .iter()
            .position(|a| a.point.compare_xy(point) != 0)
            .unwrap_or(self.anchors.len() - start);
        &self.anchors[start..start + len]
    }

    /// The anchors whose x coordinate lies in `[min_x, max_x]`.
    ///
    /// Used to prefilter the point-in-conduction-area scan by the area's
    /// bounding box; the caller still has to test y and containment.
    pub fn in_x_range(&self, min_x: i32, max_x: i32) -> &[Anchor] {
        // Only the x coordinate is read, so the y of the two probes is free.
        let lo = Point::Int(IntPoint::new(min_x, 0));
        let hi = Point::Int(IntPoint::new(max_x, 0));
        let start = self.anchors.partition_point(|a| a.point.compare_x(&lo) < 0);
        let end = self
            .anchors
            .partition_point(|a| a.point.compare_x(&hi) <= 0);
        &self.anchors[start..end.max(start)]
    }

    /// The [`ItemId`] of the item owning `anchor`, resolved against the view's
    /// item list.
    #[inline]
    pub fn owner_id(ids: &[ItemId], anchor: &Anchor) -> ItemId {
        ids[anchor.item as usize]
    }
}
