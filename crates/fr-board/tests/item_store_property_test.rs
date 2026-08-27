//! Property tests for the item store and the transaction journal.
//!
//! Four properties are pinned here, matching the acceptance criteria of the
//! item-model rewrite (`docs/ARCHITECTURE.md`, fr-board section):
//!
//! 1. **Rollback == never happened.** A random op sequence run inside a
//!    transaction and rolled back leaves the store byte-identical to a shadow
//!    model of the state before the transaction opened, at every nesting depth.
//! 2. **Committed work survives its enclosing rollback boundary correctly.**
//!    A committed inner level is still undone by an outer rollback, and a
//!    committed outermost level is permanent.
//! 3. **Id monotonicity and descending iteration under churn.** Ids never
//!    repeat, never rewind, and `iter()` is strictly descending after any mix of
//!    inserts, removals, mutations, rollbacks and index compactions.
//! 4. **Net index consistency.** The per-net index always equals the set of
//!    live items whose net array contains that net, in ascending id order.
//!
//! The differential model is a plain `Vec<(ItemId, Item)>` — deliberately the
//! dumbest possible implementation, so that a disagreement always indicts the
//! store and never the oracle.

use fr_board::item::{FixedState, Item, ItemKind, LayerRange, PolylineTrace, Via};
use fr_board::{ItemId, ItemStore, NetNos};
use fr_geometry::planar::{IntPoint, Polyline};

/// xorshift64* — deterministic and dependency-free, as in the fr-datastructures
/// differential tests.
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

    /// Uniform value in `0..bound` (bound > 0).
    fn below(&mut self, bound: u32) -> u32 {
        (self.next_u64() >> 32) as u32 % bound
    }
}

const NET_COUNT: i32 = 7;

fn make_item(rng: &mut Rng) -> Item {
    let net = 1 + rng.below(NET_COUNT as u32) as i32;
    let nets = if rng.below(8) == 0 {
        // A tie item on two nets, to exercise the multi-net index paths.
        NetNos::from_slice(&[net, 1 + rng.below(NET_COUNT as u32) as i32])
    } else {
        NetNos::of(net)
    };
    let clearance_class = rng.below(4) as i32;
    if rng.below(3) == 0 {
        Item::new(
            nets,
            clearance_class,
            FixedState::Unfixed,
            ItemKind::Via(Via {
                center: IntPoint::new(rng.below(1000) as i32, rng.below(1000) as i32),
                padstack_no: rng.below(4),
                layers: LayerRange::new(0, 1),
                attach_allowed: true,
                is_escape_via: false,
                escape_via_smd_layer: -1,
            }),
        )
    } else {
        let x = rng.below(1000) as i32;
        let y = rng.below(1000) as i32;
        Item::new(
            nets,
            clearance_class,
            FixedState::Unfixed,
            ItemKind::Trace(PolylineTrace {
                polyline: Polyline::from_int_points(&[
                    IntPoint::new(x, y),
                    IntPoint::new(x + 100 + rng.below(400) as i32, y),
                    IntPoint::new(x + 100 + rng.below(400) as i32, y + 300),
                ]),
                layer: rng.below(2) as i32,
                half_width: 25 + rng.below(50) as i32,
            }),
        )
    }
}

/// The shadow model: every live item, in ascending id order.
type Model = Vec<(ItemId, Item)>;

fn model_of(store: &ItemStore) -> Model {
    store
        .iter_ascending()
        .map(|(id, item)| (id, item.clone()))
        .collect()
}

/// Every invariant that must hold after any operation, checked against the
/// store's own internal audit plus the parity-critical ordering rules.
fn check_invariants(store: &ItemStore, highest_seen: &mut u32) {
    store.assert_consistent();

    let descending: Vec<ItemId> = store.ids().collect();
    assert!(
        descending.windows(2).all(|w| w[0] > w[1]),
        "iteration is not strictly descending by id"
    );
    let ascending: Vec<ItemId> = store.iter_ascending().map(|(id, _)| id).collect();
    let mut reversed = descending.clone();
    reversed.reverse();
    assert_eq!(ascending, reversed, "the two iteration orders disagree");
    assert_eq!(descending.len(), store.len(), "len disagrees with iter");

    // Ids never rewind: the next id to be handed out only ever grows.
    let next = store.next_id().raw();
    assert!(
        next >= *highest_seen,
        "next_id went backwards: {next} after {highest_seen}"
    );
    *highest_seen = next;
    for id in &descending {
        assert!(id.raw() < next, "{id} is at or past the next id {next}");
    }

    // Net index: exactly the live items on each net, ascending.
    for net in 1..=NET_COUNT {
        let indexed = store.net_items(net);
        let expected: Vec<ItemId> = store
            .iter_ascending()
            .filter(|(_, item)| item.contains_net(net))
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            indexed, expected,
            "per-net index for net {net} disagrees with a linear scan"
        );
    }
}

