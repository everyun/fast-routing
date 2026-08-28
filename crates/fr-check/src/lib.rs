//! **The referee.** Given a board, it says how many clearance violations it
//! has, how many connections are still missing, and what it scores.
//!
//! This crate is the arbiter for every quality claim the project makes: the
//! benchmark judges *both* tools' SES output with it, and gate G1.2a calibrates
//! its violation multiset against `java -drc` on the input boards. So it is
//! written to one rule — **it decides nothing on its own authority**. Every
//! predicate here is a transcription of `docs/upstream-study/drc-connectivity.md`,
//! which is in turn a transcription of the released 2.3.0 jar; where this port
//! deviates, the deviation is named in the module docs and is one that provably
//! cannot change a count.
//!
//! ```text
//! check_clearance(view)      -> Vec<Violation>       the M in "(N unrouted and M violations)"
//! board_connectivity(view)   -> BoardConnectivity    the N, and max_connections
//! score(stats, weights)      -> Score                the number in front of it
//! ```
//!
//! # Three things that are easy to get wrong
//!
//! 1. **The metric is not Euclidean.** Clearance is measured by enlarging both
//!    shapes and testing overlap, and enlarging is a Minkowski sum with a
//!    *regular octagon* whose bounds move by `round(d)` and `round(sqrt(2)*d)`.
//!    The octagon contains the disc, so Freerouting is up to 8.24 % stricter
//!    than a Euclidean checker in the directions between the axes and the
//!    diagonals. See [`shape`].
//! 2. **Touching is legal.** The verdict is `dimension(...) == 2` and nothing
//!    else; a 1-D or 0-D intersection of the enlarged shapes is not a violation.
//!    Combined with integer rounding this means a pair at exactly the required
//!    clearance is legal, and a pair one unit closer is not.
//! 3. **Connectivity is exact-point.** A trace connects only at its two end
//!    corners — a trace crossing another mid-span on the same net and layer is
//!    *not* connected. There is no tolerance anywhere. See [`mod@connectivity`].
//!
//! # The seam
//!
//! The checker never names a concrete board. It takes a [`BoardView`]: an item
//! list, per-item convex tile shapes with their layers, and a clearance lookup.
//! [`StoreView`] adapts the [`fr_board::ItemStore`] model to it, and the
//! clearance matrix is reached through the two-method [`ClearanceSource`] trait
//! so that the ongoing rules rework cannot break the referee. Deriving tile
//! shapes is the search-tree manager's job (drill items are inflated by the hole
//! clearance delta, the board outline becomes per-layer strips); until that
//! lands, whoever builds a board hands its shapes over in a [`ShapeStore`].
//! [`mod@view`] documents the five-line `BoardView` impl that wires this crate
//! to `fr_board::Board` once that API settles, and why the referee keeps its own
//! [`TileShape`] and clearance trait rather than sharing the router's.
//!
//! # Not ported on purpose
//!
//! The GUI's clearance-compensated tree (it double-counts, §7.1), the
//! `smallestClearance` sticky mutable field on `Item` (it reports a historical
//! minimum, not the current one, and makes DRC non-idempotent, §7.5), and the
//! seed-99 shuffled Delaunay triangulation behind the ratsnest — airline
//! *geometry* is explicitly not a parity target and an exact MST gives the same
//! count.

#![deny(missing_docs)]

pub mod anchors;
pub mod clearance;
pub mod connectivity;
pub mod score;
pub mod shape;
pub mod view;

pub use anchors::{Anchor, AnchorIndex, AnchorKind};
pub use clearance::{
    actual_clearance, check_clearance, check_clearance_with, clearance_between_shapes, Violation,
    ACTUAL_CLEARANCE_STEPS, CLEARANCE_SAFETY_MARGIN,
};
pub use connectivity::{
    board_connectivity, connectivity, net_connectivity, AirLine, BoardConnectivity, ContactGraph,
    NetConnectivity,
};
pub use score::{score, BoardStats, Score, ScoreWeights};
pub use shape::TileShape;
pub use view::{
    BoardView, ClearanceSource, IdIndex, ItemShape, ShapeStore, StoreView, UniformClearance,
};

/// Returns the crate version.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
