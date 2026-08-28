//! The optimizer's decision logic, driven by a **scripted rerouter**.
//!
//! `fr_router::optimizer::ItemRerouter` exists so that the keep/rollback rule,
//! the ripup schedule and the two exit ladders can be tested without also
//! testing the maze engine: every test here states what the reroute produced
//! and asserts what the optimizer did about it. The end-to-end property — that
//! the real engine, driven by the real optimizer, never makes a board worse —
//! is `optimizer_small_board.rs`.
//!
//! Spec: `batch-orchestration.md` §7.2/§7.3 and conformance rows #81-#96,
//! against release `autoroute/BatchOptimizer.java`.

use fr_board::board::{Board, Layer, LayerStructure};
use fr_board::item::{BoardOutline, FixedState, Item, ItemId, ItemKind, LayerRange, Pin, Via};
use fr_board::net_nos::NetNos;
use fr_board::shape::{Padstack as BoardPadstack, TileShape};
use fr_geometry::planar::{IntBox, IntPoint, Polyline};
use fr_router::optimizer::{
    BatchOptimizer, ItemRerouter, LengthMetric, LoopExit, PassExit, RerouteRequest,
};
use fr_router::test_support::{empty_board, OUTLINE_HALF_EXTENT, TEST_HALF_WIDTH};
use fr_rules::board_rules::BoardRules;
use fr_rules::router_settings::RouterSettings;
use std::cell::RefCell;
use std::rc::Rc;

// ---------------------------------------------------------------------------
// the bench
// ---------------------------------------------------------------------------

/// A two-layer board with `net_count` independent point-to-point nets, each
/// carrying one unfixed trace between its two pads.
///
/// Nets are stacked in `y` at the same `x`, so the sweep's `(x, y, layer)` key
/// orders them by net number and every test can name the visit order.
struct Bench {
    board: Board,
    rules: BoardRules,
    settings: RouterSettings,
    /// `pads[net - 1]` is that net's `(left, right)` pad centre.
    pads: Vec<(IntPoint, IntPoint)>,
    via_padstack_no: u32,
}

impl Bench {
    fn new(net_count: usize) -> Bench {
        let layers = LayerStructure::new(vec![Layer::signal("L0"), Layer::signal("L1")]);
        let (mut board, mut rules, settings) = empty_board(layers);

        let (ox, oy) = OUTLINE_HALF_EXTENT;
        board.insert_item(Item::new(
            NetNos::EMPTY,
            0,
            FixedState::SystemFixed,
            ItemKind::BoardOutline(BoardOutline {
                borders: vec![Polyline::from_int_points(&[
                    IntPoint::new(-ox, -oy),
                    IntPoint::new(ox, -oy),
                    IntPoint::new(ox, oy),
                    IntPoint::new(-ox, oy),
                    IntPoint::new(-ox, -oy),
                ])],
                keepout_outside_outline: false,
            }),
        ));

        let pad_padstack_no = board.padstacks_mut().push(BoardPadstack::uniform(
            0,
            0,
            TileShape::Box(IntBox::new(-200, -200, 200, 200)),
        ));
        let via_padstack_no = board.padstacks_mut().push(BoardPadstack::uniform(
            0,
            1,
            TileShape::Box(IntBox::new(-150, -150, 150, 150)),
        ));

        // `empty_board` already declares net 1.
        for net in 2..=net_count {
            rules.nets.add(format!("N{net}"), 1, false, 0);
        }

        let mut pads = Vec::new();
        for net in 1..=net_count {
            let y = -1_000 + 600 * (net as i32 - 1);
            let left = IntPoint::new(-2_000, y);
            let right = IntPoint::new(2_000, y);
            for (pin_no, center) in [left, right].into_iter().enumerate() {
                board.insert_pin(
                    Pin {
                        center,
                        padstack_no: pad_padstack_no,
                        pin_no: pin_no as u32,
                        layers: LayerRange::new(0, 0),
                    },
                    NetNos::of(net as i32),
                    1,
                );
            }
            pads.push((left, right));
        }

        board.ensure_compensated_tree(1);
        Bench {
            board,
            rules,
            settings,
            pads,
            via_padstack_no,
        }
    }