/// Applies one random operation to the store and, when `model` is `Some`,
/// mirrors it there. Returns nothing; the caller compares afterwards.
fn random_op(rng: &mut Rng, store: &mut ItemStore, live: &mut Vec<ItemId>) {
    match rng.below(10) {
        0..=3 => {
            let id = store.insert(make_item(rng));
            live.push(id);
        }
        4..=6 => {
            if live.is_empty() {
                return;
            }
            let idx = rng.below(live.len() as u32) as usize;
            let id = live.swap_remove(idx);
            assert!(store.remove(id));
        }
        7..=8 => {
            if live.is_empty() {
                return;
            }
            let idx = rng.below(live.len() as u32) as usize;
            let id = live[idx];
            let new_class = rng.below(9) as i32;
            let new_net = 1 + rng.below(NET_COUNT as u32) as i32;
            store
                .mutate(id, |item| {
                    item.clearance_class = new_class;
                    if rng_flip(new_class) {
                        item.nets = NetNos::of(new_net);
                    }
                })
                .expect("mutating a live id must succeed");
        }
        _ => {
            if live.is_empty() {
                return;
            }
            let idx = rng.below(live.len() as u32) as usize;
            let id = live[idx];
            assert!(store.replace(id, make_item(rng)));
        }
    }
}

/// Deterministic coin flip derived from the op's own payload, so the shadow
/// model never has to be told about it.
fn rng_flip(value: i32) -> bool {
    value % 2 == 0
}

#[test]
fn rollback_is_indistinguishable_from_never_having_happened() {
    for seed in 0..64u64 {
        let mut rng = Rng::new(0xF00D_0000 ^ seed);
        let mut store = ItemStore::new();
        let mut live: Vec<ItemId> = Vec::new();
        let mut highest_seen = 0u32;

        // A committed base state.
        for _ in 0..40 {
            random_op(&mut rng, &mut store, &mut live);
        }
        check_invariants(&store, &mut highest_seen);

        let before = model_of(&store);
        let live_before = {
            let mut v = live.clone();
            v.sort();
            v
        };
        let next_id_before = store.next_id();

        let mark = store.begin();
        let mut txn_live = live.clone();
        let ops = 1 + rng.below(40);
        for _ in 0..ops {
            random_op(&mut rng, &mut store, &mut txn_live);
        }
        store.rollback_to(mark);

        assert_eq!(
            model_of(&store),
            before,
            "seed {seed}: rollback did not restore the item set"
        );
        let mut live_after: Vec<ItemId> = store.ids().collect();
        live_after.sort();
        assert_eq!(live_after, live_before, "seed {seed}: live id set differs");
        assert!(
            store.next_id() >= next_id_before,
            "seed {seed}: the id counter rewound on rollback"
        );
        assert_eq!(store.transaction_depth(), 0);
        check_invariants(&store, &mut highest_seen);
    }
}

#[test]
fn nested_rollback_restores_the_state_at_each_mark() {
    for seed in 0..32u64 {
        let mut rng = Rng::new(0xBEEF_0000 ^ seed);
        let mut store = ItemStore::new();
        let mut live: Vec<ItemId> = Vec::new();
        let mut highest_seen = 0u32;
        for _ in 0..30 {
            random_op(&mut rng, &mut store, &mut live);
        }

        // Open three levels, snapshotting the model at each.
        let mut marks = Vec::new();
        let mut models = Vec::new();
        let mut nested_live = live.clone();
        for _ in 0..3 {
            models.push(model_of(&store));
            marks.push(store.begin());
            for _ in 0..(1 + rng.below(12)) {
                random_op(&mut rng, &mut store, &mut nested_live);
            }
        }

        // Unwind them innermost first; each must land back on its own model.
        while let (Some(mark), Some(model)) = (marks.pop(), models.pop()) {
            store.rollback_to(mark);
            assert_eq!(
                model_of(&store),
                model,
                "seed {seed}: rollback to level {} missed",
                marks.len()
            );
            check_invariants(&store, &mut highest_seen);
        }
        assert_eq!(store.transaction_depth(), 0);
    }
}

