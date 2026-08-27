//! Adversarial verification of the transaction journal (external review).
//!
//! Targets the seams the store's own property tests do not stress directly:
//! deep LIFO nesting with random commit/rollback decisions, the
//! tombstone-compaction path firing *during* a rollback, rollback across slot
//! recycling, and rollback of a removal of the newest item (which pops the
//! ordered-index entry outright rather than tombstoning it).

use fr_board::item::{FixedState, Item, ItemId, ItemKind, LayerRange, PolylineTrace, Via};
use fr_board::{ItemStore, NetNos};
use fr_geometry::planar::{IntPoint, Polyline};

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, bound: u32) -> u32 {
        (self.next_u64() % bound as u64) as u32
    }
}

fn trace(rng: &mut Rng) -> Item {
    let n = 2 + rng.below(4) as usize;
    let pts: Vec<IntPoint> = (0..n)
        .map(|i| IntPoint::new(i as i32 * 1000 + rng.below(7) as i32, rng.below(1000) as i32))
        .collect();
    Item::new(
        NetNos::from_slice(&[1 + rng.below(5) as i32]),
        rng.below(4) as i32,
        FixedState::Unfixed,
        ItemKind::Trace(PolylineTrace {
            polyline: Polyline::from_int_points(&pts),
            layer: rng.below(2) as i32,
            half_width: 50,
        }),
    )
}

