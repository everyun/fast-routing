//! `autoroute/BoardHistory.java` — the score-ranked snapshot store the batch
//! loop restores from.
//!
//! | this module | upstream |
//! |---|---|
//! | [`BoardSnapshotStore::add`] | `BoardHistory.add` (`:48-80`) |
//! | [`BoardSnapshotStore::max_score`] | `BoardHistory.getMaxScore` (`:115-129`) |
//! | [`BoardSnapshotStore::restore`] | `BoardHistory.restoreBoard(int)` (`:134-160`) |
//! | [`BoardSnapshotStore::restore_best`] | `BoardHistory.restoreBestBoard` (`:162-164`) |
//! | [`board_hash`] | `BasicBoard.get_hash` (`BasicBoard.java:202-214`) |
//!
//! Semantics transcribed exactly (conformance appendix row 73): cap 30, dedup by
//! hash, at cap evict the worst entry **only** if the newcomer scores strictly
//! better, `restore_count <= max_restores` selects, `max_score` floors at `0`
//! and answers `0` for an empty store.
//!
//! # Storage
//!
//! Upstream stores `board.serialize(false)` byte arrays and materialises one
//! with Java deserialization. `ARCHITECTURE.md` specifies the replacement:
//!
//! > 2. board history = arena snapshot (memcpy of item+node Vecs), O(board) but
//! >    ~ms; restore must also restore tree state and preserves the room DB
//! >    interaction (restore invalidates per-item room caches — budgeted)
//!
//! That is what this is. An entry is a pooled
//! [`ItemStore`] — the items and nothing else — and
//! a restore is [`Board::swap_store`], which reconciles the search trees and the
//! connectivity index from the [`StoreDelta`] rather than rebuilding them. The
//! displaced store comes back out through the same buffer and goes straight back
//! into the pool, so a full thirty-deep history costs thirty *item* arenas whose
//! per-item allocations — every polyline `Vec<Line>` — are recycled across
//! snapshots, instead of thirty whole [`Board`] clones each carrying its own
//! copy of every search tree.
//!
//! [`fr_board::history::BoardHistory`] is the same mechanism as a LIFO stack;
//! this module cannot use it because a score-ranked store needs random-access
//! restore *without* discarding the newer entries, and because an entry here is
//! handed out up to [`MAXIMUM_TRIES_ON_THE_SAME_BOARD`] + 1 times and therefore
//! has to survive being restored.
//!
//! The callback [`BoardSnapshotStore::restore`] takes is the invalidation seam
//! for state *outside* the board: the autoroute room database and its per-item
//! caches. Everything inside the board is already reconciled when it runs.

use fr_board::board::Board;
use fr_board::item::ItemKind;
use fr_board::store::{ItemStore, StoreDelta};

/// `BoardHistory.MAX_HISTORY_SIZE` (`:29`) *(jar-verified)*.
pub const MAX_HISTORY_SIZE: usize = 30;

/// `BatchAutorouter.BOARD_RANK_LIMIT` (`:54`) — `= MAX_HISTORY_SIZE`, which is
/// what makes the rank check unreachable (conformance appendix row 60).
pub const BOARD_RANK_LIMIT: usize = MAX_HISTORY_SIZE;

/// `BatchAutorouter.MAXIMUM_TRIES_ON_THE_SAME_BOARD` (`:56`) *(jar-verified)*.
///
/// Read with upstream's `restoreCount <= maxAllowedRestoreCount`
/// (`BoardHistory.java:145`), so an entry is handed out at counts 0, 1, 2 and 3
/// — **four** restores, not three. Transcribed rather than corrected.
pub const MAXIMUM_TRIES_ON_THE_SAME_BOARD: u32 = 3;

/// FNV-1a over the board's routable state — [`board_hash`]'s mixer.
#[inline]
fn mix(state: &mut u64, value: u64) {
    *state ^= value;
    *state = state.wrapping_mul(0x0000_0100_0000_01b3);
}

#[inline]
fn mix_i32(state: &mut u64, value: i32) {
    mix(state, value as u32 as u64);
}

