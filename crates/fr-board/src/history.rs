//! State mechanism 2 of 3: the **board history**.
//!
//! `ARCHITECTURE.md`, fr-board section:
//!
//! > 2. board history = arena snapshot (memcpy of item+node Vecs), O(board) but
//! >    ~ms; restore must also restore tree state and preserves the room DB
//! >    interaction (restore invalidates per-item room caches — budgeted)
//! >
//! > M1 microbench gate: restore of a 30-pass-old board on the largest fixture
//! > < 50 ms
//!
//! This is the pass-level mechanism: the batch autorouter snapshots the board
//! between passes so a pass that made things worse can be thrown away wholesale.
//! Upstream implements it as `BoardSnapshotManager.serialize` —
//! `ObjectOutputStream.writeObject(board)` — with `BasicBoard.clone()` and
//! `RoutingBoard.deepCopy()` being the same serialize/deserialize round trip
//! (`v2-vs-v19-and-scope.md` §13), which also rebuilds every search tree from
//! scratch in `readObject`.
//!
//! Here a snapshot is [`ItemStore::copy_from`] into a **pooled** buffer, and a
//! restore is a `mem::swap` plus a [`StoreDelta`] describing exactly which items
//! changed. The pool is what makes repeated snapshots cheap: the buffer
//! recycled from an earlier level already holds items of the right kind in the
//! right slots, so the copy reuses their allocations down to the individual
//! polyline `Vec<Line>`.
//!
//! # The invalidation callback
//!
//! A restore is not complete when the items are back: everything derived from
//! them is stale. Upstream's `BasicBoard.applyUndoRedoSideEffects` walks the
//! cancelled and restored object sets, removing and reinserting them in the
//! search trees and clearing their autoroute info. [`BoardHistory::restore_with`]
//! hands the same information to a caller-supplied closure as a [`StoreDelta`],
//! which is the subscription seam for the search-tree manager and the autoroute
//! room database when they land in the next wave. Computing the delta is a
//! merge walk over two id-sorted arrays comparing revision stamps — microseconds
//! on a 20 000-item board, and it turns "rebuild every tree" into "touch the
//! items that actually moved".
//!
//! # What history does *not* do
//!
//! It does not nest with the transaction journal. A snapshot taken inside an
//! open transaction would leave the journal holding pre-images of items the
//! restore replaced, so both entry points panic when
//! [`ItemStore::transaction_depth`] is non-zero. Upstream has the same layering:
//! shove transactions live entirely inside one pass.

use crate::item::ItemId;
use crate::store::{ItemStore, StoreDelta};

/// A pooled deep copy of an [`ItemStore`].
///
/// Opaque on purpose: the only operations that make sense are "fill me from a
/// store" and "become the store", both of which [`BoardHistory`] drives.
#[derive(Debug, Clone, Default)]
struct Snapshot {
    store: ItemStore,
}

/// A stack of board snapshots with a buffer pool.
///
/// ```
/// # use fr_board::{BoardHistory, ItemStore};
/// let mut store = ItemStore::new();
/// let mut history = BoardHistory::new();
/// history.snapshot(&store);          // pass boundary
/// // ... a routing pass mutates the store ...
/// let delta = history.restore(&mut store).unwrap();
/// assert!(delta.is_empty());
/// ```
#[derive(Debug, Default)]
pub struct BoardHistory {
    stack: Vec<Snapshot>,
    pool: Vec<Snapshot>,
    delta: StoreDelta,
}

impl BoardHistory {
    /// An empty history.
    pub fn new() -> Self {
        BoardHistory::default()
    }

    /// How many snapshots are stacked up.
    #[inline]
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// True when nothing has been snapshotted.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }

    /// Pushes a snapshot of `store`. O(board), reusing a pooled buffer.
    ///
    /// # Panics
    ///
    /// If a transaction is open on `store`; see the module docs.
    pub fn snapshot(&mut self, store: &ItemStore) {
        assert_eq!(
            store.transaction_depth(),
            0,
            "fr-board: cannot snapshot the board while a transaction is open — \
             the journal's pre-images would outlive the state they describe"
        );
        let mut buffer = self.pool.pop().unwrap_or_default();
        buffer.store.copy_from(store);
        self.stack.push(buffer);
    }

    /// Restores the most recent snapshot, returning what changed.
    ///
    /// Returns `None` when the history is empty.
    pub fn restore(&mut self, store: &mut ItemStore) -> Option<&StoreDelta> {
        self.restore_back(store, 1)
    }