    /// Joins net `net`'s pads with a straight wire.
    fn join_straight(&mut self, net: i32) -> ItemId {
        let (left, right) = self.pads[net as usize - 1];
        self.wire(net, &[left, right], 0)
    }

    /// Joins net `net`'s pads with a detour through `(0, y ± bulge)`.
    fn join_detour(&mut self, net: i32, bulge: i32) -> ItemId {
        let (left, right) = self.pads[net as usize - 1];
        self.wire(net, &[left, IntPoint::new(0, left.y + bulge), right], 0)
    }

    fn wire(&mut self, net: i32, corners: &[IntPoint], layer: i32) -> ItemId {
        self.board
            .insert_trace_points(
                corners,
                layer,
                TEST_HALF_WIDTH,
                NetNos::of(net),
                1,
                FixedState::Unfixed,
                None,
            )
            .expect("the wire inserts")
    }

    /// Joins net `net`'s pads the expensive way: out on layer 0, across on
    /// layer 1, back on layer 0 — two vias the optimizer should want gone.
    fn join_through_two_vias(&mut self, net: i32) {
        let (left, right) = self.pads[net as usize - 1];
        let y = left.y;
        let down = IntPoint::new(-1_000, y);
        let up = IntPoint::new(1_000, y);
        self.wire(net, &[left, down], 0);
        self.wire(net, &[down, up], 1);
        self.wire(net, &[up, right], 0);
        for center in [down, up] {
            self.board.insert_via(
                Via {
                    center,
                    padstack_no: self.via_padstack_no,
                    layers: LayerRange::new(0, 1),
                    attach_allowed: false,
                    is_escape_via: false,
                    escape_via_smd_layer: -1,
                },
                NetNos::of(net),
                1,
                FixedState::Unfixed,
            );
        }
    }

    fn via_count(&self) -> usize {
        self.board
            .store()
            .iter()
            .filter(|(_, item)| matches!(item.kind, ItemKind::Via(_)))
            .count()
    }

    fn trace_ids(&self) -> Vec<ItemId> {
        let mut ids: Vec<ItemId> = self
            .board
            .store()
            .iter()
            .filter(|(_, item)| matches!(item.kind, ItemKind::Trace(_)))
            .map(|(id, _)| id)
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Every trace's `(id, layer, corners)`, for a bit-for-bit rollback check.
    fn trace_geometry(&self) -> Vec<(ItemId, i32, Vec<String>)> {
        let mut out: Vec<(ItemId, i32, Vec<String>)> = self
            .board
            .store()
            .iter()
            .filter_map(|(id, item)| match &item.kind {
                ItemKind::Trace(trace) => Some((
                    id,
                    trace.layer,
                    (0..trace.corner_count())
                        .map(|i| format!("{:?}", trace.polyline.corner(i)))
                        .collect(),
                )),
                _ => None,
            })
            .collect();
        out.sort_by_key(|entry| entry.0);
        out
    }
}

// ---------------------------------------------------------------------------
// scripted rerouters
// ---------------------------------------------------------------------------

/// Does nothing at all: the ripped connection stays ripped, so the board gains
/// an incomplete and the item can never be kept.
#[derive(Default)]
struct DeadRerouter {
    calls: std::cell::Cell<usize>,
}

impl ItemRerouter for DeadRerouter {
    fn reroute(&mut self, _: &mut Board, _: &BoardRules, _: &RouterSettings, _: &RerouteRequest) {
        self.calls.set(self.calls.get() + 1);
    }
}

/// Every request the optimizer made, in call order — the rerouter is owned by
/// the optimizer, so the log is shared rather than read back.
type RequestLog = Rc<RefCell<Vec<RerouteRequest>>>;

/// Re-connects each net it is told about with a straight layer-0 wire, which is
/// the cheapest connection the bench admits.
struct StraightRerouter {
    nets: Vec<(i32, IntPoint, IntPoint)>,
    log: RequestLog,
}

impl StraightRerouter {
    fn new(bench: &Bench, nets: &[i32]) -> StraightRerouter {
        StraightRerouter {
            nets: nets
                .iter()
                .map(|net| {
                    let (left, right) = bench.pads[*net as usize - 1];
                    (*net, left, right)
                })
                .collect(),
            log: RequestLog::default(),
        }
    }