/// A content hash of the board, standing in for upstream's MD5 of the
/// serialized board (`BasicBoard.get_hash`).
///
/// Upstream hashes *everything*, ids included, and uses the digest only for
/// equality — dedup in [`BoardSnapshotStore::add`], the (commented-out)
/// same-hash stop, and log lines. Equality is all this has to preserve, so the
/// walk is over ascending items and hashes the fields a routing pass can
/// change: identity, net membership, fixed state, and the geometry of the two
/// classes the router creates and destroys. Pins, pours and outlines contribute
/// their id, class and layer span only — a pass never moves one, and hashing
/// their whole polygon on every pass would be the sort of O(board) tax
/// `perf-forensics` exists to remove.
///
/// FNV-1a rather than [`std::collections::hash_map::DefaultHasher`]: the
/// algorithm is fixed here, so the value cannot drift with a compiler upgrade,
/// and nothing in the project may key ordered behaviour on a default hasher.
pub fn board_hash(board: &Board) -> u64 {
    let mut state: u64 = 0xcbf2_9ce4_8422_2325;
    for (id, item) in board.store().iter_ascending() {
        mix(&mut state, u64::from(id.raw()));
        mix_i32(&mut state, item.clearance_class);
        mix_i32(&mut state, item.component_no);
        mix(&mut state, item.fixed_state as u64);
        for net in item.nets.iter().copied() {
            mix_i32(&mut state, net);
        }
        let layers = item.layer_range();
        mix_i32(&mut state, layers.first);
        mix_i32(&mut state, layers.last);
        match &item.kind {
            ItemKind::Trace(trace) => {
                mix(&mut state, 1);
                mix_i32(&mut state, trace.half_width);
                for line in &trace.polyline.arr {
                    mix_i32(&mut state, line.a.x);
                    mix_i32(&mut state, line.a.y);
                    mix_i32(&mut state, line.b.x);
                    mix_i32(&mut state, line.b.y);
                }
            }
            ItemKind::Via(via) => {
                mix(&mut state, 2);
                mix_i32(&mut state, via.center.x);
                mix_i32(&mut state, via.center.y);
                mix(&mut state, u64::from(via.padstack_no));
            }
            other => mix(&mut state, 3 + kind_tag(other)),
        }
    }
    state
}

fn kind_tag(kind: &ItemKind) -> u64 {
    match kind {
        ItemKind::Pin(_) => 0,
        ItemKind::Via(_) => 1,
        ItemKind::Trace(_) => 2,
        ItemKind::ObstacleArea(_) => 3,
        ItemKind::ConductionArea(_) => 4,
        ItemKind::BoardOutline(_) => 5,
        ItemKind::ComponentOutline(_) => 6,
    }
}

/// One retained board — `BoardHistory.BoardHistoryEntry` (`:200-213`).
///
/// The items only: everything else a [`Board`] carries is derived from them and
/// is reconciled by [`Board::swap_store`] on the way back in.
#[derive(Debug, Clone)]
struct Entry {
    store: ItemStore,
    hash: u64,
    score: f64,
    restore_count: u32,
}

/// What a successful restore did, for the caller's stop rules and logs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Restored {
    /// 1-based position in the score-descending order — `BoardHistory.getRank`.
    pub rank: usize,
    /// The restored board's score.
    pub score: f64,
    /// The entry's restore count *after* this restore.
    pub restore_count: u32,
}

/// The score-ranked snapshot store.
///
/// ```
/// # use fr_router::batch::history::BoardSnapshotStore;
/// # use fr_board::board::Board;
/// # fn demo(board: &mut Board) {
/// let mut history = BoardSnapshotStore::new();
/// history.add(board, 900.0);            // before each pass
/// // ... a pass makes the board worse ...
/// history.restore(board, 3, |_, _| {}); // back to the best one
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct BoardSnapshotStore {
    entries: Vec<Entry>,
    /// Retired snapshot buffers, kept for their allocations.
    pool: Vec<ItemStore>,
    capacity: usize,
    delta: StoreDelta,
}