    /// Restores the snapshot `levels` passes back, discarding the newer ones,
    /// and returns what changed relative to the current state.
    ///
    /// `levels == 1` is [`restore`](BoardHistory::restore). Returns `None` when
    /// `levels` is 0 or deeper than the stack, leaving the store untouched.
    ///
    /// The cost is one `mem::swap` plus the delta walk; the O(board) part of the
    /// work was paid at [`snapshot`](BoardHistory::snapshot) time, which is what
    /// the `< 50 ms` gate in `ARCHITECTURE.md` measures.
    ///
    /// # Panics
    ///
    /// If a transaction is open on `store`.
    pub fn restore_back(&mut self, store: &mut ItemStore, levels: usize) -> Option<&StoreDelta> {
        assert_eq!(
            store.transaction_depth(),
            0,
            "fr-board: cannot restore the board while a transaction is open"
        );
        if levels == 0 || levels > self.stack.len() {
            return None;
        }
        // Return the passes we are skipping over to the pool, newest first.
        for _ in 0..levels - 1 {
            let discarded = self.stack.pop().expect("stack depth was checked");
            self.pool.push(discarded);
        }
        let mut target = self.stack.pop().expect("stack depth was checked");

        store.diff_into(&target.store, &mut self.delta);
        let (next_id, next_revision) = store.raw_counters();
        std::mem::swap(store, &mut target.store);
        // The restored state carries the counters it had when it was taken;
        // ids and revisions must never move backwards (see ItemId).
        store.raise_counters(next_id, next_revision);

        self.pool.push(target);
        Some(&self.delta)
    }

    /// [`restore_back`](BoardHistory::restore_back) with the dependent-state
    /// invalidation callback.
    ///
    /// `on_restore` is the subscription point for everything derived from the
    /// item store — the compensated search trees, the autoroute room database,
    /// the connectivity union-find. It runs after the swap, so `store` is
    /// already the restored board and the delta describes how to get the
    /// derived state from where it was to where it must be:
    ///
    /// * [`removed`](StoreDelta::removed) — drop the item's tree leaves and
    ///   caches;
    /// * [`added`](StoreDelta::added) — recompute tile shapes and insert;
    /// * [`changed`](StoreDelta::changed) — remove and reinsert (upstream's
    ///   `clearDerivedData` + reinsert pair).
    ///
    /// Returns whether anything was restored.
    pub fn restore_back_with(
        &mut self,
        store: &mut ItemStore,
        levels: usize,
        on_restore: impl FnOnce(&ItemStore, &StoreDelta),
    ) -> bool {
        if self.restore_back(store, levels).is_none() {
            return false;
        }
        on_restore(store, &self.delta);
        true
    }

    /// [`restore_back_with`](BoardHistory::restore_back_with) for one level.
    pub fn restore_with(
        &mut self,
        store: &mut ItemStore,
        on_restore: impl FnOnce(&ItemStore, &StoreDelta),
    ) -> bool {
        self.restore_back_with(store, 1, on_restore)
    }

    /// Drops the newest snapshot without restoring it — the "this pass was
    /// good, keep it" path.
    ///
    /// Returns whether there was one.
    pub fn accept(&mut self) -> bool {
        match self.stack.pop() {
            Some(snapshot) => {
                self.pool.push(snapshot);
                true
            }
            None => false,
        }
    }

    /// Ids present in the snapshot `levels` passes back but not in `store`, and
    /// vice versa — a dry run of [`restore_back`](BoardHistory::restore_back)
    /// that mutates nothing.
    pub fn preview(&mut self, store: &ItemStore, levels: usize) -> Option<&StoreDelta> {
        if levels == 0 || levels > self.stack.len() {
            return None;
        }
        let target = &self.stack[self.stack.len() - levels];
        store.diff_into(&target.store, &mut self.delta);
        Some(&self.delta)
    }

    /// The delta produced by the last restore or preview.
    #[inline]
    pub fn last_delta(&self) -> &StoreDelta {
        &self.delta
    }

    /// Drops every snapshot, keeping the buffers pooled.
    pub fn clear(&mut self) {
        self.pool.append(&mut self.stack);
        self.delta.clear();
    }

    /// Releases the pooled buffers. Only useful when a board is done with.
    pub fn shrink(&mut self) {
        self.pool.clear();
    }

    /// The ids the snapshot `levels` back holds, for diagnostics.
    ///
    /// Descending id order, like every other board scan.
    pub fn snapshot_ids(&self, levels: usize) -> Option<Vec<ItemId>> {
        if levels == 0 || levels > self.stack.len() {
            return None;
        }
        Some(self.stack[self.stack.len() - levels].store.ids().collect())
    }
}

#[cfg(test)]
mod tests {
    use fr_geometry::planar::{IntPoint, Polyline};

    use super::*;
    use crate::item::{FixedState, Item, ItemKind, PolylineTrace};
    use crate::net_nos::NetNos;

    fn trace(nets: &[i32], layer: i32) -> Item {
        Item::new(
            NetNos::from_slice(nets),
            1,
            FixedState::Unfixed,
            ItemKind::Trace(PolylineTrace {
                polyline: Polyline::from_int_points(&[IntPoint::new(0, 0), IntPoint::new(1000, 0)]),
                layer,
                half_width: 50,
            }),
        )
    }

