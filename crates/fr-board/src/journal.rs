//! State mechanism 1 of 3: the **transaction journal**.
//!
//! `ARCHITECTURE.md`, fr-board section:
//!
//! > 1. transaction = command log; snapshot O(1), rollback O(Δ) — shove/insert
//!
//! This is the primitive the shove engine and the connection inserter run on:
//! try a mutation, and if the result violates a precondition (a trace tail
//! appeared, the shove ran out of budget, the inserted trace could not be
//! normalized), undo exactly the mutations that were made and nothing else.
//! Upstream implements it with `UndoableObjects.generateSnapshot()` /
//! `undo()`, whose cost is a `ConcurrentSkipListMap` level bump plus a clone of
//! every touched object (`v2-vs-v19-and-scope.md` §13); here it is an append to
//! a `Vec` and a reverse replay.
//!
//! # What the log stores
//!
//! One entry per mutation, holding whatever is needed to reverse it:
//!
//! | operation | entry | undo |
//! |---|---|---|
//! | insert | the new id | remove it |
//! | remove | the removed item (moved in, not cloned) | reinsert under its own id |
//! | mutate | the pre-image | write the pre-image back |
//!
//! `remove` is the common case during ripup and shove and costs no clone at
//! all: the item is moved out of the arena slot and into the log. `mutate`
//! costs one [`Item::clone`](crate::item::Item) — the same price upstream pays
//! in `saveForUndo`.
//!
//! # What the log deliberately does *not* undo
//!
//! The id counter and the revision counter. Ids must never be recycled (see
//! [`ItemId`](crate::item::ItemId)), so an insert that is rolled back burns its
//! id permanently; upstream's `idGenerator` behaves the same way. Revisions are
//! monotone for the same reason: [`BoardHistory`](crate::history::BoardHistory)
//! uses `(id, revision)` as a content-version key and would be fooled by a
//! counter that goes backwards.
//!
//! # Nesting
//!
//! Marks nest LIFO. An inner [`commit`](crate::store::ItemStore::commit) does
//! not discard the inner entries — the enclosing transaction can still roll
//! them back — it only closes the level. Only when the outermost level closes
//! is the log cleared. That is what makes `commit` O(1) at every depth.

use crate::item::{Item, ItemId};

/// A position in the journal, returned by
/// [`ItemStore::begin`](crate::store::ItemStore::begin).
///
/// Marks are values, not handles into the store, and carry a sequence number so
/// that using a stale one (rolling back or committing the same level twice, or
/// out of LIFO order) is caught instead of silently corrupting the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    /// Nesting depth this mark opened, i.e. its index in the open-level stack.
    pub(crate) level: u32,
    /// Journal length when the mark was taken; rollback replays down to here.
    pub(crate) log_len: u32,
    /// Distinguishes this mark from any later mark at the same level.
    pub(crate) seq: u64,
}

impl Mark {
    /// Nesting depth of the transaction this mark opened (0 = outermost).
    #[inline]
    pub fn level(&self) -> u32 {
        self.level
    }
}

/// One reversible mutation.
#[derive(Debug, Clone)]
pub(crate) enum Entry {
    /// An item was inserted under this id.
    Inserted(ItemId),
    /// An item was removed; the entry owns it.
    Removed { item: Item, revision: u64 },
    /// An item's content was overwritten; the entry owns the pre-image.
    Mutated { before: Item, revision: u64 },
}

/// The append-only command log behind
/// [`ItemStore`](crate::store::ItemStore)'s transaction API.
///
/// The journal is a field of the store rather than a free-standing object the
/// caller passes around: every mutating store method has to consult it, and
/// threading a second `&mut` through the router's call graph (shove calls
/// insert calls split calls remove) would fight the borrow checker for no gain.
/// Its entire API is therefore `pub(crate)`; the public surface is
/// [`Mark`] plus the three store methods.
#[derive(Debug, Clone, Default)]
pub struct Journal {
    entries: Vec<Entry>,
    /// Journal length at each currently open level.
    open: Vec<(u32, u64)>,
    next_seq: u64,
}

impl Journal {
    /// An empty journal with no open transaction.
    pub fn new() -> Self {
        Journal::default()
    }

    /// Number of currently open (nested) transactions.
    #[inline]
    pub fn depth(&self) -> usize {
        self.open.len()
    }

    /// Number of logged, not-yet-discarded mutations.
    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when nothing is logged.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// True while at least one transaction is open — the flag every mutating
    /// store method branches on. When it is false, mutations cost nothing
    /// extra, which matters because board load inserts tens of thousands of
    /// items outside any transaction.
    #[inline]
    pub(crate) fn is_recording(&self) -> bool {
        !self.open.is_empty()
    }

    /// Opens a transaction level. O(1).
    pub(crate) fn begin(&mut self) -> Mark {
        let seq = self.next_seq;
        self.next_seq += 1;
        let log_len = self.entries.len() as u32;
        let level = self.open.len() as u32;
        self.open.push((log_len, seq));
        Mark {
            level,
            log_len,
            seq,
        }
    }

    /// Appends an entry. Callers must check [`is_recording`](Journal::is_recording)
    /// first; this does not.
    #[inline]
    pub(crate) fn push(&mut self, entry: Entry) {
        self.entries.push(entry);
    }

    /// Closes `mark`'s level without undoing anything.
    ///
    /// Entries survive while an outer level is still open, so that the outer
    /// transaction can still roll them back. Closing the outermost level clears
    /// the log.
    pub(crate) fn commit(&mut self, mark: Mark) {
        self.validate(mark, "commit");
        self.open.truncate(mark.level as usize);
        if self.open.is_empty() {
            self.entries.clear();
        }
    }

    /// Closes `mark`'s level and moves the entries logged since it was taken
    /// into `out`, in log order.
    ///
    /// They leave the journal rather than being replayed in place because
    /// undoing them needs `&mut` access to the store, which owns the journal;
    /// `out` is the store's reusable scratch buffer, so a rollback allocates
    /// nothing.
    pub(crate) fn drain_since(&mut self, mark: Mark, out: &mut Vec<Entry>) {
        self.validate(mark, "rollback_to");
        self.open.truncate(mark.level as usize);
        out.extend(self.entries.drain(mark.log_len as usize..));
    }

    /// Discards everything. Used by the history restore path, which replaces
    /// the whole store and therefore invalidates every logged pre-image.
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.open.clear();
    }

    fn validate(&self, mark: Mark, op: &str) {
        let level = mark.level as usize;
        assert!(
            level < self.open.len(),
            "fr-board journal: {op} on a mark whose level ({level}) is already closed \
             (open levels: {})",
            self.open.len()
        );
        assert!(
            self.open[level].1 == mark.seq,
            "fr-board journal: {op} on a stale mark (level {level} was reopened); \
             transactions must be committed or rolled back in LIFO order"
        );
        debug_assert!(mark.log_len as usize <= self.entries.len());
    }
}