impl Default for BoardSnapshotStore {
    fn default() -> Self {
        BoardSnapshotStore::new()
    }
}

impl BoardSnapshotStore {
    /// A store capped at [`MAX_HISTORY_SIZE`].
    pub fn new() -> BoardSnapshotStore {
        BoardSnapshotStore::with_capacity(MAX_HISTORY_SIZE)
    }

    /// A store with a custom cap — upstream's package-private test constructor
    /// (`BoardHistory.java:41-44`).
    pub fn with_capacity(capacity: usize) -> BoardSnapshotStore {
        BoardSnapshotStore {
            entries: Vec::new(),
            pool: Vec::new(),
            capacity: capacity.max(1),
            delta: StoreDelta::new(),
        }
    }

    /// How many boards are retained.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when nothing is retained.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The cap.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Drops every retained board — `BoardHistory.clear` (`:82-84`).
    ///
    /// The buffers stay pooled: a batch loop clears the history at the top of
    /// every run and would otherwise pay for the arenas twice.
    pub fn clear(&mut self) {
        self.pool
            .extend(self.entries.drain(..).map(|entry| entry.store));
    }

    /// Releases the pooled buffers. Only useful when the store is done with.
    pub fn shrink(&mut self) {
        self.pool.clear();
    }

    /// The best retained score, **floored at 0** and `0` for an empty store
    /// (`:115-129`, conformance appendix row 73).
    ///
    /// The floor is load-bearing: the batch loop's restore test is
    /// `max_score() > score_after`, so on a board whose every state scores 0 the
    /// test can never fire, which is what stops the restore cycle from running
    /// forever.
    pub fn max_score(&self) -> f64 {
        self.entries
            .iter()
            .map(|entry| entry.score)
            .fold(0.0, f64::max)
    }

    /// Whether a board with this hash is already retained
    /// (`BoardHistory.contains`, `:86-104`).
    pub fn contains_hash(&self, hash: u64) -> bool {
        self.entries.iter().any(|entry| entry.hash == hash)
    }

    /// `BoardHistory.add` (`:48-80`): dedup by hash; at the cap, evict the worst
    /// entry **only** when the newcomer scores strictly better, and otherwise
    /// skip the snapshot entirely.
    ///
    /// The caller passes the score because it already has it — the batch loop's
    /// pre-pass board is the previous pass's post-pass board, whose score it
    /// scored to make its stop decisions. Upstream recomputes a full
    /// `BoardStatistics` here (and a second dead one at `:943`); this is the
    /// same number for strictly less work.
    ///
    /// Returns whether the board was retained.
    pub fn add(&mut self, board: &Board, score: f64) -> bool {
        let hash = board_hash(board);
        if self.contains_hash(hash) {
            return false;
        }
        if self.entries.len() >= self.capacity {
            let Some((worst_index, worst_score)) = self
                .entries
                .iter()
                .enumerate()
                .map(|(index, entry)| (index, entry.score))
                .reduce(|worst, candidate| if candidate.1 < worst.1 { candidate } else { worst })
            else {
                return false;
            };
            if score <= worst_score {
                return false;
            }
            self.pool.push(self.entries.remove(worst_index).store);
        }
        let mut store = self.pool.pop().unwrap_or_default();
        // O(board), reusing the retired buffer's allocations down to the
        // individual polyline `Vec<Line>` (`ItemStore::copy_from`).
        store.copy_from(board.store());
        self.entries.push(Entry {
            store,
            hash,
            score,
            restore_count: 0,
        });
        true
    }

