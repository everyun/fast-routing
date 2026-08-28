//! Adversarial storm for [`Board::swap_store`] / [`Board::adopt_store`] — state
//! mechanism 2 as `fr_router::batch::history::BoardSnapshotStore` now uses it.
//!
//! The unit tests in `fr-board` prove the mechanism on hand-built three-item
//! boards. This file proves the *equivalence* that the storage change in
//! `BoardSnapshotStore` rests on, under a randomized workload:
//!
//! > restoring a snapshot through `swap_store` lands on the same board that
//! > `Board::clone_from` of a whole-board clone would.
//!
//! "The same board" is not "the same items": the whole point of `swap_store` is
//! that it reconciles the *derived* state — the slot-indexed search trees and
//! the connectivity index — from a [`StoreDelta`] instead of rebuilding it. A
//! delta that under-reports leaves a stale tree leaf pointing at a recycled
//! slot, which no item comparison can see. So the oracle below reads the trees
//! and the ratsnest, not the arena.
//!
//! Three things the workload deliberately does, because each one is a way the
//! delta can be wrong:
//!
//! 1. **Transaction rollbacks between snapshots**, so the board drifts away from
//!    the snapshot through the journal as well as through plain edits.
//! 2. **Repeated restores of the same snapshot**, interleaved with further
//!    mutation — `BoardSnapshotStore` hands one entry out up to
//!    `MAXIMUM_TRIES_ON_THE_SAME_BOARD + 1` times, so a restore that consumed
//!    or aliased its entry would only show up on the second try.
//! 3. **Restores of a snapshot taken before an earlier restore**, so the id and
//!    revision counters have to stay monotone across a non-LIFO order.
//!
//! # What this file does *not* reach
//!
//! `ItemStore::diff_into_relocating`'s extra clause — same id, same revision,
//! different arena slot — is never observed here, and
//! [`delta_classes_are_actually_exercised`] records that as a measured fact
//! rather than leaving it to look like coverage. It is not for want of trying:
//! `ItemStore::rollback_to` replays the journal newest-first against a LIFO free
//! list, so every re-attached item provably lands back in the slot it left, and
//! `maybe_compact` only drops tombstones from the ordered index without
//! renumbering slots. The clause is defensive, and defensive in the safe
//! direction — an over-reported id costs one tile-shape rebuild, an
//! under-reported one is a stale tree leaf. Treat it as unproven, not as tested.

use fr_board::item::{FixedState, ItemKind};
use fr_board::shape::TileShape;
use fr_board::{
    Board, ClearanceTable, ItemId, ItemStore, LayerStructure, NetNos, Padstack, StoreDelta,
};
use fr_geometry::planar::{IntBox, IntPoint, Polyline};

const LAYERS: usize = 3;

/// xorshift64* — deterministic and dependency-free, as elsewhere in this suite.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: u32) -> u32 {
        (self.next_u64() >> 32) as u32 % bound.max(1)
    }
}

fn board() -> Board {
    let mut board = Board::new(
        LayerStructure::signal_layers(LAYERS),
        ClearanceTable::uniform(3, 3, 20),
    );
    board.padstacks_mut().push(Padstack::uniform(
        0,
        (LAYERS - 1) as i32,
        TileShape::Box(IntBox::new(-40, -40, 40, 40)),
    ));
    board
}

/// One trace, placed from the rng so the storm's boards actually overlap and
/// the trees have to do work.
fn insert_trace(board: &mut Board, rng: &mut Rng) -> Option<ItemId> {
    let x = rng.below(9_000) as i32;
    let y = rng.below(9_000) as i32;
    let polyline = Polyline::from_int_points(&[
        IntPoint::new(x, y),
        IntPoint::new(x + 300 + rng.below(900) as i32, y),
        IntPoint::new(x + 300 + rng.below(900) as i32, y + 700),
    ]);
    board.insert_trace_without_cleaning(
        polyline,
        rng.below(LAYERS as u32) as i32,
        25 + rng.below(40) as i32,
        NetNos::of(1 + rng.below(6) as i32),
        1,
        FixedState::Unfixed,
    )
}

fn live_ids(board: &Board) -> Vec<ItemId> {
    board
        .store()
        .iter()
        .filter(|(_, item)| matches!(item.kind, ItemKind::Trace(_)))
        .map(|(id, _)| id)
        .collect()
}

/// The population the storm holds the board near. Without a floor the mixture
/// below drains to nothing within a few dozen operations and every snapshot
/// comparison becomes trivially equal — see [`delta_classes_are_actually_exercised`].
const POPULATION: usize = 40;

