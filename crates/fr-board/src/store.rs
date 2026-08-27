//! The item store: a slot arena plus the two indices the router iterates
//! through, and the transaction journal that makes mutations reversible.
//!
//! Replaces upstream's `UndoableObjects` (`v2-vs-v19-and-scope.md` §13), the
//! `ConcurrentSkipListMap` that is simultaneously the board's item storage, its
//! iteration order and its undo stack. `search-tree-obstacles.md` §10 names the
//! replacement directly:
//!
//! > **Item registry.** Replace `UndoableObjects` linear scans (`get_item`
//! > O(n), `get_pin` O(n)) with a slotmap + generation, keeping the undo-layer
//! > semantics as a separate journal.
//!
//! # The three indices, and why each is shaped the way it is
//!
//! **Slots** (`slots: Vec<Slot>` plus a free list). Storage only. A slot index
//! is never a public identity and never influences ordering; it exists so the
//! search-tree manager and the autoroute engine can hold dense side tables
//! ([`slot_of`](ItemStore::slot_of), [`slot_capacity`](ItemStore::slot_capacity))
//! instead of the per-item `LinkedList<SearchTreeInfo>` upstream walks linearly
//! on every shape lookup. Freed slots are recycled, which is exactly why the
//! semantic [`ItemId`] is a separate, never-recycled counter.
//!
//! **The ordered index** (`order: Vec<OrderEntry>`, ascending by id). This is
//! the parity-critical one: upstream iterates every board scan in *descending*
//! id order, so [`iter`](ItemStore::iter) walks this vector backwards. The
//! choice between `BTreeMap<Reverse<ItemId>, slot>` and a sorted `Vec` came out
//! in favor of the vector because of four properties of this workload:
//!
//! * **Insertion is always at the maximum.** Ids are handed out monotonically,
//!   so a new item's id exceeds every id present. In an *ascending* vector that
//!   is a `push` — O(1) amortized, no comparisons — and the "descending" half of
//!   the requirement is `.rev()` at the iterator. A descending-sorted vector
//!   would memmove the whole array on every insert, which is why the ordering is
//!   stored ascending and reversed on the way out.
//! * **Removal is a tombstone.** Clearing the slot link keeps the array sorted,
//!   so the binary search that resolves an id still works, at O(1) instead of
//!   the O(n) memmove `Vec::remove` costs. Compaction runs when tombstones pass
//!   half the array, and is suppressed while a transaction is open — which also
//!   makes rolling a removal back O(log n), because the tombstone is still there
//!   to revive rather than splice.
//! * **Snapshots are memcpy.** [`BoardHistory`](crate::history::BoardHistory)
//!   copies this array into a pooled buffer every pass
//!   ([`copy_from`](ItemStore::copy_from)); an 8-byte POD element type makes
//!   that one `memcpy`, where a `BTreeMap` would be ~1800 node allocations per
//!   20 000-item snapshot.
//! * **Iteration is a linear scan.** The full-board descending walk is the
//!   hottest whole-board operation there is (DRC, ratsnest, `combineTraces`);
//!   a contiguous array of 8-byte entries streams, a B-tree hops.
//!
//! The costs paid for that: `get` is a binary search rather than a hash lookup
//! (~15 in-cache comparisons on a 20 000-item board) and iteration branches past
//! tombstones.
//!
//! **The per-net index** (`nets: BTreeMap<i32, Vec<ItemId>>`). Same ascending
//! trick per net, maintained incrementally on every insert/remove/net change.
//! `BTreeMap` and not `HashMap` because `ARCHITECTURE.md` forbids iterating a
//! default-hasher map anywhere results can depend on the order, and
//! [`nets`](ItemStore::nets) is exactly such an iteration.
//!
//! # Mechanism 3: copy-on-write clones
//!
//! [`ItemStore`] derives a plain deep [`Clone`], which is what the portfolio /
//! optimizer workers need today: a worker gets an independent board, mutates it,
//! and its result is either promoted or dropped. The CoW form (`Arc`-shared
//! immutable chunks, `ARCHITECTURE.md` mechanism 3) lands with the parallel work
//! and slots in at exactly one place: the slot arena becomes a
//! `Vec<Arc<ItemChunk>>` of fixed-size chunks, and the two `&mut Item` paths
//! ([`get_mut`](ItemStore::get_mut) and [`mutate`](ItemStore::mutate)) do
//! `Arc::make_mut` on the chunk holding the item. The three indices stay owned
//! and memcpy-cloned — they are 8 bytes per item and no worker benefits from
//! sharing them, whereas the item payloads (polylines, convex decompositions)
//! are the bytes worth sharing. No public signature in this module changes;
//! "the only routes to a `&mut Item` are `get_mut` and `mutate`" is already the
//! invariant CoW needs, and it is enforced by the private `slots` field.

use std::collections::BTreeMap;

use crate::item::{Item, ItemId};
use crate::journal::{Entry, Journal, Mark};
use crate::net_nos::NetNos;

/// Sentinel in `OrderEntry::slot` marking a removed (tombstoned) entry.
const NO_SLOT: u32 = u32::MAX;

/// Below this many entries, compacting the ordered index is not worth the scan.
const MIN_COMPACT_LEN: usize = 64;

/// An index into the slot arena. Internal identity only — see the module docs
/// on why this must never reach an ordering decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SlotIndex(u32);

impl SlotIndex {
    /// The raw index, for side tables sized by [`ItemStore::slot_capacity`].
    #[inline]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// One entry of the ordered index: an id and the slot it currently lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OrderEntry {
    id: ItemId,
    slot: u32,
}

impl OrderEntry {
    #[inline]
    fn is_live(&self) -> bool {
        self.slot != NO_SLOT
    }
}