    fn log(&self) -> RequestLog {
        Rc::clone(&self.log)
    }
}

impl ItemRerouter for StraightRerouter {
    fn reroute(
        &mut self,
        board: &mut Board,
        _: &BoardRules,
        _: &RouterSettings,
        request: &RerouteRequest,
    ) {
        self.log.borrow_mut().push(*request);
        for (net, left, right) in self.nets.clone() {
            // Only the net that was actually ripped is missing its wire; the
            // insert is a no-op for the others because the trace is still there
            // and `combine_traces` folds the duplicate away.
            let unconnected = board.net_component_count(net) > 1;
            if !unconnected {
                continue;
            }
            board
                .insert_trace_points(
                    &[left, right],
                    0,
                    TEST_HALF_WIDTH,
                    NetNos::of(net),
                    1,
                    FixedState::Unfixed,
                    None,
                )
                .expect("the straight wire inserts");
        }
    }
}

/// Re-connects one net through two vias every time, on a path that gets a
/// little longer with each call: the via count never moves, the copper only
/// grows, and the release's mixed length comparison keeps every one of them.
struct LengtheningRerouter {
    net: i32,
    left: IntPoint,
    right: IntPoint,
    via_padstack_no: u32,
    calls: i32,
}

impl LengtheningRerouter {
    fn new(bench: &Bench, net: i32) -> LengtheningRerouter {
        let (left, right) = bench.pads[net as usize - 1];
        LengtheningRerouter {
            net,
            left,
            right,
            via_padstack_no: bench.via_padstack_no,
            calls: 0,
        }
    }
}

impl ItemRerouter for LengtheningRerouter {
    fn reroute(
        &mut self,
        board: &mut Board,
        _: &BoardRules,
        _: &RouterSettings,
        _: &RerouteRequest,
    ) {
        self.calls += 1;
        if board.net_component_count(self.net) <= 1 {
            return;
        }
        let bulge = (100 * self.calls).min(400);
        let y = self.left.y;
        let down = IntPoint::new(-1_000, y);
        let up = IntPoint::new(1_000, y);
        let wire = |board: &mut Board, corners: &[IntPoint], layer: i32| {
            board
                .insert_trace_points(
                    corners,
                    layer,
                    TEST_HALF_WIDTH,
                    NetNos::of(self.net),
                    1,
                    FixedState::Unfixed,
                    None,
                )
                .expect("the wire inserts");
        };
        wire(board, &[self.left, down], 0);
        wire(
            board,
            &[
                down,
                IntPoint::new(down.x + bulge, y + bulge),
                IntPoint::new(up.x - bulge, y + bulge),
                up,
            ],
            1,
        );
        wire(board, &[up, self.right], 0);
        for center in [down, up] {
            board.insert_via(
                Via {
                    center,
                    padstack_no: self.via_padstack_no,
                    layers: LayerRange::new(0, 1),
                    attach_allowed: false,
                    is_escape_via: false,
                    escape_via_smd_layer: -1,
                },
                NetNos::of(self.net),
                1,
                FixedState::Unfixed,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// exit rules
// ---------------------------------------------------------------------------

#[test]
fn an_exhausted_sweep_ends_the_pass() {
    let mut bench = Bench::new(3);
    for net in 1..=3 {
        bench.join_straight(net);
    }
    let settings = bench.settings.clone();
    let mut optimizer =
        BatchOptimizer::with_rerouter(&bench.rules, &settings, Box::new(DeadRerouter::default()));
    optimizer.length_metric = LengthMetric::WeightedBoth;

    let pass = optimizer.opt_route_pass(&mut bench.board, 1, true);
    assert_eq!(pass.exit, PassExit::Exhausted);
    assert_eq!(pass.items_attempted, 3, "one attempt per trace");
    assert_eq!(pass.items_improved, 0);
    assert_eq!(pass.consecutive_failures, 3);
}

#[test]
fn a_pass_stops_after_the_consecutive_failure_cap() {
    // Upstream's cap is 50 (`BatchOptimizer.java:233-234`); the rule is what is
    // under test, not the constant, so the bench turns it down to 2 rather than
    // building a 51-net board.
    let mut bench = Bench::new(5);
    for net in 1..=5 {
        bench.join_straight(net);
    }
    let mut settings = bench.settings.clone();
    settings.optimizer.max_consecutive_failures = 2;
    let mut optimizer =
        BatchOptimizer::with_rerouter(&bench.rules, &settings, Box::new(DeadRerouter::default()));
    optimizer.length_metric = LengthMetric::WeightedBoth;

    let pass = optimizer.opt_route_pass(&mut bench.board, 1, true);
    assert_eq!(pass.exit, PassExit::ConsecutiveFailures);
    assert_eq!(pass.items_attempted, 2, "the cap cut the sweep short");
    assert_eq!(pass.consecutive_failures, 2);
}

#[test]
fn the_shipped_consecutive_failure_cap_is_fifty() {
    let settings = RouterSettings::default();
    assert_eq!(settings.optimizer.max_consecutive_failures, 50);
    assert_eq!(settings.optimizer.max_passes, 100);
    assert_eq!(settings.optimizer.max_autoroute_passes, 6);
    assert_eq!(settings.optimizer.additional_ripup_cost_factor_at_start, 10);
    assert_eq!(settings.optimizer.trace_ripup_cost_factor, 0.6);
    assert!((settings.optimizer.improvement_threshold - 0.01).abs() < 1e-9);
}

#[test]
fn the_item_cap_ends_the_pass() {
    let mut bench = Bench::new(4);
    for net in 1..=4 {
        bench.join_straight(net);
    }
    let mut settings = bench.settings.clone();
    settings.optimizer.max_items = 2;
    let mut optimizer =
        BatchOptimizer::with_rerouter(&bench.rules, &settings, Box::new(DeadRerouter::default()));
    optimizer.length_metric = LengthMetric::WeightedBoth;

    let pass = optimizer.opt_route_pass(&mut bench.board, 1, true);
    assert_eq!(pass.exit, PassExit::MaxItems);
    assert_eq!(pass.items_attempted, 2);
    assert_eq!(optimizer.total_items_optimized(), 2);
}

#[test]
fn the_batch_loop_stops_on_the_pass_cap_and_alternates_preferred_directions() {
    let mut bench = Bench::new(1);
    bench.join_straight(1);
    let mut settings = bench.settings.clone();
    settings.optimizer.max_passes = 4;
    // Keep the improvement-threshold exit out of the way: with a dead rerouter
    // no pass ever gains score, so only the surcharge-drop rule and the cap
    // decide. A negative threshold makes "improvement < threshold" false.
    settings.optimizer.improvement_threshold = -1.0;

    let mut optimizer =
        BatchOptimizer::with_rerouter(&bench.rules, &settings, Box::new(DeadRerouter::default()));
    optimizer.length_metric = LengthMetric::WeightedBoth;
    let outcome = optimizer.run_batch_loop(&mut bench.board);

    assert_eq!(outcome.exit, LoopExit::MaxPasses);
    assert_eq!(outcome.passes.len(), 4);
    let alternation: Vec<bool> = outcome
        .passes
        .iter()
        .map(|pass| pass.with_preferred_directions)
        .collect();
    // `pass % 2 != 0` (`BatchOptimizer.java:153`).
    assert_eq!(alternation, vec![true, false, true, false]);
    let numbers: Vec<i32> = outcome.passes.iter().map(|pass| pass.pass_no).collect();
    assert_eq!(numbers, vec![1, 2, 3, 4]);
}

#[test]
fn the_in_pass_surcharge_rule_pre_empts_the_loop_level_one() {
    // Conformance #95: the release has **two** independent rules that clear
    // `use_increased_ripup_costs`, and they coexist. The one at the end of
    // `opt_route_pass` (`:281-284`) fires on the via/length ratio and runs
    // *first*, so a pass in which nothing improved has already cleared the flag
    // by the time `runBatchLoop`'s score-based rule (`:164-170`) looks at it —
    // and the loop therefore stops on the 1 % threshold after a single pass
    // rather than granting itself a second one.
    //
    // Net 1 is wired, net 2 is not, so the board scores around 500 and the
    // near-perfect early exit stays out of the way.
    let mut bench = Bench::new(2);
    bench.join_straight(1);
    let mut settings = bench.settings.clone();
    settings.optimizer.max_passes = 3;

    let mut optimizer =
        BatchOptimizer::with_rerouter(&bench.rules, &settings, Box::new(DeadRerouter::default()));
    optimizer.length_metric = LengthMetric::WeightedBoth;
    assert!(optimizer.uses_increased_ripup_costs());
    let outcome = optimizer.run_batch_loop(&mut bench.board);

    assert_eq!(outcome.passes.len(), 1, "{:?}", outcome.exit);
    assert_eq!(outcome.passes[0].items_improved, 0);
    assert_eq!(outcome.passes[0].route_improved, 0.0);
    assert!(outcome.passes[0].used_increased_ripup_costs);
    assert!(!optimizer.uses_increased_ripup_costs(), "cleared in-pass");
    assert_eq!(outcome.exit, LoopExit::BelowImprovementThreshold);
}

#[test]
fn a_pass_that_improved_items_but_lost_score_drops_the_surcharge_and_runs_again() {
    // The other half of #95: when the pass *did* keep items its ratio is
    // non-zero, so the in-pass rule leaves the flag alone and the loop-level
    // rule at `:164-170` is the one that fires. It converts "the score went
    // down" from a stop into "drop the surcharge and try again".
    //
    // Getting there needs a pass that improves items and still loses score,
    // which is precisely what the release's mixed weighted/unweighted length
    // comparison produces (#87) — so this test runs the default metric.
    let mut bench = Bench::new(2);
    bench.join_through_two_vias(1);
    let mut settings = bench.settings.clone();
    settings.optimizer.max_passes = 3;

    let rerouter = LengtheningRerouter::new(&bench, 1);
    let mut optimizer = BatchOptimizer::with_rerouter(&bench.rules, &settings, Box::new(rerouter));
    assert_eq!(optimizer.length_metric, LengthMetric::UpstreamMixed);
    let outcome = optimizer.run_batch_loop(&mut bench.board);

    assert!(outcome.passes.len() >= 2, "{:?}", outcome.exit);
    let first = &outcome.passes[0];
    assert!(first.items_improved > 0, "{first:?}");
    assert_ne!(first.route_improved, 0.0, "the ratio kept the flag alive");
    assert!(first.score_after <= first.score_before, "{first:?}");
    assert!(first.used_increased_ripup_costs);
    assert!(!outcome.passes[1].used_increased_ripup_costs);
}

#[test]
fn a_board_already_at_the_maximum_score_is_left_alone() {
    // No traces at all and no pads to connect: `max_connections` is zero, so
    // `getNormalizedScore` is zero and the near-perfect exit cannot fire ...
    let mut bench = Bench::new(1);
    bench.join_straight(1);
    let mut settings = bench.settings.clone();
    // ... unless the threshold is wide enough to swallow whatever the routed
    // board is missing. This is the `scoreBefore * (1 + threshold) >= 1000`
    // branch (`:142-147`) and nothing else.
    settings.optimizer.improvement_threshold = 1_000.0;

    let mut optimizer =
        BatchOptimizer::with_rerouter(&bench.rules, &settings, Box::new(DeadRerouter::default()));
    let outcome = optimizer.run_batch_loop(&mut bench.board);
    assert_eq!(outcome.exit, LoopExit::NearMaximumScore);
    assert!(outcome.passes.is_empty(), "not a single item was touched");
    assert_eq!(optimizer.total_items_optimized(), 0);
}

// ---------------------------------------------------------------------------
// keep / rollback
// ---------------------------------------------------------------------------

#[test]
fn a_rejected_reroute_leaves_the_board_bit_for_bit() {
    let mut bench = Bench::new(3);
    for net in 1..=3 {
        bench.join_straight(net);
    }
    let before_geometry = bench.trace_geometry();
    let before_incompletes = fr_router::batch::incomplete_count(&mut bench.board);
    let before_vias = bench.via_count();

    let settings = bench.settings.clone();
    let mut optimizer =
        BatchOptimizer::with_rerouter(&bench.rules, &settings, Box::new(DeadRerouter::default()));
    optimizer.length_metric = LengthMetric::WeightedBoth;
    let pass = optimizer.opt_route_pass(&mut bench.board, 1, true);

    assert_eq!(pass.items_improved, 0);
    assert_eq!(
        bench.trace_geometry(),
        before_geometry,
        "every trace kept its id and its corners"
    );
    assert_eq!(bench.via_count(), before_vias);
    assert_eq!(
        fr_router::batch::incomplete_count(&mut bench.board),
        before_incompletes
    );
    assert_eq!(pass.key_before, pass.key_after);
}

#[test]
fn a_shorter_reroute_is_kept_and_lowers_the_length_floor() {
    let mut bench = Bench::new(1);
    bench.join_detour(1, 900);
    let before = bench.trace_ids();

    let settings = bench.settings.clone();
    let rerouter = StraightRerouter::new(&bench, &[1]);
    let mut optimizer = BatchOptimizer::with_rerouter(&bench.rules, &settings, Box::new(rerouter));
    optimizer.length_metric = LengthMetric::WeightedBoth;

    let pass = optimizer.opt_route_pass(&mut bench.board, 1, true);
    assert_eq!(
        pass.items_improved, 1,
        "the detour was swapped for the wire"
    );
    assert_eq!(pass.key_after.incomplete_count, 0);
    assert!(
        pass.key_after.weighted_trace_length < pass.key_before.weighted_trace_length,
        "{:?} -> {:?}",
        pass.key_before,
        pass.key_after
    );
    assert_ne!(bench.trace_ids(), before, "the detour is gone");
}

#[test]
fn a_reroute_that_deletes_vias_is_kept_even_when_it_is_no_shorter() {
    let mut bench = Bench::new(1);
    bench.join_through_two_vias(1);
    assert_eq!(bench.via_count(), 2);

    let settings = bench.settings.clone();
    let rerouter = StraightRerouter::new(&bench, &[1]);
    let mut optimizer = BatchOptimizer::with_rerouter(&bench.rules, &settings, Box::new(rerouter));
    optimizer.length_metric = LengthMetric::WeightedBoth;

    let pass = optimizer.opt_route_pass(&mut bench.board, 1, true);
    assert!(pass.items_improved >= 1);
    assert_eq!(bench.via_count(), 0, "both vias were traded away");
    assert_eq!(pass.key_after.incomplete_count, 0, "and nothing came loose");
    // The via branch of the lattice outranks length, so it must have been the
    // one that fired: vias fell, incompletes did not move.
    assert_eq!(
        pass.key_before.incomplete_count,
        pass.key_after.incomplete_count
    );
    assert!(pass.key_after.via_count < pass.key_before.via_count);
}

#[test]
fn the_sweep_hands_out_vias_before_the_traces_they_hold_up() {
    // `ReadSortedRouteItems` reads traces last (`:509`) and skips those
    // contacting a live via (`:542-547`), so a connection carrying vias is
    // entered through a via — which is why one item is enough to rip the whole
    // detour above.
    let mut bench = Bench::new(1);
    bench.join_through_two_vias(1);
    let settings = bench.settings.clone();
    let rerouter = StraightRerouter::new(&bench, &[1]);
    let mut optimizer = BatchOptimizer::with_rerouter(&bench.rules, &settings, Box::new(rerouter));
    optimizer.length_metric = LengthMetric::WeightedBoth;

    let pass = optimizer.opt_route_pass(&mut bench.board, 1, true);
    // Three traces + two vias on the board, but the traces all touch a via, so
    // only the two vias were ever candidates — and the first one's reroute
    // removed the second.
    assert!(
        pass.items_attempted <= 2,
        "attempted {}",
        pass.items_attempted
    );
}

// ---------------------------------------------------------------------------
// the ripup schedule reaches the rerouter
// ---------------------------------------------------------------------------

#[test]
fn a_trace_item_is_ripped_at_a_hundred_times_ten_times_zero_point_six() {
    let mut bench = Bench::new(1);
    bench.join_detour(1, 900);

    let settings = bench.settings.clone();
    let rerouter = StraightRerouter::new(&bench, &[1]);
    let log = rerouter.log();
    let mut optimizer = BatchOptimizer::with_rerouter(&bench.rules, &settings, Box::new(rerouter));
    optimizer.length_metric = LengthMetric::WeightedBoth;
    optimizer.opt_route_pass(&mut bench.board, 1, false);

    let requests = log.borrow();
    assert!(!requests.is_empty(), "the pass rerouted at least one item");
    // Every item on this bench is a trace, and pass 1 still carries the
    // surcharge when the *first* item is handed over: 100 × 10 × 0.6.
    assert_eq!(requests[0].ripup_costs, 600);
    assert_eq!(requests[0].max_passes, 6, "optimizer.maxAutoroutePasses");
}

#[test]
fn a_viasless_board_drops_the_surcharge_at_the_end_of_the_very_first_pass() {
    // `route_improved`'s ratio is `0` whenever the pass started with no vias or
    // no copper (`BatchOptimizer.java:265-269`, the ternary's else branch), and
    // the end-of-pass rule at `:281-284` then clears the flag. On a via-less
    // board that fires even when every item improved — upstream's behaviour,
    // reproduced.
    let mut bench = Bench::new(1);
    bench.join_detour(1, 900);
    let settings = bench.settings.clone();
    let rerouter = StraightRerouter::new(&bench, &[1]);
    let mut optimizer = BatchOptimizer::with_rerouter(&bench.rules, &settings, Box::new(rerouter));
    optimizer.length_metric = LengthMetric::WeightedBoth;

    let pass = optimizer.opt_route_pass(&mut bench.board, 1, true);
    assert_eq!(pass.key_before.via_count, 0);
    assert_eq!(pass.route_improved, 0.0);
    assert!(pass.items_improved > 0, "and yet an item was kept");
    assert!(!optimizer.uses_increased_ripup_costs());
    assert_eq!(optimizer.item_ripup_costs(true), 60);
}

#[test]
fn the_pass_parity_flag_reaches_the_rerouter() {
    for with_preferred_directions in [true, false] {
        let mut bench = Bench::new(2);
        for net in 1..=2 {
            bench.join_straight(net);
        }
        let settings = bench.settings.clone();
        let rerouter = StraightRerouter::new(&bench, &[]);
        let log = rerouter.log();
        let mut optimizer =
            BatchOptimizer::with_rerouter(&bench.rules, &settings, Box::new(rerouter));
        optimizer.length_metric = LengthMetric::WeightedBoth;
        optimizer.opt_route_pass(&mut bench.board, 1, with_preferred_directions);

        let requests = log.borrow();
        assert!(!requests.is_empty());
        assert!(requests
            .iter()
            .all(|request| request.with_preferred_directions == with_preferred_directions));
    }
}