/// A burst of mutation, holding the board near [`POPULATION`] items.
///
/// The last arm drives the journal: a handful of removals and inserts inside a
/// transaction that is then thrown away, so the board reaches a snapshot
/// comparison having gone through `rollback_to` and not only through plain
/// edits.
fn storm(board: &mut Board, rng: &mut Rng, ops: u32) {
    for _ in 0..ops {
        let ids = live_ids(board);
        let hungry = ids.len() < POPULATION;
        match rng.below(6) {
            0..=2 if hungry => {
                insert_trace(board, rng);
            }
            0..=2 => {
                let id = ids[rng.below(ids.len() as u32) as usize];
                board.remove_item(id);
            }
            3 | 4 => {
                if !ids.is_empty() {
                    let id = ids[rng.below(ids.len() as u32) as usize];
                    board.remove_item(id);
                }
                insert_trace(board, rng);
            }
            _ => {
                // Free a handful of slots inside a transaction, insert into
                // them, then throw the whole thing away: the removed items come
                // back through a free list the inserts reordered.
                let mark = board.begin();
                for _ in 0..4 {
                    if ids.is_empty() {
                        break;
                    }
                    let id = ids[rng.below(ids.len() as u32) as usize];
                    board.remove_item(id);
                }
                for _ in 0..3 {
                    insert_trace(board, rng);
                }
                board.rollback_to(mark);
            }
        }
    }
}

/// Everything a caller can observe about the board *through the derived state*:
/// the items, the tree's leaf count, what a whole-board tree query answers, and
/// what the connectivity index answers per net. An under-reported delta shows up
/// in the third and fourth components, never in the first.
type Observable = (Vec<(u32, String)>, usize, Vec<ItemId>, Vec<(i32, usize)>);

fn observable(board: &mut Board) -> Observable {
    let items = board
        .store()
        .iter_ascending()
        .map(|(id, item)| (id.raw(), format!("{item:?}")))
        .collect();
    let leaves = board.default_tree().leaf_count();
    let mut hits = Vec::new();
    board.overlapping_items(
        &TileShape::Box(IntBox::new(-1_000_000, -1_000_000, 1_000_000, 1_000_000)),
        None,
        &[],
        &mut hits,
    );
    hits.sort_unstable();
    hits.dedup();
    let nets: Vec<i32> = board.store().nets().map(|(net, _)| net).collect();
    let components = nets
        .into_iter()
        .map(|net| (net, board.net_component_count(net)))
        .collect();
    (items, leaves, hits, components)
}