/// Which items differ between two states of the store.
///
/// Produced by [`ItemStore::diff_into`] and handed to the restore callback of
/// [`BoardHistory`](crate::history::BoardHistory) so dependent state — the
/// search trees, the autoroute room database, connectivity — can be invalidated
/// per item instead of rebuilt wholesale. Upstream computes the same thing as
/// the `cancelledObjects` / `restoredObjects` pair that
/// `BasicBoard.applyUndoRedoSideEffects` consumes (§13).
///
/// Every list is in ascending id order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoreDelta {
    /// Present before, absent after: remove these from the search trees.
    pub removed: Vec<ItemId>,
    /// Absent before, present after: insert these into the search trees.
    pub added: Vec<ItemId>,
    /// Present in both with different content: remove and reinsert.
    pub changed: Vec<ItemId>,
}

impl StoreDelta {
    /// An empty delta.
    pub fn new() -> Self {
        StoreDelta::default()
    }

    /// Empties the three lists while keeping their capacity, so one delta can
    /// be reused across restores.
    pub fn clear(&mut self) {
        self.removed.clear();
        self.added.clear();
        self.changed.clear();
    }

    /// True when the two states were identical.
    pub fn is_empty(&self) -> bool {
        self.removed.is_empty() && self.added.is_empty() && self.changed.is_empty()
    }

    /// Total number of affected items.
    pub fn len(&self) -> usize {
        self.removed.len() + self.added.len() + self.changed.len()
    }
}

/// A slot of the arena: the item, plus the revision its content was written at.
#[derive(Debug)]
struct Slot {
    item: Option<Item>,
    /// Value of the store-wide revision counter when this content was written.
    ///
    /// Globally monotone, never reused and never rolled back, so
    /// `(id, revision)` identifies a content version uniquely for the lifetime
    /// of the board. That is what lets [`ItemStore::diff_into`] detect
    /// "same id, different content" in O(1) per item instead of deep-comparing.
    revision: u64,
}

impl Clone for Slot {
    fn clone(&self) -> Self {
        Slot {
            item: self.item.clone(),
            revision: self.revision,
        }
    }

    /// Hand-written so that `Vec::clone_from` on the arena reaches
    /// [`Item::clone_from`](crate::item::Item), which reuses the per-item
    /// buffers. The derived `clone_from` would be `*self = source.clone()` and
    /// reallocate every polyline on every snapshot.
    fn clone_from(&mut self, source: &Self) {
        self.item.clone_from(&source.item);
        self.revision = source.revision;
    }
}

/// The board's items, in the two orders the engine reads them in.
///
/// See the module documentation for the design rationale of each index.
#[derive(Debug, Clone)]
pub struct ItemStore {
    slots: Vec<Slot>,
    free_slots: Vec<u32>,
    /// Ascending by id, including tombstones; iterated backwards for parity.
    order: Vec<OrderEntry>,
    tombstones: u32,
    live: u32,
    /// Per net, ascending by id. Only real nets (`> 0`) are indexed.
    nets: BTreeMap<i32, Vec<ItemId>>,
    next_id: u32,
    next_revision: u64,
    journal: Journal,
    /// Reusable buffer for rollback, so undoing a transaction allocates nothing.
    undo_scratch: Vec<Entry>,
}

impl Default for ItemStore {
    fn default() -> Self {
        ItemStore::new()
    }
}

impl ItemStore {
    /// An empty store. The first id handed out is 1, matching upstream's
    /// 1-based id generator.
    pub fn new() -> Self {
        ItemStore {
            slots: Vec::new(),
            free_slots: Vec::new(),
            order: Vec::new(),
            tombstones: 0,
            live: 0,
            nets: BTreeMap::new(),
            next_id: 1,
            next_revision: 1,
            journal: Journal::new(),
            undo_scratch: Vec::new(),
        }
    }

    /// An empty store with room for `capacity` items — the board-load path,
    /// which knows the item count from the DSN before inserting anything.
    pub fn with_capacity(capacity: usize) -> Self {
        let mut store = ItemStore::new();
        store.slots.reserve(capacity);
        store.order.reserve(capacity);
        store
    }

    // ---- size ------------------------------------------------------------

    /// Number of items on the board.
    #[inline]
    pub fn len(&self) -> usize {
        self.live as usize
    }

    /// True when the board holds no items.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// The id the next [`insert`](ItemStore::insert) will hand out.
    ///
    /// This is upstream's `maxItemIdBefore` idiom: capture it before an
    /// operation, and afterwards every item with `id >= it` was created by that
    /// operation (`v2-vs-v19-and-scope.md` §9.1).
    #[inline]
    pub fn next_id(&self) -> ItemId {
        ItemId::from_raw(self.next_id)
    }

    /// The store-wide content revision counter, monotone across rollback and
    /// history restore.
    #[inline]
    pub fn revision(&self) -> u64 {
        self.next_revision
    }

    // ---- lookup ----------------------------------------------------------

    /// Position of `id` in the ordered index, tombstone or not.
    #[inline]
    fn find(&self, id: ItemId) -> Result<usize, usize> {
        self.order.binary_search_by_key(&id, |e| e.id)
    }

    /// Slot of a live id.
    #[inline]
    fn live_slot(&self, id: ItemId) -> Option<usize> {
        let entry = *self.order.get(self.find(id).ok()?)?;
        if entry.is_live() {
            Some(entry.slot as usize)
        } else {
            None
        }
    }

    /// The item with this id, or `None` if it is not on the board.
    pub fn get(&self, id: ItemId) -> Option<&Item> {
        self.slots[self.live_slot(id)?].item.as_ref()
    }