fn a_via(net: i32) -> Item {
    Item::new(
        NetNos::of(net),
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

/// Ascending (id, item) pairs — the whole observable board state.
fn model(store: &ItemStore) -> Vec<(ItemId, Item)> {
    store
        .iter_ascending()
        .map(|(id, item)| (id, item.clone()))
        .collect()
}

fn full_audit(store: &ItemStore) {
    store.assert_consistent();
    let desc: Vec<ItemId> = store.ids().collect();
    assert!(desc.windows(2).all(|w| w[0] > w[1]), "not descending");
    let mut asc: Vec<ItemId> = store.iter_ascending().map(|(id, _)| id).collect();
    asc.reverse();
    assert_eq!(asc, desc);
    for net in 1..=6 {
        let expected: Vec<ItemId> = store
            .iter_ascending()
            .filter(|(_, i)| i.contains_net(net))
            .map(|(id, _)| id)
            .collect();
        assert_eq!(store.net_items(net), expected, "net {net} index");
    }
}

/// A removal of the *newest* item pops its ordered-index entry (and any
/// trailing tombstones) instead of leaving a tombstone. Rolling that back has
/// to splice the entries back in at the right positions.
#[test]
fn rollback_of_a_tail_removal_that_also_dropped_trailing_tombstones() {
    let mut rng = Rng::new(7);
    let mut store = ItemStore::new();
    let ids: Vec<ItemId> = (0..6).map(|_| store.insert(trace(&mut rng))).collect();
    let before = model(&store);

    let mark = store.begin();
    // Tombstone two interior entries, then remove the newest — which pops its
    // own entry and then unwinds the freshly made trailing tombstone.
    assert!(store.remove(ids[3]));
    assert!(store.remove(ids[4]));
    assert!(store.remove(ids[5]));
    assert_eq!(store.len(), 3);
    store.rollback_to(mark);

    full_audit(&store);
    assert_eq!(model(&store), before, "tail-removal rollback lost state");
}

/// Remove an item inside a transaction, let a fresh insert recycle its slot,
/// then roll back: the resurrected item must not alias the newer one's slot.
#[test]
fn rollback_across_slot_recycling_does_not_alias() {
    let mut rng = Rng::new(11);
    let mut store = ItemStore::new();
    let a = store.insert(trace(&mut rng));
    let b = store.insert(trace(&mut rng));
    let before = model(&store);
    let slot_a = store.slot_of(a).unwrap();

    let mark = store.begin();
    assert!(store.remove(a));
    let recycled = store.insert(a_via(4));
    assert_eq!(
        store.slot_of(recycled),
        Some(slot_a),
        "precondition: the slot was recycled"
    );
    store.rollback_to(mark);

    full_audit(&store);
    assert_eq!(model(&store), before);
    assert!(store.contains(a) && store.contains(b));
    assert!(!store.contains(recycled));
    assert_eq!(store.slot_of(a), Some(slot_a));
}

/// Removing more than half the board inside a transaction should not compact
/// the ordered index (compaction is suppressed while recording), and the
/// rollback that follows — during which recording has already stopped and
/// compaction *can* fire between undo steps — must still land exactly.
#[test]
fn rollback_of_a_mass_removal_survives_mid_rollback_compaction() {
    let mut rng = Rng::new(23);
    let mut store = ItemStore::new();
    let ids: Vec<ItemId> = (0..300).map(|_| store.insert(trace(&mut rng))).collect();
    let before = model(&store);

    let mark = store.begin();
    // Remove 250 of 300 in scattered order, keeping the newest alive so the
    // tail-pop path is not the one exercised.
    let mut order: Vec<ItemId> = ids[..299].to_vec();
    for i in (1..order.len()).rev() {
        let j = rng.below((i + 1) as u32) as usize;
        order.swap(i, j);
    }
    for id in order.iter().take(250) {
        assert!(store.remove(*id));
    }
    assert_eq!(store.len(), 50);
    // Interleave inserts and mutations so the log is not homogeneous.
    for _ in 0..40 {
        store.insert(trace(&mut rng));
    }
    for id in order.iter().skip(250).take(20) {
        store.mutate(*id, |it| it.clearance_class += 17).unwrap();
    }
    store.rollback_to(mark);

    full_audit(&store);
    assert_eq!(model(&store), before, "mass-removal rollback diverged");
}

/// One open transaction level: its mark plus the shadow state captured when
/// the mark was taken.
type OpenLevel = (fr_board::Mark, Vec<(ItemId, Item)>, Vec<ItemId>);

/// Deep LIFO nesting with random commit/rollback at each level, checked
/// against a stack of shadow models.
#[test]
fn deep_nesting_with_random_commit_or_rollback_lands_on_the_right_model() {
    for seed in 0..48u64 {
        let mut rng = Rng::new(0xADD_0000 ^ seed);
        let mut store = ItemStore::new();
        let mut live: Vec<ItemId> = Vec::new();
        for _ in 0..40 {
            live.push(store.insert(trace(&mut rng)));
        }

        let mut open: Vec<OpenLevel> = Vec::new();

        for _ in 0..220 {
            match rng.below(12) {
                0..=1 if open.len() < 5 => {
                    let m = store.begin();
                    open.push((m, model(&store), live.clone()));
                }
                2..=3 if !open.is_empty() => {
                    let (m, snapshot, snap_live) = open.pop().unwrap();
                    store.rollback_to(m);
                    assert_eq!(
                        model(&store),
                        snapshot,
                        "seed {seed}: rollback to depth {} diverged",
                        open.len()
                    );
                    live = snap_live;
                    full_audit(&store);
                }
                4 if !open.is_empty() => {
                    let (m, _, _) = open.pop().unwrap();
                    store.commit(m);
                    full_audit(&store);
                }
                5..=7 => {
                    live.push(store.insert(trace(&mut rng)));
                }
                8..=9 if !live.is_empty() => {
                    let i = rng.below(live.len() as u32) as usize;
                    let id = live.swap_remove(i);
                    assert!(store.remove(id));
                }
                _ if !live.is_empty() => {
                    let i = rng.below(live.len() as u32) as usize;
                    let id = live[i];
                    let c = rng.below(9) as i32;
                    let n = 1 + rng.below(5) as i32;
                    store
                        .mutate(id, |it| {
                            it.clearance_class = c;
                            if c % 2 == 0 {
                                it.nets = NetNos::of(n);
                            }
                        })
                        .unwrap();
                }
                _ => {}
            }
        }

        // Unwind everything still open by rolling back, innermost first.
        while let Some((m, snapshot, _)) = open.pop() {
            store.rollback_to(m);
            assert_eq!(model(&store), snapshot, "seed {seed}: unwind diverged");
        }
        full_audit(&store);
        assert_eq!(store.transaction_depth(), 0);
    }
}

/// Rolling back an *outer* mark while inner levels are still open must undo
/// the inner work too and close every level.
#[test]
fn rollback_of_an_outer_mark_closes_and_undoes_the_open_inner_levels() {
    let mut rng = Rng::new(31);
    let mut store = ItemStore::new();
    let base = store.insert(trace(&mut rng));
    let before = model(&store);

    let outer = store.begin();
    let a = store.insert(trace(&mut rng));
    let _inner = store.begin();
    let b = store.insert(trace(&mut rng));
    let _innermost = store.begin();
    store.remove(base);
    let c = store.insert(trace(&mut rng));

    store.rollback_to(outer);
    assert_eq!(store.transaction_depth(), 0, "inner levels stayed open");
    assert_eq!(model(&store), before);
    assert!(!store.contains(a) && !store.contains(b) && !store.contains(c));
    full_audit(&store);
}

/// Ids burned by rolled-back work stay burned even after a mass rollback, and
/// the revision counter never rewinds.
#[test]
fn counters_never_rewind_across_nested_rollback() {
    let mut rng = Rng::new(41);
    let mut store = ItemStore::new();
    store.insert(trace(&mut rng));
    let mut highest_id = store.next_id().raw();
    let mut highest_rev = store.revision();

    for round in 0..40 {
        let outer = store.begin();
        for _ in 0..5 {
            store.insert(trace(&mut rng));
        }
        let inner = store.begin();
        for _ in 0..5 {
            store.insert(trace(&mut rng));
        }
        if round % 2 == 0 {
            store.commit(inner);
        } else {
            store.rollback_to(inner);
        }
        store.rollback_to(outer);

        assert!(store.next_id().raw() > highest_id, "id counter rewound");
        assert!(store.revision() >= highest_rev, "revision counter rewound");
        highest_id = store.next_id().raw();
        highest_rev = store.revision();
        assert_eq!(store.len(), 1, "round {round}: rollback left residue");
    }
    full_audit(&store);
}
