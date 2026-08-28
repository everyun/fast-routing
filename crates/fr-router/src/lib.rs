//! The M2 shape-expansion maze router.
//!
//! This crate is the Rust port of `app.freerouting.autoroute`'s maze engine as
//! shipped in the **2.3.0 release jar** (`/tmp/fr-java-release`, revision
//! `2d4de019`): the flat `autoroute/*` package, *not* the `autoroute.maze` /
//! `expansion` / `drill` package split of git master, which does not exist in
//! 2.3.0 (`maze-engine.md` conformance appendix D2).
//!
//! The specs it implements are `docs/upstream-study/maze-engine.md` (whose
//! "Release-jar conformance" appendix **overrides** the spec body) and
//! `search-tree-obstacles.md` §4 for the room-completion query.
//!
//! # What is here
//!
//! | module | upstream |
//! |---|---|
//! | [`ctrl`] | `AutorouteControl` — per-net costs, half widths, via masks, layer activity |
//! | [`engine`] | `AutorouteEngine` — the room database and its trees |
//! | [`rooms`] | `ExpansionRoom` / `ExpansionDoor` arenas |
//! | [`rooms::complete`] | `ShapeSearchTree.complete_shape` + the room lifecycle |
//! | [`rooms::neighbours`] | `SortedRoomNeighbours` — doors and gap rooms |
//! | [`maze::nodes`] | `MazeSearchElement`, as a generation-stamped arena |
//! | [`maze::queue`] | `MazeListElement` + the frontier, as a lazy-deletion heap |
//! | [`maze::heuristic`] | `DestinationDistance` |
//! | [`maze::drills`] | `ExpansionDrill`, `DrillPage`, `DrillPageArray` |
//! | [`maze::ripup`] | `check_ripup` and the small-door probes |
//! | [`maze::search`] | `MazeSearchAlgo` — the loop |
//! | [`result`] | the backtrack chain `LocateFoundConnectionAlgo` consumes |
//! | [`batch`] | `BatchAutorouter` — work-list selection, one pass, tail cleanup |
//! | [`fanout`] | `BatchFanout` + `RoutingBoard.fanout` — the SMD pin-escape stage |
//!
//! # Typical use
//!
//! ```no_run
//! # use fr_board::board::Board;
//! # use fr_board::item::ItemId;
//! # use fr_rules::board_rules::BoardRules;
//! # use fr_rules::router_settings::RouterSettings;
//! use fr_router::ctrl::AutorouteControl;
//! use fr_router::engine::AutorouteEngine;
//! use fr_router::maze::search::MazeSearch;
//!
//! # fn route(board: &mut Board, rules: &BoardRules, settings: &RouterSettings,
//! #          net_no: i32, start: &[ItemId], destination: &[ItemId]) {
//! let ctrl = AutorouteControl::for_net(board, rules, settings, net_no);
//! // The compensated tree must exist before the engine borrows the board.
//! board.ensure_compensated_tree(ctrl.trace_clearance_class_no);
//! let mut engine = AutorouteEngine::new(board, rules, ctrl);
//! if let Ok(mut search) = MazeSearch::get_instance(&mut engine, start, destination) {
//!     if let Ok(connection) = search.find_connection() {
//!         // `connection.backtrack` runs destination → start; the last element
//!         // is the start target door. `connection.ripped_items` must be
//!         // deleted before the new trace is inserted.
//!         let _ = connection;
//!     }
//! }
//! # }
//! ```
//!
//! # M2a scope
//!
//! Shove is **not** in this wave. Every `MazeShoveTraceAlgo` integration point
//! answers "the obstacle is hard", which routes the search down the
//! delayed-ripup branch it already takes when the release's shove fails on both
//! sides. The stubs are marked `M2a` in [`maze::search`] and are the only
//! functions M2b has to replace. Consequences are one-sided: M2a finds a subset
//! of the paths the release finds, never a superset.
//!
//! Two further approximations, each documented at its call site:
//!
//! * the 90°/45° `Sorted*RoomNeighbours` specializations are not ported; every
//!   board uses the any-angle general case (`maze-engine.md` §13 prescribes
//!   this porting order). The tiling is valid but not Java's;
//! * `ForcedViaAlgo.check_layer`, `check_forced_trace_polyline`, pin trace-exit
//!   geometry and pin neckdown all belong to the shove/locate waves; their M2a
//!   stand-ins are the no-shove cases of the same predicates.
//!
//! # Determinism
//!
//! Every ordering input is an integer or an arena id:
//!
//! * the frontier compares `f`, `g`, object id, section, in that order, all
//!   four (`MazeListElement.compareTo` — the id tie-breaks are shipped in
//!   2.3.0, conformance appendix D1);
//! * obstacle visit order is items by ascending id then tile index, then rooms
//!   by ascending id ([`engine`] module docs);
//! * the neighbour sort ends in an obstacle-id tie-break;
//! * no hash-map iteration anywhere.
//!
//! Ids are **ours**, not Java's, so the settle trace is comparable to the lab
//! jar's on `(g, f)` and on sequence length, not on `(door_id, section)`. See
//! [`maze::search::SettleRecord`].

#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

pub mod batch;
pub mod ctrl;
pub mod engine;
pub mod fanout;
pub mod geom;
pub mod insert;
pub mod insert_forced;
pub mod locate;
pub mod locate45;
pub mod maze;
pub mod result;
pub mod rooms;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use batch::{AttemptState, BatchRouter, PassOutcome, RunOutcome};
pub use ctrl::AutorouteControl;
pub use engine::AutorouteEngine;
pub use fanout::{fanout_board, FanoutConfig, FanoutOutcome};
pub use maze::search::{MazeSearch, SearchFailure, SettleRecord};
pub use result::{BacktrackElement, BacktrackKind, FoundConnection, RippedItem};