    /// True when `id` is on the board — the replacement for upstream's
    /// `Item.isOnTheBoard()` flag.
    #[inline]
    pub fn contains(&self, id: ItemId) -> bool {
        self.live_slot(id).is_some()
    }

    /// The arena slot `id` currently occupies.
    ///
    /// The seam for dense per-item side tables (search-tree leaves and
    /// precomputed tile shapes, autoroute info). Slots are recycled, so a table
    /// keyed by slot must be invalidated for every id in the [`StoreDelta`] a
    /// mutation reports.
    #[inline]
    pub fn slot_of(&self, id: ItemId) -> Option<SlotIndex> {
        self.live_slot(id).map(|s| SlotIndex(s as u32))
    }

    /// Upper bound on slot indices — the size a side table must have.
    #[inline]
    pub fn slot_capacity(&self) -> usize {
        self.slots.len()
    }

    /// The item in a slot, for callers that already resolved one.
    pub fn item_at_slot(&self, slot: SlotIndex) -> Option<&Item> {
        self.slots.get(slot.index())?.item.as_ref()
    }

    /// The revision at which `id`'s content was last written.
    pub fn revision_of(&self, id: ItemId) -> Option<u64> {
        Some(self.slots[self.live_slot(id)?].revision)
    }

    // ---- iteration -------------------------------------------------------