    /// `BoardHistory.restoreBoard(maxAllowedRestoreCount)` (`:134-160`): sort by
    /// score descending, take the first entry with
    /// `restore_count <= max_restores`, increment it, and materialise it into
    /// `board`.
    ///
    /// `max_restores == 0` means "unlimited" — upstream's `<= 0` branch at
    /// `:135-137`, which is how `restoreBestBoard` asks for the best one
    /// regardless.
    ///
    /// `on_restore` is the dependent-state invalidation callback described in
    /// the module docs; it receives the restored board and the [`StoreDelta`]
    /// that describes how to get derived state from where it was to where it
    /// must be. Returns `None` when every entry is out of tries, which is the
    /// batch loop's "the router was not able to improve the board" stop.
    pub fn restore(
        &mut self,
        board: &mut Board,
        max_restores: u32,
        on_restore: impl FnOnce(&Board, &StoreDelta),
    ) -> Option<Restored> {
        // `List.sort` is stable, so entries of equal score keep insertion order
        // and the choice is deterministic.
        self.entries
            .sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        let allowed = if max_restores == 0 {
            u32::MAX
        } else {
            max_restores
        };
        let index = self
            .entries
            .iter()
            .position(|entry| entry.restore_count <= allowed)?;

        let restored = {
            let entry = &mut self.entries[index];
            entry.restore_count += 1;
            Restored {
                rank: index + 1,
                score: entry.score,
                restore_count: entry.restore_count,
            }
        };

        // The entry must survive being restored — it is handed out up to
        // `MAXIMUM_TRIES_ON_THE_SAME_BOARD + 1` times — so the *copy* is what
        // goes into the board, and the buffer it displaces is what comes back
        // for the next snapshot to fill.
        let mut buffer = self.pool.pop().unwrap_or_default();
        buffer.copy_from(&self.entries[index].store);
        board.swap_store(&mut buffer, &mut self.delta);
        self.pool.push(buffer);

        on_restore(board, &self.delta);
        Some(restored)
    }

    /// `BoardHistory.restoreBestBoard` (`:162-164`) — the best entry, whatever
    /// its restore count.
    pub fn restore_best(
        &mut self,
        board: &mut Board,
        on_restore: impl FnOnce(&Board, &StoreDelta),
    ) -> Option<Restored> {
        self.restore(board, 0, on_restore)
    }

