//! The M1 board-history gate from `docs/ARCHITECTURE.md`:
//!
//! > M1 microbench gate: restore of a 30-pass-old board on the largest fixture
//! > < 50 ms
//!
//! Criterion is deliberately not a workspace dependency, so this is a plain
//! `#[ignore]`d test that prints a table, in the same shape as
//! `fr-datastructures/tests/arena_shape_tree_bench.rs`. Run it in release mode:
//!
//! ```text
//! cargo test -p fr-board --release --test board_history_bench \
//!     -- --ignored --nocapture
//! ```
//!
//! The workload is a synthetic board of 20 000 items (the suite's largest
//! fixtures are in that range) mutated for 30 passes of ~170 mutations each
//! — 5 000 mutations total, snapshotted at every pass boundary — and then
//! restored all 30 passes back. The assertion is on the **restore**, which is
//! what the gate names and what the batch autorouter pays when it discards a
//! pass; the snapshot and mutation costs are printed alongside because they are
//! the price of being able to restore at all.
//!
//! The test also checks that the restored board is item-for-item the board that
//! was snapshotted, so it doubles as a correctness check at a scale the unit
//! tests do not reach.

use std::hint::black_box;
use std::time::{Duration, Instant};

use fr_board::item::{FixedState, Item, ItemKind, LayerRange, PolylineTrace, Via};
use fr_board::{BoardHistory, ItemId, ItemStore, NetNos};
use fr_geometry::planar::{IntPoint, Polyline};

const ITEMS: usize = 20_000;
const PASSES: usize = 30;
const MUTATIONS: usize = 5_000;
const NETS: u32 = 400;
/// Half-extent of the coordinate window, in board units.
const WORLD: i32 = 400_000;

/// The gate from `ARCHITECTURE.md`.
const RESTORE_BUDGET: Duration = Duration::from_millis(50);

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

    fn coord(&mut self) -> i32 {
        self.below((2 * WORLD) as u32) as i32 - WORLD
    }
}

/// A trace of 3–6 corners, which is the shape distribution the fixture suite
/// shows after pull-tight; the polyline is the only heap payload an item has,
/// so it dominates snapshot cost.
fn random_trace(rng: &mut Rng) -> Item {
    let corners = 3 + rng.below(4) as usize;
    let mut points = Vec::with_capacity(corners);
    let mut x = rng.coord();
    let mut y = rng.coord();
    points.push(IntPoint::new(x, y));
    for i in 0..corners - 1 {
        if i % 2 == 0 {
            x += 200 + rng.below(4000) as i32;
        } else {
            y += 200 + rng.below(4000) as i32;
        }
        points.push(IntPoint::new(x, y));
    }
    Item::new(
        NetNos::of(1 + rng.below(NETS) as i32),
        1,
        FixedState::Unfixed,
        ItemKind::Trace(PolylineTrace {
            polyline: Polyline::from_int_points(&points),
            layer: rng.below(4) as i32,
            half_width: 50 + rng.below(100) as i32,
        }),
    )
}

fn random_via(rng: &mut Rng) -> Item {
    Item::new(
        NetNos::of(1 + rng.below(NETS) as i32),
        1,
        FixedState::Unfixed,
        ItemKind::Via(Via {
            center: IntPoint::new(rng.coord(), rng.coord()),
            padstack_no: rng.below(8),
            layers: LayerRange::new(0, 3),
            attach_allowed: true,
            is_escape_via: false,
            escape_via_smd_layer: -1,
        }),
    )
}

fn random_item(rng: &mut Rng) -> Item {
    if rng.below(6) == 0 {
        random_via(rng)
    } else {
        random_trace(rng)
    }
}

fn report(label: &str, elapsed: Duration, count: usize) {
    let per_op = elapsed.as_secs_f64() * 1e6 / count as f64;
    println!(
        "  {label:<38} {:>9.2} ms   {per_op:>8.1} us/op",
        elapsed.as_secs_f64() * 1e3
    );
}

#[test]
#[ignore = "timing benchmark; run with --release -- --ignored --nocapture"]
fn bench_board_history_restore_thirty_passes_back() {
    let mut rng = Rng::new(0x000B_0A4D_1234_5678);

    // ---- build the synthetic board --------------------------------------
    let start = Instant::now();
    let mut store = ItemStore::with_capacity(ITEMS);
    let mut live: Vec<ItemId> = Vec::with_capacity(ITEMS);
    for _ in 0..ITEMS {
        live.push(store.insert(random_item(&mut rng)));
    }
    let build = start.elapsed();

    println!("\n{ITEMS} items, {PASSES} passes, {MUTATIONS} mutations total\n");
    report("build board", build, ITEMS);

    let mut history = BoardHistory::new();
    let baseline: Vec<ItemId> = store.ids().collect();
    let baseline_len = store.len();

    // The snapshot the restore has to land on, taken before any pass runs.
    let start = Instant::now();
    history.snapshot(&store);
    let first_snapshot = start.elapsed();
    report("snapshot (cold, unpooled buffer)", first_snapshot, 1);

    // ---- 30 passes of mutation, snapshotting each ------------------------
    let per_pass = MUTATIONS / PASSES;
    let mut snapshot_total = Duration::ZERO;
    let mut mutate_total = Duration::ZERO;

    for _ in 0..PASSES {
        let start = Instant::now();
        for _ in 0..per_pass {
            match rng.below(4) {
                // rip up
                0 => {
                    if !live.is_empty() {
                        let idx = rng.below(live.len() as u32) as usize;
                        let id = live.swap_remove(idx);
                        store.remove(id);
                    }
                }
                // route something new
                1 => live.push(store.insert(random_item(&mut rng))),
                // pull-tight: rewrite a trace's geometry in place
                _ => {
                    if !live.is_empty() {
                        let idx = rng.below(live.len() as u32) as usize;
                        let id = live[idx];
                        let replacement = random_item(&mut rng);
                        store.replace(id, replacement);
                    }
                }
            }
        }
        mutate_total += start.elapsed();

        let start = Instant::now();
        history.snapshot(&store);
        snapshot_total += start.elapsed();
    }

    report("mutate", mutate_total, MUTATIONS);
    report("snapshot (pooled, per pass)", snapshot_total, PASSES);
    println!(
        "  {:<38} {:>9.2} ms",
        "snapshot, one pass (mean)",
        snapshot_total.as_secs_f64() * 1e3 / PASSES as f64
    );

    // ---- restore all the way back ---------------------------------------
    assert_eq!(history.depth(), PASSES + 1);
    let start = Instant::now();
    let delta_len = history
        .restore_back(&mut store, PASSES + 1)
        .expect("the stack is deeper than the requested level")
        .len();
    let restore = start.elapsed();
    report("restore 30 passes back", restore, 1);
    println!("  {:<38} {delta_len:>9} items\n", "delta reported");

    black_box(&store);

    // Correctness at scale: the restored board is the board that was snapshotted.
    assert_eq!(store.len(), baseline_len, "restored item count differs");
    assert_eq!(
        store.ids().collect::<Vec<_>>(),
        baseline,
        "restored id set differs"
    );
    store.assert_consistent();

    assert!(
        restore < RESTORE_BUDGET,
        "restore of a {PASSES}-pass-old {ITEMS}-item board took {:.2} ms, budget is {:.0} ms",
        restore.as_secs_f64() * 1e3,
        RESTORE_BUDGET.as_secs_f64() * 1e3
    );
}