    /// Every item, in **descending id order** — upstream's iteration order for
    /// every board scan (`Item.compareTo` is `other.id - this.id`, §9.1/§13).
    ///
    /// Routing results depend on this order; anything that iterates the board
    /// and feeds the result into a decision must use it.
    pub fn iter(&self) -> impl Iterator<Item = (ItemId, &Item)> + '_ {
        self.order.iter().rev().filter_map(move |entry| {
            if !entry.is_live() {
                return None;
            }
            Some((entry.id, self.slots[entry.slot as usize].item.as_ref()?))
        })
    }

    /// Every item in ascending id order — creation order.
    ///
    /// Not the parity order. Use it only where the order provably cannot reach
    /// a result (serialization, statistics, diffing).
    pub fn iter_ascending(&self) -> impl Iterator<Item = (ItemId, &Item)> + '_ {
        self.order.iter().filter_map(move |entry| {
            if !entry.is_live() {
                return None;
            }
            Some((entry.id, self.slots[entry.slot as usize].item.as_ref()?))
        })
    }

    /// Every id, in descending (parity) order.
    pub fn ids(&self) -> impl Iterator<Item = ItemId> + '_ {
        self.order
            .iter()
            .rev()
            .filter(|e| e.is_live())
            .map(|e| e.id)
    }

    /// The ids of every item on `net_no`, **ascending**.
    ///
    /// Ascending because that is the order the incremental index maintains for
    /// free; reverse it for parity order. Returns an empty slice for
    /// `net_no <= 0`, which is not a real net (see
    /// [`Item::contains_net`](crate::item::Item::contains_net)).
    pub fn net_items(&self, net_no: i32) -> &[ItemId] {
        if net_no <= 0 {
            return &[];
        }
        self.nets.get(&net_no).map_or(&[][..], |v| v.as_slice())
    }

    /// Every non-empty net and its items, in ascending net order.
    pub fn nets(&self) -> impl Iterator<Item = (i32, &[ItemId])> + '_ {
        self.nets
            .iter()
            .filter(|(_, ids)| !ids.is_empty())
            .map(|(net, ids)| (*net, ids.as_slice()))
    }

    // ---- mutation --------------------------------------------------------

    /// Inserts `item`, stamping it with a fresh [`ItemId`] and returning it.
    ///
    /// The store overwrites `item.id` unconditionally: it is the only authority
    /// on identity.
    pub fn insert(&mut self, mut item: Item) -> ItemId {
        let id = ItemId::from_raw(self.next_id);
        self.next_id += 1;
        item.id = id;

        let revision = self.take_revision();
        Self::index_nets(&mut self.nets, id, &item.nets);
        let slot = self.alloc_slot(item, revision);
        // Ids are monotone, so this id is greater than every id in `order`
        // (live or tombstoned) and the ascending invariant holds by `push`.
        // (`Option::is_none_or` would read better but needs Rust 1.82; the
        // workspace MSRV is 1.75.)
        debug_assert!(match self.order.last() {
            Some(last) => last.id < id,
            None => true,
        });
        self.order.push(OrderEntry { id, slot });
        self.live += 1;

        if self.journal.is_recording() {
            self.journal.push(Entry::Inserted(id));
        }
        id
    }

    /// Removes the item with this id; returns whether it was there.
    ///
    /// This is the cheap path: while a transaction is open the item is *moved*
    /// into the journal rather than cloned.
    ///
    /// It is the raw store operation and applies no policy — it is not
    /// upstream's `BoardItemRepository.removeItem`, which silently no-ops on
    /// `isDeletionForbidden()`. That guard belongs to the board layer, which
    /// owns the layer structure the predicate needs.
    pub fn remove(&mut self, id: ItemId) -> bool {
        let Some((item, revision)) = self.detach(id) else {
            return false;
        };
        if self.journal.is_recording() {
            self.journal.push(Entry::Removed { item, revision });
        }
        true
    }

    /// Removes the item and hands it back.
    ///
    /// Costs one clone while a transaction is open (the journal needs its own
    /// copy); prefer [`remove`](ItemStore::remove) when the value is not wanted.
    pub fn take(&mut self, id: ItemId) -> Option<Item> {
        let (item, revision) = self.detach(id)?;
        if self.journal.is_recording() {
            self.journal.push(Entry::Removed {
                item: item.clone(),
                revision,
            });
        }
        Some(item)
    }

    /// Replaces the content of `id`, keeping its identity and its position in
    /// the iteration order; returns whether the id was on the board.
    ///
    /// `new_item.id` is ignored and overwritten with `id`. The pre-image goes
    /// into the journal when a transaction is open, which is why this returns a
    /// flag rather than the old item — use [`mutate`](ItemStore::mutate) to
    /// inspect the old content.
    pub fn replace(&mut self, id: ItemId, mut new_item: Item) -> bool {
        let Some(slot) = self.live_slot(id) else {
            return false;
        };
        new_item.id = id;

        let revision = self.take_revision();
        let slot_ref = &mut self.slots[slot];
        let Some(before) = slot_ref.item.replace(new_item) else {
            return false;
        };
        let before_revision = std::mem::replace(&mut slot_ref.revision, revision);

        if self.slots[slot]
            .item
            .as_ref()
            .is_some_and(|now| now.nets != before.nets)
        {
            Self::unindex_nets(&mut self.nets, id, &before.nets);
            if let Some(now) = self.slots[slot].item.as_ref() {
                Self::index_nets(&mut self.nets, id, &now.nets);
            }
        }
        if self.journal.is_recording() {
            self.journal.push(Entry::Mutated {
                before,
                revision: before_revision,
            });
        }
        true
    }

    /// Mutates the item in place through `f`, journaling the pre-image first.
    ///
    /// The upstream contract is `UndoableObjects.saveForUndo(o)` **before** the
    /// first mutation (§13); here it cannot be forgotten, because the only
    /// route to a `&mut Item` inside a transaction goes through this method
    /// ([`get_mut`](ItemStore::get_mut) is the documented, assert-guarded
    /// no-journal escape hatch).
    ///
    /// The item's `id` is restored after `f` runs, and the per-net index is
    /// updated if `f` changed the net array.
    pub fn mutate<R>(&mut self, id: ItemId, f: impl FnOnce(&mut Item) -> R) -> Option<R> {
        let slot = self.live_slot(id)?;
        let recording = self.journal.is_recording();

        let (nets_before, pre_image) = {
            let slot_ref = &self.slots[slot];
            let item = slot_ref.item.as_ref()?;
            let pre_image = recording.then(|| Entry::Mutated {
                before: item.clone(),
                revision: slot_ref.revision,
            });
            (item.nets.clone(), pre_image)
        };

        let revision = self.take_revision();
        let out = {
            let slot_ref = &mut self.slots[slot];
            let item = slot_ref.item.as_mut()?;
            let out = f(item);
            item.id = id;
            slot_ref.revision = revision;
            out
        };

        if self.slots[slot]
            .item
            .as_ref()
            .is_some_and(|now| now.nets != nets_before)
        {
            Self::unindex_nets(&mut self.nets, id, &nets_before);
            if let Some(now) = self.slots[slot].item.as_ref() {
                Self::index_nets(&mut self.nets, id, &now.nets);
            }
        }
        if let Some(entry) = pre_image {
            self.journal.push(entry);
        }
        Some(out)
    }

    /// Direct mutable access, **without** journaling.
    ///
    /// The escape hatch for callers provably outside any transaction (board
    /// load, IO). Changing the net array through it corrupts the per-net index,
    /// and using it inside a transaction makes that transaction unrollable — the
    /// latter is `debug_assert`-guarded.
    pub fn get_mut(&mut self, id: ItemId) -> Option<&mut Item> {
        debug_assert!(
            !self.journal.is_recording(),
            "fr-board: get_mut inside an open transaction bypasses the journal; use mutate()"
        );
        let slot = self.live_slot(id)?;
        let revision = self.take_revision();
        let slot_ref = &mut self.slots[slot];
        slot_ref.revision = revision;
        slot_ref.item.as_mut()
    }

    /// Empties the board, keeping every allocation for reuse.
    ///
    /// `next_id` and the revision counter deliberately survive: ids are never
    /// reused for the lifetime of a store, not even across a clear.
    pub fn clear(&mut self) {
        self.slots.clear();
        self.free_slots.clear();
        self.order.clear();
        self.tombstones = 0;
        self.live = 0;
        for ids in self.nets.values_mut() {
            ids.clear();
        }
        self.journal.clear();
        self.undo_scratch.clear();
    }

    // ---- transactions (state mechanism 1) --------------------------------

    /// Opens a transaction and returns its [`Mark`]. O(1).
    ///
    /// Marks nest LIFO; see [`journal`](crate::journal) for what is and is not
    /// undone.
    pub fn begin(&mut self) -> Mark {
        self.journal.begin()
    }

    /// Undoes every mutation made since `mark` and closes its level.
    ///
    /// O(number of mutations), independent of board size. Panics if `mark` is
    /// stale or used out of LIFO order.
    pub fn rollback_to(&mut self, mark: Mark) {
        let mut scratch = std::mem::take(&mut self.undo_scratch);
        scratch.clear();
        self.journal.drain_since(mark, &mut scratch);
        for entry in scratch.drain(..).rev() {
            self.undo(entry);
        }
        self.undo_scratch = scratch;
        self.maybe_compact();
    }

    /// Accepts every mutation made since `mark` and closes its level. O(1).
    ///
    /// Panics if `mark` is stale or used out of LIFO order.
    pub fn commit(&mut self, mark: Mark) {
        self.journal.commit(mark);
        self.maybe_compact();
    }

    /// Number of currently open transactions.
    #[inline]
    pub fn transaction_depth(&self) -> usize {
        self.journal.depth()
    }

    /// Number of logged, not-yet-discarded mutations — the Δ a rollback replays.
    #[inline]
    pub fn journal_len(&self) -> usize {
        self.journal.len()
    }

    fn undo(&mut self, entry: Entry) {
        match entry {
            Entry::Inserted(id) => {
                self.detach(id);
            }
            Entry::Removed { item, revision } => {
                self.attach(item, revision);
            }
            Entry::Mutated { before, revision } => {
                let id = before.id;
                // Entries are replayed newest-first, so a removal that followed
                // this mutation has already been undone and the id is live
                // again. Anything else is a journal bug.
                let Some(slot) = self.live_slot(id) else {
                    debug_assert!(false, "fr-board: undoing a mutation of a dead id {id}");
                    return;
                };
                let nets_after = self.slots[slot].item.as_ref().map(|i| i.nets.clone());
                let slot_ref = &mut self.slots[slot];
                slot_ref.item = Some(before);
                slot_ref.revision = revision;
                if let Some(nets_after) = nets_after {
                    if self.slots[slot]
                        .item
                        .as_ref()
                        .is_some_and(|now| now.nets != nets_after)
                    {
                        Self::unindex_nets(&mut self.nets, id, &nets_after);
                        if let Some(now) = self.slots[slot].item.as_ref() {
                            Self::index_nets(&mut self.nets, id, &now.nets);
                        }
                    }
                }
            }
        }
    }

    // ---- snapshot / restore support (state mechanism 2) ------------------

    /// Overwrites `self` with a deep copy of `src`, reusing every allocation
    /// `self` already holds.
    ///
    /// The workhorse of [`BoardHistory`](crate::history::BoardHistory): with a
    /// pooled destination the slot arena, the ordered index and the per-net
    /// lists are memcpy'd into existing capacity, and
    /// [`Item::clone_from_ref`](crate::item::Item::clone_from_ref) keeps the
    /// per-item polyline buffers too.
    pub fn copy_from(&mut self, src: &ItemStore) {
        self.slots.clone_from(&src.slots);
        self.free_slots.clear();
        self.free_slots.extend_from_slice(&src.free_slots);
        self.order.clear();
        self.order.extend_from_slice(&src.order);
        self.tombstones = src.tombstones;
        self.live = src.live;

        // Keep the destination's per-net Vec allocations: empty the ones the
        // source does not have, refill the ones it does.
        for (net, ids) in self.nets.iter_mut() {
            if !src.nets.contains_key(net) {
                ids.clear();
            }
        }
        for (net, src_ids) in &src.nets {
            let dst = self.nets.entry(*net).or_default();
            dst.clear();
            dst.extend_from_slice(src_ids);
        }

        self.next_id = src.next_id;
        self.next_revision = src.next_revision;
        // A snapshot is only ever taken outside a transaction, so there is
        // nothing to copy; clearing keeps that invariant explicit.
        self.journal.clear();
        self.undo_scratch.clear();
    }

    /// Records into `delta` which items differ between `self` (the "before"
    /// state) and `other` (the "after" state).
    ///
    /// O(entries in the two ordered indices), no allocation beyond the delta's
    /// own lists: both indices are sorted by id, so this is a merge walk, and
    /// content changes are detected by comparing the O(1) revision stamps rather
    /// than the items themselves.
    ///
    /// `delta` is cleared first.
    pub fn diff_into(&self, other: &ItemStore, delta: &mut StoreDelta) {
        delta.clear();
        let (a, b) = (&self.order, &other.order);
        let (mut i, mut j) = (0usize, 0usize);
        while i < a.len() && j < b.len() {
            let (ea, eb) = (a[i], b[j]);
            match ea.id.cmp(&eb.id) {
                std::cmp::Ordering::Equal => {
                    match (ea.is_live(), eb.is_live()) {
                        (true, true) => {
                            if self.slots[ea.slot as usize].revision
                                != other.slots[eb.slot as usize].revision
                            {
                                delta.changed.push(ea.id);
                            }
                        }
                        (true, false) => delta.removed.push(ea.id),
                        (false, true) => delta.added.push(eb.id),
                        (false, false) => {}
                    }
                    i += 1;
                    j += 1;
                }
                std::cmp::Ordering::Less => {
                    if ea.is_live() {
                        delta.removed.push(ea.id);
                    }
                    i += 1;
                }
                std::cmp::Ordering::Greater => {
                    if eb.is_live() {
                        delta.added.push(eb.id);
                    }
                    j += 1;
                }
            }
        }
        for ea in &a[i..] {
            if ea.is_live() {
                delta.removed.push(ea.id);
            }
        }
        for eb in &b[j..] {
            if eb.is_live() {
                delta.added.push(eb.id);
            }
        }
    }

    /// Raises the id and revision counters to at least these values.
    ///
    /// Called by the history restore path after swapping in an older store: the
    /// restored state carries the *old* counters, and letting them go backwards
    /// would recycle ids — breaking `id > maxItemIdBefore` and, worse, giving two
    /// different items the same identity in a search tree that outlived the
    /// restore.
    pub(crate) fn raise_counters(&mut self, next_id: u32, next_revision: u64) {
        self.next_id = self.next_id.max(next_id);
        self.next_revision = self.next_revision.max(next_revision);
    }

    pub(crate) fn raw_counters(&self) -> (u32, u64) {
        (self.next_id, self.next_revision)
    }

    // ---- internals -------------------------------------------------------

    #[inline]
    fn take_revision(&mut self) -> u64 {
        let r = self.next_revision;
        self.next_revision += 1;
        r
    }

    fn alloc_slot(&mut self, item: Item, revision: u64) -> u32 {
        if let Some(slot) = self.free_slots.pop() {
            let slot_ref = &mut self.slots[slot as usize];
            slot_ref.item = Some(item);
            slot_ref.revision = revision;
            slot
        } else {
            self.slots.push(Slot {
                item: Some(item),
                revision,
            });
            (self.slots.len() - 1) as u32
        }
    }

    /// Removes `id` from every index and returns its item plus the revision it
    /// was written at, without touching the journal.
    fn detach(&mut self, id: ItemId) -> Option<(Item, u64)> {
        let pos = self.find(id).ok()?;
        let entry = self.order[pos];
        if !entry.is_live() {
            return None;
        }
        let slot = entry.slot;
        let item = self.slots[slot as usize].item.take()?;
        let revision = self.slots[slot as usize].revision;
        self.free_slots.push(slot);

        if pos + 1 == self.order.len() {
            // Removing the newest item: drop the entry outright, and with it
            // any trailing tombstones that were only kept because they were
            // interior.
            self.order.pop();
            while self.order.last().is_some_and(|last| !last.is_live()) {
                self.order.pop();
                self.tombstones -= 1;
            }
        } else {
            self.order[pos].slot = NO_SLOT;
            self.tombstones += 1;
        }
        self.live -= 1;
        Self::unindex_nets(&mut self.nets, id, &item.nets);
        self.maybe_compact();
        Some((item, revision))
    }

    /// Puts a previously detached item back under its own id — the rollback
    /// path. The id keeps its position in the iteration order.
    fn attach(&mut self, item: Item, revision: u64) {
        let id = item.id;
        Self::index_nets(&mut self.nets, id, &item.nets);
        let slot = self.alloc_slot(item, revision);
        match self.find(id) {
            Ok(pos) => {
                // The tombstone is still there (compaction is suppressed inside
                // a transaction), so this is O(log n).
                debug_assert!(!self.order[pos].is_live());
                self.order[pos].slot = slot;
                self.tombstones -= 1;
            }
            Err(pos) => self.order.insert(pos, OrderEntry { id, slot }),
        }
        self.live += 1;
    }

    /// Adds `id` to the per-net lists of every real net in `nets`.
    ///
    /// An associated function taking the map directly so callers can hold a
    /// borrow of the arena at the same time; it allocates nothing, and the
    /// duplicate check is a linear scan over an array of length ≤ 3.
    fn index_nets(index: &mut BTreeMap<i32, Vec<ItemId>>, id: ItemId, nets: &NetNos) {
        let slice = nets.as_slice();
        for (i, net) in slice.iter().copied().enumerate() {
            if net <= 0 || slice[..i].contains(&net) {
                continue;
            }
            let ids = index.entry(net).or_default();
            // Ascending by construction: an insert appends the new maximum, a
            // rollback splices an older id back into place.
            if let Err(pos) = ids.binary_search(&id) {
                ids.insert(pos, id);
            }
        }
    }

    fn unindex_nets(index: &mut BTreeMap<i32, Vec<ItemId>>, id: ItemId, nets: &NetNos) {
        for net in nets.iter().copied() {
            if net <= 0 {
                continue;
            }
            if let Some(ids) = index.get_mut(&net) {
                if let Ok(pos) = ids.binary_search(&id) {
                    ids.remove(pos);
                }
            }
        }
    }

    /// Drops tombstones once they outnumber the live entries.
    ///
    /// Suppressed while a transaction is open: rolling back a removal relies on
    /// finding the tombstone it left behind, which keeps that path O(log n)
    /// instead of an O(n) splice.
    fn maybe_compact(&mut self) {
        if self.journal.is_recording()
            || self.order.len() < MIN_COMPACT_LEN
            || (self.tombstones as usize) * 2 <= self.order.len()
        {
            return;
        }
        self.order.retain(|e| e.is_live());
        self.tombstones = 0;
    }

    /// Panics unless every internal index agrees with the arena.
    ///
    /// For tests and `debug_assert`-style use from the board layer; it is
    /// O(board) and allocates, so it is not on any hot path.
    pub fn assert_consistent(&self) {
        let mut live = 0u32;
        let mut tombstones = 0u32;
        let mut previous: Option<ItemId> = None;
        for entry in &self.order {
            if let Some(prev) = previous {
                assert!(prev < entry.id, "ordered index is not ascending by id");
            }
            previous = Some(entry.id);
            if entry.is_live() {
                live += 1;
                let item = self.slots[entry.slot as usize]
                    .item
                    .as_ref()
                    .expect("live ordered-index entry points at an empty slot");
                assert_eq!(
                    item.id, entry.id,
                    "item carries a different id than its index entry"
                );
                assert!(
                    entry.id.raw() < self.next_id,
                    "an item's id is at or beyond the next id to be handed out"
                );
            } else {
                tombstones += 1;
            }
        }
        assert_eq!(
            live, self.live,
            "live counter disagrees with the ordered index"
        );
        assert_eq!(
            tombstones, self.tombstones,
            "tombstone counter disagrees with the ordered index"
        );

        let occupied = self.slots.iter().filter(|s| s.item.is_some()).count();
        assert_eq!(
            occupied, live as usize,
            "occupied slots disagree with live items"
        );
        assert_eq!(
            self.free_slots.len() + occupied,
            self.slots.len(),
            "free list plus occupied slots does not cover the arena"
        );

        // Per-net index: ascending, duplicate-free, and exactly the live items
        // whose net array contains the net.
        for (net, ids) in &self.nets {
            let mut previous: Option<ItemId> = None;
            for id in ids {
                if let Some(prev) = previous {
                    assert!(prev < *id, "per-net index for net {net} is not ascending");
                }
                previous = Some(*id);
                let item = self
                    .get(*id)
                    .unwrap_or_else(|| panic!("per-net index for net {net} holds a dead id {id}"));
                assert!(
                    item.contains_net(*net),
                    "per-net index for net {net} holds {id}, which is not on that net"
                );
            }
        }
        for (id, item) in self.iter() {
            for net in item.nets.iter().copied().filter(|n| *n > 0) {
                assert!(
                    self.net_items(net).contains(&id),
                    "item {id} on net {net} is missing from the per-net index"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use fr_geometry::planar::{IntPoint, Polyline};

    use super::*;
    use crate::item::{FixedState, ItemKind, LayerRange, PolylineTrace, Via};

    fn trace(nets: &[i32]) -> Item {
        Item::new(
            NetNos::from_slice(nets),
            1,
            FixedState::Unfixed,
            ItemKind::Trace(PolylineTrace {
                polyline: Polyline::from_int_points(&[IntPoint::new(0, 0), IntPoint::new(1000, 0)]),
                layer: 0,
                half_width: 50,
            }),
        )
    }

    fn via(nets: &[i32]) -> Item {
        Item::new(
            NetNos::from_slice(nets),
            1,
            FixedState::Unfixed,
            ItemKind::Via(Via {
                center: IntPoint::new(0, 0),
                padstack_no: 0,
                layers: LayerRange::new(0, 1),
                attach_allowed: true,
                is_escape_via: false,
                escape_via_smd_layer: -1,
            }),
        )
    }

    #[test]
    fn ids_start_at_one_and_never_repeat() {
        let mut store = ItemStore::new();
        let a = store.insert(trace(&[1]));
        let b = store.insert(trace(&[1]));
        assert_eq!(a, ItemId::from_raw(1));
        assert_eq!(b, ItemId::from_raw(2));

        store.remove(a);
        store.remove(b);
        let c = store.insert(trace(&[1]));
        assert_eq!(c, ItemId::from_raw(3), "a freed id must never be reissued");
    }

    #[test]
    fn iteration_is_descending_by_id() {
        let mut store = ItemStore::new();
        let ids: Vec<ItemId> = (0..8).map(|_| store.insert(trace(&[1]))).collect();
        let mut expected = ids.clone();
        expected.reverse();
        assert_eq!(store.ids().collect::<Vec<_>>(), expected);

        // and it stays descending across a hole in the middle
        store.remove(ids[3]);
        let seen: Vec<ItemId> = store.ids().collect();
        assert_eq!(seen.len(), 7);
        assert!(seen.windows(2).all(|w| w[0] > w[1]));
        assert!(!seen.contains(&ids[3]));
    }

    #[test]
    fn removal_frees_the_slot_but_not_the_id() {
        let mut store = ItemStore::new();
        let a = store.insert(trace(&[1]));
        let slot_a = store.slot_of(a).unwrap();
        store.remove(a);
        assert!(!store.contains(a));
        assert_eq!(store.slot_of(a), None);

        let b = store.insert(trace(&[1]));
        assert_eq!(store.slot_of(b), Some(slot_a), "slots are recycled");
        assert_ne!(a, b, "ids are not");
        store.assert_consistent();
    }

    #[test]
    fn per_net_index_tracks_inserts_removals_and_net_changes() {
        let mut store = ItemStore::new();
        let a = store.insert(trace(&[1]));
        let b = store.insert(trace(&[1, 2]));
        let c = store.insert(via(&[2]));
        assert_eq!(store.net_items(1), &[a, b]);
        assert_eq!(store.net_items(2), &[b, c]);

        store.remove(b);
        assert_eq!(store.net_items(1), &[a]);
        assert_eq!(store.net_items(2), &[c]);

        store.mutate(c, |item| item.nets = NetNos::of(1)).unwrap();
        assert_eq!(store.net_items(1), &[a, c]);
        assert_eq!(store.net_items(2), &[] as &[ItemId]);
        store.assert_consistent();
    }

    #[test]
    fn a_repeated_net_number_is_indexed_once() {
        let mut store = ItemStore::new();
        let a = store.insert(trace(&[3, 3]));
        assert_eq!(store.net_items(3), &[a]);
        store.remove(a);
        assert!(store.net_items(3).is_empty());
        store.assert_consistent();
    }

    #[test]
    fn net_zero_and_negative_nets_are_not_indexed() {
        let mut store = ItemStore::new();
        store.insert(trace(&[0]));
        store.insert(trace(&[-4]));
        assert!(store.net_items(0).is_empty());
        assert!(store.net_items(-4).is_empty());
        assert_eq!(store.nets().count(), 0);
        store.assert_consistent();
    }

    #[test]
    fn rollback_undoes_inserts_removals_and_mutations() {
        let mut store = ItemStore::new();
        let kept = store.insert(trace(&[1]));
        let doomed = store.insert(trace(&[2]));
        let before = store.get(doomed).cloned().unwrap();

        let mark = store.begin();
        let added = store.insert(via(&[3]));
        store.remove(doomed);
        store
            .mutate(kept, |item| item.clearance_class = 99)
            .unwrap();
        assert_eq!(store.len(), 2);

        store.rollback_to(mark);
        assert_eq!(store.len(), 2);
        assert!(!store.contains(added));
        assert_eq!(store.get(doomed), Some(&before));
        assert_eq!(store.get(kept).unwrap().clearance_class, 1);
        store.assert_consistent();
    }

    #[test]
    fn a_rolled_back_insert_still_burns_its_id() {
        let mut store = ItemStore::new();
        let mark = store.begin();
        let doomed = store.insert(trace(&[1]));
        store.rollback_to(mark);
        let next = store.insert(trace(&[1]));
        assert!(next > doomed, "ids must stay monotone across rollback");
    }

    #[test]
    fn nested_transactions_commit_and_roll_back_independently() {
        let mut store = ItemStore::new();
        let base = store.insert(trace(&[1]));

        let outer = store.begin();
        let outer_item = store.insert(trace(&[2]));

        let inner = store.begin();
        let inner_item = store.insert(trace(&[3]));
        store.commit(inner);
        assert!(store.contains(inner_item));

        // The committed inner work is still part of the outer transaction.
        store.rollback_to(outer);
        assert!(!store.contains(inner_item));
        assert!(!store.contains(outer_item));
        assert!(store.contains(base));
        assert_eq!(store.transaction_depth(), 0);
        store.assert_consistent();
    }

    #[test]
    fn commit_of_the_outermost_level_clears_the_log() {
        let mut store = ItemStore::new();
        let mark = store.begin();
        store.insert(trace(&[1]));
        store.insert(trace(&[1]));
        assert_eq!(store.journal_len(), 2);
        store.commit(mark);
        assert_eq!(store.journal_len(), 0);
        assert_eq!(store.transaction_depth(), 0);
    }

    #[test]
    #[should_panic(expected = "stale mark")]
    fn reusing_a_mark_after_its_level_was_reopened_panics() {
        let mut store = ItemStore::new();
        let mark = store.begin();
        store.commit(mark);
        let _reopened = store.begin();
        store.commit(mark);
    }

    #[test]
    #[should_panic(expected = "already closed")]
    fn committing_an_already_closed_mark_panics() {
        let mut store = ItemStore::new();
        let mark = store.begin();
        store.commit(mark);
        store.commit(mark);
    }

    #[test]
    fn replace_keeps_identity_and_iteration_position() {
        let mut store = ItemStore::new();
        let a = store.insert(trace(&[1]));
        let b = store.insert(trace(&[1]));
        let c = store.insert(trace(&[1]));

        let mut replacement = via(&[5]);
        replacement.id = ItemId::from_raw(12345);
        assert!(store.replace(b, replacement));

        assert_eq!(store.get(b).unwrap().id, b);
        assert_eq!(store.ids().collect::<Vec<_>>(), vec![c, b, a]);
        assert_eq!(store.net_items(5), &[b]);
        assert_eq!(store.net_items(1), &[a, c]);
        store.assert_consistent();
    }

    #[test]
    fn revision_moves_forward_on_every_write_and_never_backwards_on_rollback() {
        let mut store = ItemStore::new();
        let a = store.insert(trace(&[1]));
        let r0 = store.revision_of(a).unwrap();

        let mark = store.begin();
        store.mutate(a, |item| item.clearance_class = 7).unwrap();
        let r1 = store.revision_of(a).unwrap();
        assert!(r1 > r0);
        store.rollback_to(mark);
        assert_eq!(store.revision_of(a), Some(r0));
        assert!(
            store.revision() > r1,
            "the store-wide counter must not rewind"
        );
    }

    #[test]
    fn tombstones_are_compacted_once_they_dominate() {
        let mut store = ItemStore::new();
        let ids: Vec<ItemId> = (0..200).map(|_| store.insert(trace(&[1]))).collect();
        // Remove the oldest 130, leaving 70 live behind interior tombstones
        // (the newest item survives, so nothing is popped off the tail).
        for id in ids.iter().take(130) {
            store.remove(*id);
        }
        assert_eq!(store.len(), 70);
        store.assert_consistent();
        assert!(
            store.order.len() < 200,
            "compaction never ran: {} entries left",
            store.order.len()
        );
        assert!(store
            .ids()
            .collect::<Vec<_>>()
            .windows(2)
            .all(|w| w[0] > w[1]));
    }

    #[test]
    fn diff_reports_added_removed_and_changed() {
        let mut before = ItemStore::new();
        let kept = before.insert(trace(&[1]));
        let doomed = before.insert(trace(&[1]));
        let touched = before.insert(trace(&[1]));

        let mut after = before.clone();
        after.remove(doomed);
        let added = after.insert(trace(&[2]));
        after.mutate(touched, |i| i.clearance_class = 3).unwrap();

        let mut delta = StoreDelta::new();
        before.diff_into(&after, &mut delta);
        assert_eq!(delta.removed, vec![doomed]);
        assert_eq!(delta.added, vec![added]);
        assert_eq!(delta.changed, vec![touched]);
        assert!(!delta.removed.contains(&kept));
    }

    #[test]
    fn copy_from_reproduces_the_source_exactly() {
        let mut src = ItemStore::new();
        for i in 0..50 {
            src.insert(trace(&[i % 5 + 1]));
        }
        for id in src.ids().collect::<Vec<_>>().iter().step_by(3) {
            src.remove(*id);
        }

        let mut dst = ItemStore::new();
        dst.insert(via(&[9]));
        dst.copy_from(&src);
        dst.assert_consistent();

        assert_eq!(dst.len(), src.len());
        assert_eq!(dst.ids().collect::<Vec<_>>(), src.ids().collect::<Vec<_>>());
        assert_eq!(
            dst.nets().collect::<Vec<_>>(),
            src.nets().collect::<Vec<_>>()
        );
        assert_eq!(dst.next_id(), src.next_id());
        let mut delta = StoreDelta::new();
        src.diff_into(&dst, &mut delta);
        assert!(delta.is_empty(), "copy_from left a difference: {delta:?}");
    }

    #[test]
    fn clone_is_an_independent_deep_copy() {
        // Mechanism 3 in its non-CoW form: a worker's board must not alias the
        // board it was cloned from.
        let mut original = ItemStore::new();
        let a = original.insert(trace(&[1]));
        let mut worker = original.clone();
        worker.mutate(a, |item| item.clearance_class = 42).unwrap();
        worker.insert(trace(&[2]));

        assert_eq!(original.get(a).unwrap().clearance_class, 1);
        assert_eq!(original.len(), 1);
        assert_eq!(worker.len(), 2);
    }
}
