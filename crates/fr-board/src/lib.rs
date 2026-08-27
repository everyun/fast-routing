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
//! Trace split/merge/normalize (`v2-vs-v19-and-scope.md` §11) and the
//! `SearchTreeManager` with its compensated trees are the next wave. The seams
//! are already here: [`PolylineTrace`](item::PolylineTrace) keeps upstream's
//! line-based polyline so `split`/`combine` can operate on it directly;
//! [`ItemStore::slot_of`] and [`ItemStore::slot_capacity`] give the tree manager
//! a dense key for per-item leaf and tile-shape side tables; and
//! [`BoardHistory::restore_with`](history::BoardHistory::restore_with) is where
//! those tables get invalidated.
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

pub mod history;
pub mod item;
pub mod journal;
pub mod legacy;
pub mod net_nos;
pub mod store;

pub use history::BoardHistory;
pub use item::{
    AreaShape, FixedState, Item, ItemClass, ItemId, ItemKind, LayerRange, ObstacleKind, PlacedArea,
};
pub use journal::{Journal, Mark};
pub use net_nos::NetNos;
pub use store::{ItemStore, SlotIndex, StoreDelta};

// The pre-rewrite model. See the `legacy` module docs; these names shadow the
// new payload types of the same name, which are reachable as `item::Pin` etc.
pub use legacy::{
    BasicBoard, ConductionArea, ItemHeader, ItemType, ObstacleArea, Pin, PolylineTrace, Via,
};
