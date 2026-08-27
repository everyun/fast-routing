//! Property tests for the board history (state mechanism 2).
//!
//! Pins the three things a pass-level snapshot must guarantee:
//!
//! 1. **Restore roundtrip.** `snapshot(); mutate*; restore()` returns the store
//!    to exactly the item set it held at snapshot time, at any stack depth and
//!    for any mix of inserts, removals and replacements — compared against a
//!    shadow `Vec<(ItemId, Item)>` model.
//! 2. **The delta is exactly right.** The [`StoreDelta`] handed to the
//!    invalidation callback lists precisely the items whose presence or content
//!    differs between the pre-restore and post-restore boards. Anything missing
//!    from it would leave a stale search-tree leaf behind; anything spurious
//!    costs a needless tile-shape recomputation.
//! 3. **Counters never rewind.** Restoring an older board must not reissue an
//!    id or a content revision, or a search tree that outlived the restore would
//!    resolve two different items to the same identity.
//!
//! The workload alternates snapshot/restore with the transaction journal so the
//! two mechanisms are exercised against each other, which is how the batch
//! autorouter uses them (shove transactions inside a pass, history at pass
//! boundaries).

use fr_board::item::{FixedState, Item, ItemKind, LayerRange, PolylineTrace, Via};
use fr_board::{BoardHistory, ItemId, ItemStore, NetNos, StoreDelta};
use fr_geometry::planar::{IntPoint, Polyline};

/// xorshift64* — deterministic and dependency-free.
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
        (self.next_u64() >> 32) as u32 % bound
    }
}

const NETS: u32 = 6;

fn make_item(rng: &mut Rng) -> Item {
    let nets = NetNos::of(1 + rng.below(NETS) as i32);
    if rng.below(4) == 0 {
        Item::new(
            nets,
            rng.below(3) as i32,
            FixedState::Unfixed,
            ItemKind::Via(Via {
                center: IntPoint::new(rng.below(9000) as i32, rng.below(9000) as i32),
                padstack_no: rng.below(4),
                layers: LayerRange::new(0, 1),
                attach_allowed: true,
                is_escape_via: false,
                escape_via_smd_layer: -1,
            }),
        )
    } else {
        let x = rng.below(9000) as i32;
        let y = rng.below(9000) as i32;
        Item::new(
            nets,
            rng.below(3) as i32,
            FixedState::Unfixed,
            ItemKind::Trace(PolylineTrace {
                polyline: Polyline::from_int_points(&[
                    IntPoint::new(x, y),
                    IntPoint::new(x + 300 + rng.below(900) as i32, y),
                    IntPoint::new(x + 300 + rng.below(900) as i32, y + 700),
                ]),
                layer: rng.below(3) as i32,
                half_width: 25 + rng.below(40) as i32,
            }),
        )
    }
}

type Model = Vec<(ItemId, Item)>;

fn model_of(store: &ItemStore) -> Model {
    store
        .iter_ascending()
        .map(|(id, item)| (id, item.clone()))
        .collect()
}

/// The delta the store *should* report between two models, computed the slow
/// obvious way so it cannot share a bug with `ItemStore::diff_into`.
fn model_delta(before: &Model, after: &Model) -> StoreDelta {
    let mut delta = StoreDelta::new();
    for (id, item) in before {
        match after.iter().find(|(other, _)| other == id) {
            None => delta.removed.push(*id),
            Some((_, other)) if other != item => delta.changed.push(*id),
            Some(_) => {}
        }
    }
    for (id, _) in after {
        if !before.iter().any(|(other, _)| other == id) {
            delta.added.push(*id);
        }
    }
    delta
}

fn mutate_pass(rng: &mut Rng, store: &mut ItemStore, live: &mut Vec<ItemId>, ops: u32) {
    for _ in 0..ops {
        match rng.below(6) {
            0..=1 => live.push(store.insert(make_item(rng))),
            2..=3 => {
                if !live.is_empty() {
                    let idx = rng.below(live.len() as u32) as usize;
                    let id = live.swap_remove(idx);
                    store.remove(id);
                }
            }
            4 => {
                if !live.is_empty() {
                    let idx = rng.below(live.len() as u32) as usize;
                    let id = live[idx];
                    let replacement = make_item(rng);
                    store.replace(id, replacement);
                }
            }
            _ => {
                if !live.is_empty() {
                    let idx = rng.below(live.len() as u32) as usize;
                    let id = live[idx];
                    let class = rng.below(9) as i32;
                    store.mutate(id, |item| item.clearance_class = class);
                }
            }
        }
    }
}

#[test]
fn restore_returns_the_board_to_the_snapshotted_state() {
    for seed in 0..48u64 {
        let mut rng = Rng::new(0x5A5A_0000 ^ seed);
        let mut store = ItemStore::new();
        let mut live: Vec<ItemId> = Vec::new();
        mutate_pass(&mut rng, &mut store, &mut live, 40);

        let mut history = BoardHistory::new();
        let expected = model_of(&store);
        history.snapshot(&store);

        let ops = 1 + rng.below(60);
        mutate_pass(&mut rng, &mut store, &mut live, ops);
        let before_restore = model_of(&store);

        let delta = history.restore(&mut store).unwrap().clone();
        assert_eq!(
            model_of(&store),
            expected,
            "seed {seed}: restore did not reproduce the snapshotted board"
        );
        let expected_delta = model_delta(&before_restore, &expected);
        assert_eq!(
            delta, expected_delta,
            "seed {seed}: the reported delta is wrong"
        );
        store.assert_consistent();
    }
}