    #[test]
    fn restore_undoes_a_pass_and_reports_the_delta() {
        let mut store = ItemStore::new();
        let kept = store.insert(trace(&[1], 0));
        let doomed = store.insert(trace(&[1], 0));

        let mut history = BoardHistory::new();
        history.snapshot(&store);

        store.remove(doomed);
        let added = store.insert(trace(&[2], 1));
        store.mutate(kept, |i| i.clearance_class = 42).unwrap();

        let delta = history.restore(&mut store).unwrap().clone();
        assert_eq!(delta.added, vec![doomed], "the removed item comes back");
        assert_eq!(delta.removed, vec![added], "the new item goes away");
        assert_eq!(delta.changed, vec![kept]);

        assert!(store.contains(doomed));
        assert!(!store.contains(added));
        assert_eq!(store.get(kept).unwrap().clearance_class, 1);
        store.assert_consistent();
    }

    #[test]
    fn restoring_thirty_passes_back_skips_the_intermediate_snapshots() {
        let mut store = ItemStore::new();
        let base: Vec<_> = (0..5).map(|_| store.insert(trace(&[1], 0))).collect();

        let mut history = BoardHistory::new();
        history.snapshot(&store);
        for _ in 0..29 {
            store.insert(trace(&[2], 0));
            history.snapshot(&store);
        }
        assert_eq!(history.depth(), 30);

        history.restore_back(&mut store, 30).unwrap();
        assert_eq!(history.depth(), 0);
        assert_eq!(store.ids().collect::<Vec<_>>().len(), base.len());
        store.assert_consistent();
    }

    #[test]
    fn ids_stay_monotone_across_a_restore() {
        let mut store = ItemStore::new();
        let mut history = BoardHistory::new();
        history.snapshot(&store);

        let a = store.insert(trace(&[1], 0));
        history.restore(&mut store).unwrap();
        assert!(!store.contains(a));

        let b = store.insert(trace(&[1], 0));
        assert!(
            b > a,
            "restoring an older state must not rewind the id counter: {a} then {b}"
        );
        store.assert_consistent();
    }

    #[test]
    fn revision_stamps_stay_monotone_across_a_restore() {
        let mut store = ItemStore::new();
        let a = store.insert(trace(&[1], 0));
        let mut history = BoardHistory::new();
        history.snapshot(&store);

        store.mutate(a, |i| i.clearance_class = 9).unwrap();
        let mutated_revision = store.revision_of(a).unwrap();
        history.restore(&mut store).unwrap();

        // The item's content version is the old one again ...
        assert!(store.revision_of(a).unwrap() < mutated_revision);
        // ... but the store-wide counter is not, so the next write gets a
        // version number that has never been used for this id.
        store.mutate(a, |i| i.clearance_class = 11).unwrap();
        assert!(store.revision_of(a).unwrap() > mutated_revision);
    }

    #[test]
    fn the_callback_sees_the_restored_board() {
        let mut store = ItemStore::new();
        let mut history = BoardHistory::new();
        history.snapshot(&store);
        let added = store.insert(trace(&[1], 0));

        let mut seen = Vec::new();
        let restored = history.restore_with(&mut store, |store, delta| {
            seen.extend_from_slice(&delta.removed);
            assert!(!store.contains(added), "callback runs after the swap");
        });
        assert!(restored);
        assert_eq!(seen, vec![added]);
    }

    #[test]
    fn accept_drops_a_snapshot_without_touching_the_board() {
        let mut store = ItemStore::new();
        let mut history = BoardHistory::new();
        history.snapshot(&store);
        let added = store.insert(trace(&[1], 0));
        assert!(history.accept());
        assert_eq!(history.depth(), 0);
        assert!(store.contains(added));
    }

    #[test]
    fn preview_reports_the_delta_without_mutating() {
        let mut store = ItemStore::new();
        let mut history = BoardHistory::new();
        history.snapshot(&store);
        let added = store.insert(trace(&[1], 0));

        let delta = history.preview(&store, 1).unwrap().clone();
        assert_eq!(delta.removed, vec![added]);
        assert_eq!(history.depth(), 1);
        assert!(store.contains(added));
    }

    #[test]
    fn restoring_more_levels_than_exist_is_a_no_op() {
        let mut store = ItemStore::new();
        let mut history = BoardHistory::new();
        history.snapshot(&store);
        let added = store.insert(trace(&[1], 0));
        assert!(history.restore_back(&mut store, 5).is_none());
        assert!(history.restore_back(&mut store, 0).is_none());
        assert!(store.contains(added));
        assert_eq!(history.depth(), 1);
    }

    #[test]
    #[should_panic(expected = "while a transaction is open")]
    fn snapshotting_inside_a_transaction_panics() {
        let mut store = ItemStore::new();
        let _mark = store.begin();
        let mut history = BoardHistory::new();
        history.snapshot(&store);
    }

    #[test]
    #[should_panic(expected = "while a transaction is open")]
    fn restoring_inside_a_transaction_panics() {
        let mut store = ItemStore::new();
        let mut history = BoardHistory::new();
        history.snapshot(&store);
        let _mark = store.begin();
        history.restore(&mut store);
    }
}