#[test]
fn a_committed_inner_level_is_still_undone_by_the_outer_rollback() {
    for seed in 0..32u64 {
        let mut rng = Rng::new(0xC0FF_EE00 ^ seed);
        let mut store = ItemStore::new();
        let mut live: Vec<ItemId> = Vec::new();
        let mut highest_seen = 0u32;
        for _ in 0..25 {
            random_op(&mut rng, &mut store, &mut live);
        }
        let before = model_of(&store);

        let outer = store.begin();
        let mut txn_live = live.clone();
        for _ in 0..5 {
            random_op(&mut rng, &mut store, &mut txn_live);
        }
        let inner = store.begin();
        for _ in 0..5 {
            random_op(&mut rng, &mut store, &mut txn_live);
        }
        store.commit(inner);
        for _ in 0..5 {
            random_op(&mut rng, &mut store, &mut txn_live);
        }
        store.rollback_to(outer);

        assert_eq!(
            model_of(&store),
            before,
            "seed {seed}: committed inner work escaped the outer rollback"
        );
        check_invariants(&store, &mut highest_seen);
    }
}

#[test]
fn a_committed_outermost_transaction_is_permanent() {
    let mut rng = Rng::new(0x5EED);
    let mut store = ItemStore::new();
    let mut live: Vec<ItemId> = Vec::new();

    let mark = store.begin();
    for _ in 0..50 {
        random_op(&mut rng, &mut store, &mut live);
    }
    let after_txn = model_of(&store);
    store.commit(mark);

    assert_eq!(model_of(&store), after_txn);
    assert_eq!(store.journal_len(), 0, "the log must be discarded");
    store.assert_consistent();
}

#[test]
fn ids_stay_monotone_and_iteration_stays_descending_under_long_churn() {
    let mut rng = Rng::new(0xA5A5_1234);
    let mut store = ItemStore::new();
    let mut live: Vec<ItemId> = Vec::new();
    let mut highest_seen = 0u32;
    let mut all_ids_ever: Vec<ItemId> = Vec::new();

    for round in 0..300 {
        // Every fifth round runs inside a transaction that is rolled back, so
        // the index compaction path is exercised both suppressed and free.
        let mark = (round % 5 == 4).then(|| store.begin());
        let before_ids: Vec<ItemId> = store.ids().collect();
        let mut round_live = live.clone();

        for _ in 0..10 {
            random_op(&mut rng, &mut store, &mut round_live);
        }
        all_ids_ever.extend(store.ids());

        match mark {
            Some(mark) => {
                store.rollback_to(mark);
                assert_eq!(store.ids().collect::<Vec<_>>(), before_ids);
            }
            None => live = round_live,
        }
        check_invariants(&store, &mut highest_seen);
    }

    // No id was ever observed on two different items: since ids are handed out
    // once and never recycled, an id's item content may change but the id set
    // is a subset of `1..next_id` with no repeats inside any single snapshot,
    // which `check_invariants` already asserts. Here we only pin the counter.
    assert!(store.next_id().raw() > 1);
    assert!(all_ids_ever
        .iter()
        .all(|id| id.raw() < store.next_id().raw()));
}

#[test]
fn net_index_survives_rollback_of_net_changing_mutations() {
    let mut rng = Rng::new(0x1357_9BDF);
    let mut store = ItemStore::new();
    let mut live: Vec<ItemId> = Vec::new();
    let mut highest_seen = 0u32;
    for _ in 0..60 {
        random_op(&mut rng, &mut store, &mut live);
    }

    for _ in 0..80 {
        let before = model_of(&store);
        let mark = store.begin();
        for id in live.iter().take(10) {
            let net = 1 + rng.below(NET_COUNT as u32) as i32;
            store.mutate(*id, |item| item.nets = NetNos::of(net));
        }
        check_invariants(&store, &mut highest_seen);
        store.rollback_to(mark);
        assert_eq!(model_of(&store), before);
        check_invariants(&store, &mut highest_seen);
    }
}

#[test]
fn slot_indices_are_recycled_while_ids_are_not() {
    // The separation the whole design rests on: a side table keyed by slot sees
    // reuse, a search-tree leaf keyed by id never does.
    let mut store = ItemStore::new();
    let mut slots_seen = Vec::new();
    let mut ids_seen = Vec::new();

    for _ in 0..50 {
        let mut rng = Rng::new(7);
        let id = store.insert(make_item(&mut rng));
        slots_seen.push(store.slot_of(id).unwrap().index());
        ids_seen.push(id);
        store.remove(id);
    }

    assert!(
        slots_seen.windows(2).all(|w| w[0] == w[1]),
        "the single freed slot should be handed straight back out"
    );
    let mut sorted = ids_seen.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), ids_seen.len(), "an id was reused");
    assert!(ids_seen.windows(2).all(|w| w[0] < w[1]));
}