/// The headline property: a `swap_store` restore is indistinguishable from a
/// whole-board `clone_from`, over a randomized workload, at every restore.
#[test]
fn a_swap_store_restore_matches_a_whole_board_clone_under_a_mutation_storm() {
    for seed in 0..24u64 {
        let mut rng = Rng::new(0xADD0_5709 ^ seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut live = board();
        storm(&mut live, &mut rng, 120);

        // Two snapshots, taken at different points, so the restore order below
        // is not a stack.
        let early_store = live.store().clone();
        let early_board = live.clone();
        storm(&mut live, &mut rng, 30);
        let late_store = live.store().clone();
        let late_board = live.clone();

        let mut buffer = ItemStore::new();
        let mut delta = StoreDelta::new();

        // Restore the *late* snapshot four times, each time after moving the
        // board away again — the `MAXIMUM_TRIES_ON_THE_SAME_BOARD` pattern.
        for round in 0..4 {
            storm(&mut live, &mut rng, 20);
            buffer.copy_from(&late_store);
            live.swap_store(&mut buffer, &mut delta);
            live.assert_consistent();

            let mut reference = late_board.clone();
            assert_eq!(
                observable(&mut live),
                observable(&mut reference),
                "seed {seed} round {round}: the late snapshot diverged from the clone"
            );
        }

        // Then the *earlier* snapshot, which the board has already been past —
        // a non-LIFO restore, where the counters must still be monotone.
        let next_id_before = live.store().next_id();
        storm(&mut live, &mut rng, 25);
        let high_water = live.store().next_id();
        buffer.copy_from(&early_store);
        live.swap_store(&mut buffer, &mut delta);
        live.assert_consistent();

        let mut reference = early_board.clone();
        assert_eq!(
            observable(&mut live),
            observable(&mut reference),
            "seed {seed}: the early snapshot diverged from the clone"
        );
        assert!(
            live.store().next_id() >= high_water && high_water >= next_id_before,
            "seed {seed}: ids rewound across a non-LIFO restore"
        );
    }
}

/// The delta is not merely *sufficient* to fix the trees — it is exactly the set
/// of ids whose presence or content differs, plus the ones that only relocated.
/// A spurious entry costs a needless tile-shape rebuild; a missing one is a
/// stale leaf.
#[test]
fn the_reported_delta_covers_every_difference_between_the_two_states() {
    for seed in 0..24u64 {
        let mut rng = Rng::new(0xDE17_A000 ^ seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut live = board();
        storm(&mut live, &mut rng, 120);

        let snapshot = live.store().clone();
        let before: Vec<(ItemId, String)> = snapshot
            .iter_ascending()
            .map(|(id, item)| (id, format!("{item:?}")))
            .collect();

        storm(&mut live, &mut rng, 40);
        let after: Vec<(ItemId, String)> = live
            .store()
            .iter_ascending()
            .map(|(id, item)| (id, format!("{item:?}")))
            .collect();

        let mut buffer = ItemStore::new();
        let mut delta = StoreDelta::new();
        buffer.copy_from(&snapshot);
        live.swap_store(&mut buffer, &mut delta);
        live.assert_consistent();

        // Direction: `swap_store` diffs the board it *had* against the store it
        // is adopting, so `removed` is "was live, is gone".
        for (id, item) in &after {
            match before.iter().find(|(other, _)| other == id) {
                None => assert!(
                    delta.removed.contains(id),
                    "seed {seed}: {id:?} was dropped and not reported: {delta:?}"
                ),
                Some((_, snapshotted)) if snapshotted != item => assert!(
                    delta.changed.contains(id),
                    "seed {seed}: {id:?} changed content and was not reported"
                ),
                Some(_) => {}
            }
        }
        for (id, _) in &before {
            if !after.iter().any(|(other, _)| other == id) {
                assert!(
                    delta.added.contains(id),
                    "seed {seed}: {id:?} came back and was not reported"
                );
            }
        }
        // Nothing spurious: every reported id really is on one side or the other.
        for id in delta.added.iter().chain(delta.removed.iter()) {
            assert!(
                before.iter().any(|(other, _)| other == id)
                    || after.iter().any(|(other, _)| other == id),
                "seed {seed}: {id:?} is reported but exists in neither state"
            );
        }
    }
}

/// `adopt_store` is the owning form of the same operation and must agree with
/// the pooled one item for item and leaf for leaf.
#[test]
fn adopt_store_and_swap_store_land_on_the_same_board() {
    for seed in 0..12u64 {
        let mut rng = Rng::new(0x5AFE_0001 ^ seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut a = board();
        storm(&mut a, &mut rng, 120);
        let snapshot = a.store().clone();
        let mut b = a.clone();

        let mut rng_a = Rng::new(0xF00D ^ seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut rng_b = Rng::new(0xF00D ^ seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        storm(&mut a, &mut rng_a, 40);
        storm(&mut b, &mut rng_b, 40);

        let mut buffer = ItemStore::new();
        let mut delta = StoreDelta::new();
        buffer.copy_from(&snapshot);
        a.swap_store(&mut buffer, &mut delta);
        let owned = b.adopt_store(snapshot);

        a.assert_consistent();
        b.assert_consistent();
        assert_eq!(delta.added, owned.added, "seed {seed}");
        assert_eq!(delta.removed, owned.removed, "seed {seed}");
        assert_eq!(delta.changed, owned.changed, "seed {seed}");
        assert_eq!(observable(&mut a), observable(&mut b), "seed {seed}");
    }
}

/// A property test that stopped exercising its property is worse than no test:
/// it reports green while proving nothing. This pins what the storm above
/// actually reaches, so a future change to the workload that quietly empties it
/// fails here instead of passing everywhere.
///
/// It also records the one branch the storm does **not** reach — see the module
/// docs — as a measurement rather than an assumption.
#[test]
fn delta_classes_are_actually_exercised() {
    let mut adds = 0usize;
    let mut removes = 0usize;
    let mut relocations = 0usize;
    let mut smallest_board = usize::MAX;

    for seed in 0..24u64 {
        let mut rng = Rng::new(0xC0FF_EE00 ^ seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut live = board();
        storm(&mut live, &mut rng, 120);
        let snapshot = live.store().clone();
        storm(&mut live, &mut rng, 40);
        smallest_board = smallest_board.min(live.store().len());

        let mut buffer = ItemStore::new();
        let mut delta = StoreDelta::new();
        buffer.copy_from(&snapshot);
        live.swap_store(&mut buffer, &mut delta);
        live.assert_consistent();

        adds += delta.added.len();
        removes += delta.removed.len();
        relocations += delta.changed.len();
    }

    assert!(adds > 100, "the storm stopped removing items: {adds} adds");
    assert!(
        removes > 100,
        "the storm stopped inserting items: {removes} removes"
    );
    assert!(
        smallest_board >= POPULATION / 2,
        "the storm collapsed to a {smallest_board}-item board and proves little"
    );
    assert_eq!(
        relocations, 0,
        "a slot relocation became reachable — the `diff_into_relocating` clause is \
         no longer dead, and this workload can now be used to test it"
    );
}