#[test]
fn restoring_n_passes_back_matches_the_snapshot_taken_n_passes_ago() {
    for seed in 0..24u64 {
        let mut rng = Rng::new(0x9E37_0000 ^ seed);
        let mut store = ItemStore::new();
        let mut live: Vec<ItemId> = Vec::new();
        mutate_pass(&mut rng, &mut store, &mut live, 60);

        let mut history = BoardHistory::new();
        let mut models = Vec::new();
        let passes = 3 + rng.below(8) as usize;
        for _ in 0..passes {
            models.push(model_of(&store));
            history.snapshot(&store);
            let ops = 1 + rng.below(20);
            mutate_pass(&mut rng, &mut store, &mut live, ops);
        }

        let back = 1 + rng.below(passes as u32) as usize;
        let expected = models[passes - back].clone();
        history.restore_back(&mut store, back).unwrap();

        assert_eq!(
            model_of(&store),
            expected,
            "seed {seed}: restoring {back} of {passes} passes landed on the wrong state"
        );
        assert_eq!(history.depth(), passes - back);
        store.assert_consistent();
    }
}

#[test]
fn counters_never_rewind_across_repeated_snapshot_restore_cycles() {
    let mut rng = Rng::new(0xDEAD_BEEF);
    let mut store = ItemStore::new();
    let mut live: Vec<ItemId> = Vec::new();
    let mut history = BoardHistory::new();
    let mut all_ids_ever: Vec<ItemId> = Vec::new();
    let mut highest_next_id = 0u32;
    let mut highest_revision = 0u64;

    for _ in 0..120 {
        history.snapshot(&store);
        let ops = 1 + rng.below(15);
        mutate_pass(&mut rng, &mut store, &mut live, ops);
        all_ids_ever.extend(store.ids());

        if rng.below(2) == 0 {
            history.restore(&mut store).unwrap();
            live = store.ids().collect();
        } else {
            history.accept();
        }

        assert!(
            store.next_id().raw() >= highest_next_id,
            "the id counter rewound"
        );
        highest_next_id = store.next_id().raw();
        assert!(store.revision() >= highest_revision, "the revision rewound");
        highest_revision = store.revision();
        store.assert_consistent();
    }

    // Every id ever observed is below the counter, and no id was ever handed
    // out twice — which is what a search tree keyed by id relies on.
    assert!(all_ids_ever
        .iter()
        .all(|id| id.raw() < store.next_id().raw()));
}

#[test]
fn history_and_the_journal_compose_at_pass_boundaries() {
    // How the batch autorouter uses the two mechanisms: history at pass
    // boundaries, shove transactions inside a pass.
    for seed in 0..24u64 {
        let mut rng = Rng::new(0x1BAD_0000 ^ seed);
        let mut store = ItemStore::new();
        let mut live: Vec<ItemId> = Vec::new();
        mutate_pass(&mut rng, &mut store, &mut live, 50);

        let mut history = BoardHistory::new();
        let pass_start = model_of(&store);
        history.snapshot(&store);

        // A pass made of several shove attempts, some abandoned.
        for _ in 0..8 {
            let mark = store.begin();
            let attempt_start = model_of(&store);
            let mut attempt_live = live.clone();
            let ops = 1 + rng.below(8);
            mutate_pass(&mut rng, &mut store, &mut attempt_live, ops);
            if rng.below(2) == 0 {
                store.rollback_to(mark);
                assert_eq!(
                    model_of(&store),
                    attempt_start,
                    "seed {seed}: shove rollback"
                );
            } else {
                store.commit(mark);
                live = attempt_live;
            }
        }
        assert_eq!(store.transaction_depth(), 0);

        // The pass as a whole is discarded.
        history.restore(&mut store).unwrap();
        assert_eq!(
            model_of(&store),
            pass_start,
            "seed {seed}: discarding the pass did not undo its committed shoves"
        );
        store.assert_consistent();
    }
}

#[test]
fn the_restore_callback_receives_the_same_delta_the_return_value_carries() {
    let mut rng = Rng::new(0x2468_ACE0);
    let mut store = ItemStore::new();
    let mut live: Vec<ItemId> = Vec::new();
    mutate_pass(&mut rng, &mut store, &mut live, 40);

    let mut history = BoardHistory::new();
    let expected = model_of(&store);
    history.snapshot(&store);
    mutate_pass(&mut rng, &mut store, &mut live, 30);
    let before_restore = model_of(&store);

    let mut observed = StoreDelta::new();
    let restored = history.restore_with(&mut store, |store, delta| {
        observed = delta.clone();
        // The callback must see the board *after* the swap, which is what the
        // search-tree manager needs to recompute tile shapes for `added`.
        for id in &delta.added {
            assert!(store.contains(*id));
        }
        for id in &delta.removed {
            assert!(!store.contains(*id));
        }
    });
    assert!(restored);
    assert_eq!(observed, model_delta(&before_restore, &expected));
    assert_eq!(&observed, history.last_delta());
}

#[test]
fn snapshots_of_an_empty_board_round_trip() {
    let mut store = ItemStore::new();
    let mut history = BoardHistory::new();
    history.snapshot(&store);
    let a = store.insert(make_item(&mut Rng::new(3)));
    let delta = history.restore(&mut store).unwrap();
    assert_eq!(delta.removed, vec![a]);
    assert!(store.is_empty());
    store.assert_consistent();
}
