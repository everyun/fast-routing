//! The board model: items, the store they live in, and the state mechanisms
//! the router mutates them through.
//!
//! Mirrors `app.freerouting.board` from the upstream Java project, following
//! `docs/ARCHITECTURE.md` (fr-board section) and the upstream studies it cites:
//!
//! * [`item`] — the item model. Nine item classes flattened to one struct plus
//!   an [`ItemKind`] enum, carrying what the checker and the router need: net
//!   array, clearance class, layer span, [`FixedState`], and the obstacle matrix
//!   (`v2-vs-v19-and-scope.md` §9, §12; `drc-connectivity.md` §1.4, §2.3).
//! * [`store`] — the [`ItemStore`]: a slot arena, a separate monotonic
//!   [`ItemId`], descending-id iteration for parity, and an incremental per-net
//!   index (`search-tree-obstacles.md` §10, "item registry").
//! * [`journal`] — **state mechanism 1**, the transaction: a command log with
//!   O(1) [`begin`](ItemStore::begin) and O(Δ)
//!   [`rollback_to`](ItemStore::rollback_to). The shove/insert primitive.
//! * [`history`] — **state mechanism 2**, the board history: pooled arena
//!   snapshots, O(board) to take, and a swap plus a [`StoreDelta`] to restore,
//!   with a callback for dependent-state invalidation.
//! * state mechanism 3, the worker clone, is [`ItemStore`]'s plain deep
//!   [`Clone`] for now; where `Arc`-sharing slots in is documented in [`store`].
//!
//! On top of that storage layer sits the board proper:
//!
//! * [`board`] — the [`Board`]: store + layer stack + clearance
//!   matrix + padstacks + search trees + connectivity, and the mutation
//!   protocol that keeps all of them in step (`v2-vs-v19-and-scope.md` §11.1-3).
//! * [`trace_ops`] — trace split, combine and normalize (§11.4-6), including
//!   the three guards that make them terminate: an iterative `combine`, the
//!   depth-16 normalize cap, and the 2000-iteration per-net oscillation valve.
//! * [`search_tree`] — the [`SearchTreeManager`]:
//!   one `ArenaShapeTree` per compensated clearance class, per-item tile shapes
//!   and the incremental insert/remove wired into every board mutation
//!   (`search-tree-obstacles.md` §2, §3, §5).
//! * [`clearance`] — the compensation rule those trees are built on: obstacle
//!   inflation `clearance(item, tree) − (self+1)/2`, router radius
//!   `half_width + (self+1)/2`, even clearances, class 0 never inflated.
//! * [`shape`] — the tile-shape enum the trees index and the padstack geometry
//!   drill items derive theirs from.
//! * [`connectivity`] — per-net connected components under the exact-point
//!   contact predicate (`drc-connectivity.md` §4.3), rebuilt lazily per dirty
//!   net.
//!
//! # Ordering discipline
//!
//! Upstream's `Item.compareTo` is `other.id - this.id`, so every board scan runs
//! in **descending id order** and routing results depend on it.
//! [`ItemStore::iter`] and [`ItemStore::ids`] are that order;
//! [`ItemStore::iter_ascending`] exists for the places that provably cannot
//! affect a result. Arena slot indices are never an ordering input.
//!
//! # What lands next
//!
//! The pieces this wave deliberately left to the crates that own them, each
//! flagged at its call site:
//!
//! * `ChangedArea` — the per-layer "what did this operation touch" octagon that
//!   supplies the normalize clip and drives regional pull-tight. Passed in as a
//!   parameter here; belongs with the shove/optimize wave.
//! * `removeIfCycle` / `getConnectionItems` / trace-tail removal (§10.3-10.4) —
//!   the ripup unit, which belongs with the router. Its absence is why
//!   [`normalize_traces`](Board::normalize_traces) has no cycle branch
//!   and why `split` skips self-intersections.
//! * The `merge_entries_*` / `change_entries` / `reuse_entries_after_cutout`
//!   leaf-reuse fast paths (`search-tree-obstacles.md` §6). Plain remove +
//!   insert produces the same index; these are the optimization.
//! * `overlapping_tree_entries_with_clearance` — the uncompensated
//!   sorted-clearance query, which only the checker uses.
//!
//! [`BoardHistory::restore_with`](history::BoardHistory::restore_with) is still
//! the seam where a pass-level restore invalidates the derived state; wiring it
//! to [`Board`] is a `StoreDelta` walk calling the same
//! index/unindex pair [`rollback_to`](Board::rollback_to) uses.
//!
//! # Legacy compatibility
//!
//! [`BasicBoard`] and its per-type `Vec`s are the pre-rewrite model, kept in
//! [`legacy`] only until `fr-core`, `fr-autoroute` and `fr-drc` are replaced.
//! The crate root re-exports them under their old names so those crates compile
//! unchanged; the new payload types with colliding names live in [`item`]
//! (`fr_board::item::Pin`, `fr_board::item::Via`, …). When the last consumer of
//! the legacy model is gone the module is deleted and the root re-exports move
//! to the [`item`] ones.

pub mod board;
pub mod clearance;
pub mod connectivity;
pub mod history;
pub mod item;
pub mod journal;
pub mod legacy;
pub mod net_nos;
pub mod search_tree;
pub mod shape;
pub mod store;
pub mod trace_ops;

pub use board::{Board, BoardMark, Layer, LayerStructure};
pub use clearance::{ClearanceSource, ClearanceTable};
pub use connectivity::Connectivity;
pub use history::BoardHistory;
pub use item::{
    AreaShape, FixedState, Item, ItemClass, ItemId, ItemKind, LayerRange, ObstacleKind, PlacedArea,
};
pub use journal::{Journal, Mark};
pub use net_nos::NetNos;
pub use search_tree::{SearchTree, SearchTreeManager, TreeEntry};
pub use shape::{AngleRestriction, Padstack, PadstackTable, TileShape};
pub use store::{ItemStore, SlotIndex, StoreDelta};

// The pre-rewrite model. See the `legacy` module docs; these names shadow the
// new payload types of the same name, which are reachable as `item::Pin` etc.
pub use legacy::{
    BasicBoard, ConductionArea, ItemHeader, ItemType, ObstacleArea, Pin, PolylineTrace, Via,
};