    /// The delta produced by the last restore, for diagnostics.
    pub fn last_delta(&self) -> &StoreDelta {
        &self.delta
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestFixture;
    use fr_board::item::{FixedState, ItemId};
    use fr_board::net_nos::NetNos;
    use fr_geometry::planar::IntPoint;

    fn stub_trace(board: &mut Board, offset: i32) {
        board
            .insert_trace_points(
                &[IntPoint::new(offset, 0), IntPoint::new(offset, 400)],
                0,
                50,
                NetNos::of(1),
                1,
                FixedState::Unfixed,
                None,
            )
            .expect("the stub inserts");
    }

    #[test]
    fn the_hash_changes_with_the_wiring_and_not_with_a_reread() {
        let mut fixture = TestFixture::two_layer_with_pads();
        let before = board_hash(&fixture.board);
        assert_eq!(before, board_hash(&fixture.board), "the hash is a function");
        stub_trace(&mut fixture.board, 100);
        assert_ne!(before, board_hash(&fixture.board));
    }

    #[test]
    fn an_identical_board_is_not_retained_twice() {
        let fixture = TestFixture::two_layer_with_pads();
        let mut history = BoardSnapshotStore::new();
        assert!(history.add(&fixture.board, 500.0));
        assert!(!history.add(&fixture.board, 900.0), "same hash, same board");
        assert_eq!(history.len(), 1);
        assert_eq!(history.max_score(), 500.0);
    }

    #[test]
    fn the_max_score_is_floored_at_zero_and_empty_reads_zero() {
        let fixture = TestFixture::two_layer_with_pads();
        let mut history = BoardSnapshotStore::new();
        assert_eq!(history.max_score(), 0.0);
        history.add(&fixture.board, -17.0);
        assert_eq!(history.max_score(), 0.0, "negative scores read as 0");
    }

    /// `BoardHistory.add:53-77` — at the cap the newcomer must beat the *worst*
    /// entry strictly, and otherwise no snapshot is taken at all.
    #[test]
    fn at_the_cap_only_a_strictly_better_board_evicts_the_worst() {
        let mut fixture = TestFixture::two_layer_with_pads();
        let mut history = BoardSnapshotStore::with_capacity(3);
        for (index, score) in [10.0, 30.0, 20.0].iter().enumerate() {
            stub_trace(&mut fixture.board, 100 * (index as i32 + 1));
            assert!(history.add(&fixture.board, *score));
        }
        assert_eq!(history.len(), 3);

        stub_trace(&mut fixture.board, 1_000);
        assert!(!history.add(&fixture.board, 10.0), "equal to the worst");
        assert_eq!(history.len(), 3);

        assert!(history.add(&fixture.board, 11.0), "beats the worst");
        assert_eq!(history.len(), 3, "the cap holds");
        assert_eq!(history.max_score(), 30.0);
    }

    /// `restoreCount <= MAXIMUM_TRIES_ON_THE_SAME_BOARD` with the constant at 3
    /// hands the same entry out at counts 0, 1, 2 and 3 — four times.
    #[test]
    fn one_entry_is_restorable_four_times_under_the_limit_of_three() {
        let mut fixture = TestFixture::two_layer_with_pads();
        let mut history = BoardSnapshotStore::new();
        history.add(&fixture.board, 100.0);
        for expected in 1..=4 {
            let restored = history
                .restore(&mut fixture.board, MAXIMUM_TRIES_ON_THE_SAME_BOARD, |_, _| {})
                .expect("tries left");
            assert_eq!(restored.restore_count, expected);
            assert_eq!(restored.rank, 1);
        }
        assert!(history
            .restore(&mut fixture.board, MAXIMUM_TRIES_ON_THE_SAME_BOARD, |_, _| {})
            .is_none());
        // `restoreBestBoard` asks for the best board regardless of the count.
        assert!(history.restore_best(&mut fixture.board, |_, _| {}).is_some());
    }

    #[test]
    fn a_restore_puts_the_recorded_board_back_and_reports_the_delta() {
        let mut fixture = TestFixture::two_layer_with_pads();
        let items_before = fixture.board.store().len();
        let mut history = BoardSnapshotStore::new();
        history.add(&fixture.board, 100.0);

        stub_trace(&mut fixture.board, 100);
        stub_trace(&mut fixture.board, 200);
        assert_eq!(fixture.board.store().len(), items_before + 2);

        let mut seen = StoreDelta::new();
        history
            .restore(&mut fixture.board, 3, |_, delta| seen = delta.clone())
            .expect("one entry");
        assert_eq!(fixture.board.store().len(), items_before);
        assert_eq!(seen.removed.len(), 2, "the two stubs are gone: {seen:?}");
        assert!(seen.added.is_empty());
    }

    #[test]
    fn the_best_board_wins_the_restore_and_the_rank_is_its_position() {
        let mut fixture = TestFixture::two_layer_with_pads();
        let mut history = BoardSnapshotStore::new();
        history.add(&fixture.board, 10.0);
        stub_trace(&mut fixture.board, 100);
        let best_items = fixture.board.store().len();
        history.add(&fixture.board, 900.0);
        stub_trace(&mut fixture.board, 200);
        history.add(&fixture.board, 20.0);

        stub_trace(&mut fixture.board, 300);
        let restored = history.restore(&mut fixture.board, 3, |_, _| {}).unwrap();
        assert_eq!(restored.score, 900.0);
        assert_eq!(restored.rank, 1, "rank is the score-descending position");
        assert_eq!(fixture.board.store().len(), best_items);
    }

    // ---- the pooled-store storage ---------------------------------------

    /// The items (id and rendered content), the tree's leaf count, what a
    /// whole-board tree query answers, and each net's component count.
    type Observable = (Vec<(u32, String)>, usize, Vec<ItemId>, Vec<usize>);

    /// The whole board's observable state, for the equivalence test: the items,
    /// what the search trees answer, and what the connectivity index answers.
    fn observable(board: &mut Board) -> Observable {
        let items = board
            .store()
            .iter_ascending()
            .map(|(id, item)| (id.raw(), format!("{item:?}")))
            .collect();
        let leaves = board.default_tree().leaf_count();
        let mut hits = Vec::new();
        board.overlapping_items(
            &fr_board::shape::TileShape::Box(fr_geometry::planar::IntBox::new(
                -100_000, -100_000, 100_000, 100_000,
            )),
            None,
            &[],
            &mut hits,
        );
        hits.sort_unstable();
        let nets: Vec<i32> = board.store().nets().map(|(net, _)| net).collect();
        let components = nets
            .into_iter()
            .map(|net| board.net_component_count(net))
            .collect();
        (items, leaves, hits, components)
    }

    /// The property the storage change has to preserve: restoring through
    /// [`Board::swap_store`] must land on the same board a whole-board clone
    /// would have, trees and connectivity included.
    #[test]
    fn a_pooled_restore_equals_a_whole_board_clone_swap() {
        let mut fixture = TestFixture::two_layer_with_pads();
        stub_trace(&mut fixture.board, 100);
        // The reference: what upstream's deserialize — and this module's
        // previous `Board::clone_from` — would put back.
        let reference = fixture.board.clone();

        let mut history = BoardSnapshotStore::new();
        assert!(history.add(&fixture.board, 100.0));

        // Move the board away from the snapshot in all three directions the
        // delta distinguishes: add, remove, and mutate in place.
        let doomed = fixture
            .board
            .store()
            .iter()
            .find(|(_, item)| matches!(item.kind, ItemKind::Trace(_)))
            .map(|(id, _)| id)
            .expect("the stub is a trace");
        stub_trace(&mut fixture.board, 200);
        stub_trace(&mut fixture.board, 300);
        fixture.board.remove_item(doomed);

        let mut seen = StoreDelta::new();
        history
            .restore(&mut fixture.board, 3, |_, delta| seen = delta.clone())
            .expect("one entry");
        fixture.board.assert_consistent();

        let mut clone_swapped = fixture.board.clone();
        clone_swapped.clone_from(&reference);

        assert_eq!(
            observable(&mut fixture.board),
            observable(&mut clone_swapped),
            "the pooled restore diverged from the clone swap: {seen:?}"
        );
        assert!(seen.added.contains(&doomed), "{seen:?}");
        assert_eq!(seen.removed.len(), 2, "the two new stubs go away: {seen:?}");
    }

    /// An entry survives being restored, so the same board can come back four
    /// times — the pooled buffer must be a copy, not the entry itself.
    #[test]
    fn a_restored_entry_is_still_restorable() {
        let mut fixture = TestFixture::two_layer_with_pads();
        let items_before = fixture.board.store().len();
        let mut history = BoardSnapshotStore::new();
        history.add(&fixture.board, 100.0);

        for _ in 0..4 {
            stub_trace(&mut fixture.board, 100);
            assert_eq!(fixture.board.store().len(), items_before + 1);
            history
                .restore(&mut fixture.board, MAXIMUM_TRIES_ON_THE_SAME_BOARD, |_, _| {})
                .expect("tries left");
            assert_eq!(fixture.board.store().len(), items_before);
            fixture.board.assert_consistent();
        }
    }

    /// The point of the storage change: a retained pass is one item arena, and
    /// the buffers are recycled rather than reallocated.
    #[test]
    fn evicted_and_cleared_buffers_are_pooled() {
        let mut fixture = TestFixture::two_layer_with_pads();
        let mut history = BoardSnapshotStore::with_capacity(2);
        for (index, score) in [10.0, 30.0].iter().enumerate() {
            stub_trace(&mut fixture.board, 100 * (index as i32 + 1));
            assert!(history.add(&fixture.board, *score));
        }
        assert_eq!(history.pool.len(), 0);

        stub_trace(&mut fixture.board, 900);
        assert!(history.add(&fixture.board, 20.0), "beats the worst");
        assert_eq!(history.pool.len(), 0, "the evicted buffer was refilled");

        history.clear();
        assert!(history.is_empty());
        assert_eq!(history.pool.len(), 2, "clearing keeps the arenas");
        history.shrink();
        assert_eq!(history.pool.len(), 0);
    }
}
