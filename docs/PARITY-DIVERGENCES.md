# Parity divergences

Every place fast-routing deliberately behaves differently from the reference
(`freerouting-2.3.0.jar`, Build-Revision `2d4de019`) is recorded here **before** the divergent
code merges. Formats: what upstream does, what we do, why, and what it measurably changes.
An empty section means "no divergences yet", not "nobody checked".

## Import / board model

- **`Wiring.readViaScope` multi-subnet net array** (v2 regression: `currentIndex` never
  incremented → padding zeros make distinct vias share net 0, suppressing violations between
  them). We follow the v1.9 semantics (correct array). Effect: on fixtures with multi-subnet
  vias (Issue035: 124 protected vias, Issue102: 65, Issue190: 223) the imported-board violation
  baseline V_in can differ from Java's `-drc` count. The import gate (G1.2a) therefore records
  V_in under BOTH semantics for affected fixtures and the router gate (G2.1) uses deltas only.
  Status: declared ahead of implementation; measure the actual V_in difference when fr-check lands.

## Router

Budget policy is intentionally NOT divergent: upstream wall-clock constants are the default, see
ARCHITECTURE "Budgets".

- **45° door calculation on 45° boards by default**
  (`fr_board::search_tree::Tiling45Mode`, default `Doors`). *Upstream* runs
  the 45° chain on 45°-restricted boards (`SearchTreeManager.java:166`).
  *We* default to `Doors` (the 45° `Sorted45DegreeRoomNeighbours` door calculation)
  on 45°-restricted boards, aligning with upstream. Any-angle and 90° boards
  continue to run their respective native chains. `FR_ROUTER_TILING_45=doors|full|off`
  allows explicit selection.
  *Why*: under `Doors` easy-10 is u same-or-better on all 9 fixtures, 24-board
  regression achieves zero added violations across all 24 boards, and RelayModule
  fanout /CAN_RX escape avoids dead pockets and routes cleanly in pass 1.
  On easyeda-pcb1 fanout runs 127× faster (4.07 s vs 515.9 s, matching jar parity).

- **BoardOutline tree shapes stay Simplex in 45° family tree**
  (`fr_board::search_tree::calculate_tree_shapes_for_family`, skip added in `48ef09d`).
  - *Upstream behaviour*: Upstream builds outline tree shapes in base class `ShapeSearchTree.java:971-1024` as a 3-line polyline offset (concave Simplex strip). `ShapeSearchTree45Degree` does not re-flavor them into exact octagons (its only outline override is `:453-460`, which replaces each strip by `bounding_octagon()`).
  - *Our behaviour*: Outline strips stay Simplex rather than converting into tighter 45° octagons via `TileShape::to_flavor(FortyFive)` which would swallow boundary pads.
  - *Evidence*: `REPORT-054-pocket.md` (commit `48ef09d`); eliminates `Issue054-tairakb` Net 10 exhaustion loop, dropping runtime from 78s/1u to 4.7s/0u.
  - *Status*: 已对齐.

- **Trace tree shapes stay at their base Simplex in the 45° family tree**
  (`fr_board::search_tree::calculate_tree_shapes_for_family`).
  - *Upstream behaviour*: `ShapeSearchTree45Degree` has no `PolylineTrace` override — strips stay the exact concave Simplex the base class builds (`ShapeSearchTree.java:1039-1051`), and the 45° chain narrows each to `bounding_octagon()` only at its own read sites (`:147-149`).
  - *Our behaviour*: Trace strips stay at base Simplex in the 45° family tree instead of post-flavoring.
  - *Evidence*: `REPORT-tree-family-audit.md` (commit `765f161`); cla45-sensor-l6 119→110u, cla45-power-io-l6 76→69u, kicad-coldfire-unrouted 34→26u.
  - *Status*: 已对齐.

- **Fanout is on by default on the CLI** (`fr-cli`, `--no-fanout` to skip).
  - *Upstream behaviour*: `DefaultSettings.fanout.enabled = true` (`app/freerouting/settings/DefaultSettings.java:113`); headless entry gates on `isFanoutEnabled()` (`app/freerouting/autoroute/BatchAutorouter.java:798`).
  - *Our behaviour*: CLI `-de … -mp 20` enables fanout by default, exactly matching the release jar. Library default (`MazeOptions`) stays off for minimal in-process test harnesses.
  - *Evidence*: `REPORT-burners.md` (commit `3851aa3`), `bench/results/ledger2/20260906-4f537aa/gate.json`.
  - *Status*: 已对齐.

- **`fanout.fallbackToBoardVias` ships OFF; fallback vias inherit the routed net's via clearance class**
  (`fr_rules::router_settings::FanoutSettings::fallback_to_board_vias`, default `false`;
  `FR_FANOUT_FALLBACK_VIAS=1` restores the upstream behaviour for one run).
  - *Upstream behaviour*: the fanout borrows board-wide vias when the net class's via rule is empty
    (`RoutingBoard.java:1019-1035`, gate at `BatchFanout.java:196-199`, default on at
    `DefaultSettings.java:123`). The lent `ViaInfo` keeps its own clearance class, which is the
    **default net class's** (`Network.java:317-343`), and insertion uses exactly that class
    (`ForcedViaAlgo.java:230`). Upstream never notices the resulting under-clearance because its
    `SesReader` re-imports vias with the *default* net class's via clearance too
    (`SesReader.java:359-363`) — router and re-importer are consistently wrong together.
  - *Our behaviour*: (a) a fallback via is re-classed to the routed net's `ItemClass::Via`
    clearance (`fanout.rs::combined_via_rule`, `ctrl.rs::init_net`), and (b) the fallback itself
    ships off, because even re-classed, borrowing a smaller board via changes Power-class fanout
    geometry and costs unrouted (Issue039 Doors 28u → 44u with fallback on and correct classes).
    Our Lossless SES reader restores the item's own net class on re-import, so an under-clearanced
    fallback via *is* charged — the two behaviours upstream stacks cannot hide it.
  - *Evidence*: `REPORT-tree-desync.md` (commit `3c19050`): three-arm A/B/C on Issue039 —
    A (upstream-identical) Full-mode +1v at `via(1462338,-671317)` (2448 actual vs 2502 required);
    B (fallback on, correct classes) 0 added v both modes, Doors 44u;
    C (fallback off) 0 added v both modes, Doors 35u, Full 38u. Ship default = C; re-enable with
    `FR_FANOUT_FALLBACK_VIAS=1`.
  - *Status*: 有意分歧 (upstream default on → ours off; upstream via-class semantics broken →
    ours per-net). Revisit if the 45° Full mode becomes the shipping default: there B (fallback
    on, correct classes) beat both A and C (32u/25v vs A 40u/26v, C 38u/25v).

- **Pull-tight fork-splice recheck** (`fr_router::pull_tight`,
  `PullTightAlgo::check_fork_splice`, called from the acute branch of all
  four §6.4 end-corner rules).
  - *Upstream behaviour*: `app/freerouting/board/PullTightAlgo45.java:475-497`
  (and its three sister files `PullTightAlgo.java`, `PullTightAlgo90.java`, `PullTightAlgoAnyAngle.java`)
  splices the contact trace's line directly into the new trace polyline without a clearance recheck:
  ```java
  Line add_line = translate_line.translate(translate_dist);
  Line[] new_lines = new Line[trace_polyline.arr.length + 1];
  new_lines[0] = other_trace_line;
  new_lines[1] = add_line;
  System.arraycopy(trace_polyline.arr, 1, new_lines, 2, new_lines.length - 2);
  return new Polyline(new_lines);
  ```
  The adjusted trace can cover ground its predecessor never legally occupied.
  - *Our behaviour*: check the spliced three-line window at the compensated half width
  (same granularity as the bend branch's `reposition_line`) and refuse the move when clearance fails.
  - *Evidence*: `REPORT-pulltight-fork.md` (commit `6b8b49b`), easyeda-pcb1 fanout escape violation avoided
  (gap 295 vs 800 required); `Issue113-Protein.dsn` loop hang eliminated into 794s completion.
  - *Status*: 有意分歧 (constructive DRC guarantee).

- **Destination-pin neckdown stub** (`fr_router::maze::search`,
  `check_neck_down_at_dest_pin` returns 0).
  - *Upstream behaviour*: `app/freerouting/autoroute/MazeSearchAlgo.java:1542-1550` walks the room's target doors
  and returns the first Pin's `get_trace_neckdown_halfwidth(layer)`, and `:340-347` then caps `half_width` with it for `ExpansionDoor` entries.
  - *Our behaviour*: start-pin side (`:361-369`, `TargetItemExpansionDoor` to a Pin) is fully ported (`AutorouteEngine::pin_trace_neckdown_halfwidth`, tested on easyeda-pcb1 `$1N2152`); destination-pin neckdown returns 0 (stub).
  - *Evidence*: `REPORT-neckdown.md`; no 46-fixture board requires destination pin neckdown.
  - *Status*: 待核实 / 待定 (low priority capability stub; no corpus fixture exercises it).

- **Fanout no-progress stop** (`fr_router::fanout`) — **removed**, was an intentional divergence.
  - *Upstream behaviour*: `app/freerouting/autoroute/BatchFanout.java:82-144` stops a fanout run on three conditions only: `routed_count == 0` (`:111-113`), three consecutive bit-identical `(routed_count << 32) ^ via_count` signatures (`:114-130`, `STAGNATION_PASS_LIMIT = 3`), and unchanged board hash (`:134-138`).
  - *Our behaviour*: dropped the extra 2-pass flat via count stop (commit `wip/full-relay` `f753361`), restoring upstream's exact three stop rules.
  - *Evidence*: `REPORT-full-relay.md` (Issue110-RelayModule Full mode unrouted improved).
  - *Status*: 已对齐.

- **`remove_ripped`'s `FANOUT_VIA` stop uses a stricter fanout-via predicate** (`fr_router::insert::connection_items`, intentional divergence).
  - *Upstream behaviour*: `Item.get_connection_items(StopConnectionOption.FANOUT_VIA)` stops at a via where
  `Item.is_fanout_via` (`Item.java:1251-1282`) answers true — a via that touches a lone SMD pad directly
  or through a trace shorter than `PROTECT_FANOUT_LENGTH × half_width`, **even if the via feeds several
  escapes**. Main-pass ripups run with this stop whenever fanout is enabled
  (`BatchAutorouter(job)` → `remove_unconnected_vias = !isFanoutEnabled()`).
  - *Our behaviour*: kept the stricter pre-`a85e43c` predicate — a via counts as a fanout via only when all
  of its non-collected contacts are pins — so a main-pass ripup walks **through** multi-escape fanout vias
  and deletes the neighbouring escapes with it. Verified upstream-faithful port exists and was measured
  (see below); it is deliberately **not** shipped.
  - *Why*: with the upstream predicate every main-pass ripup that touches a multi-escape fanout via strands
  the via's far-side escape as an island needing re-routing; convergence needs more passes. On
  Issue102-Mars-64 (Doors default, `--deterministic --no-optimizer`) Java's own trajectory passes through
  **7 unrouted at pass #6** and converges 0u at pass #14 in 43.6 s
  (`java-stability/Issue102-*.r1.log`), while we need >20 passes — under the ledger's 285 s / 20-pass
  budget the upstream predicate lands at a stable **7u** (2u at 1200 s), vs **1u** with the stricter
  predicate. RelayModule Full's 0u (REPORT-full-relay) was achieved with the stricter predicate kept.
  - *Evidence*: `REPORT-fanout-102.md` §1–§3 (first-divergence ripup walk dump, per-contact evaluation dump
  proving the upstream-faithful port answers identically to the Java formula, Java trajectory data),
  dispatcher ruling 2026-09-07.
  - *Status*: 有意分歧（发货谓词比上游严；上游忠实版保留在 `batch::is_fanout_via`，`remove_tails` 的
  `FANOUT_VIA` 停止仍用它）。收敛速度差另立任务。

- **Ripped connections are never requeued inside a pass across all pass counts by default**
  (`fr_router::batch::schedule::RequeuePolicy`, default `Never`).
  - *Upstream behaviour*: `app/freerouting/autoroute/BatchAutorouter.java:431-694` derives the work list once per pass
  (`getAutorouteItems`, `:250-306`) and never adds to it; a net whose wiring a later connection ripped is picked up by the next pass
  at `start_ripup_costs * pass_no` (§4.1: "there is no retry inside a pass").
  - *Our behaviour*: `RequeuePolicy` defaults to `Never` (asserted by `schedule.rs:1771-1773` for all `max_passes` including 1 and 2),
  strictly matching upstream behaviour everywhere. `SinglePassOnly` and `Always` exist as opt-in modes for divergence research.
  - *Evidence*: `crates/fr-router/src/batch/schedule.rs:1771-1773`, `REPORT-pass-semantics.md` (commit `5f2ad0a`),
  Orca 24-board sweep zero regressions.
  - *Status*: 已对齐.

- **A negative `-mp` is rejected instead of running uncapped** (`fr-cli`, `parse_max_passes`).
  *Upstream* accepts it, and it is a latent bug the conformance appendix flags as "the
  cautionary example" (§1.2): the entry-point guard
  `getRunRouter() && (maxPasses == null || maxPasses >= 0)` sends a negative value down the
  *fanout-only* branch, which sets `maxPasses = 0`; `BatchAutorouter.runBatchLoop:894` then
  recomputes the same guard and re-enables the router, while the pass cap at `:931` requires
  `maxPasses > 0` — so `-mp -1` routes with **no pass limit at all**, ended only by a stagnation
  rule.
  *We* fail the argument scan with an error and exit 1. A non-numeric `-mp` is rejected the same
  way; the pre-existing `.parse().ok()` silently turned `-mp tow` into a one-pass run.
  *Why not replicate*: `-mp -1` has no meaning a user could be relying on, and the failure mode
  is a run that does not end. Silently clamping to 1 was rejected as the other option, because
  it hides a typo in a long batch.
  *Effect*: none on any fixture or harness — `bench/run_bench.py` passes positive counts only.
  The uncapped loop stays reachable deliberately through
  `fr_router::LoopConfig { max_passes: 0, .. }`, which is how the loop's own tests exercise it.
  Fanout-only is an explicit mode (`--fanout` with the router stage off), never a magic pass
  count, per `batch-orchestration.md` §11.1.

- **~~Fork/end smoothing checks its candidate shapes with the compensated half width~~ —
  RETIRED (fanout034).** The whole compensated-oracle divergence is gone, not just the §6.4
  half of it: `PullTightAlgo`'s clearance oracle now runs upstream's headless branch —
  `overlapping_tree_entries_with_clearance` on the default, uncompensated tree, both shapes
  enlarged by half the pairwise clearance *plus* `clearance_safety_margin`
  (`ShapeSearchTree.java:430-480`, dispatched at `BasicBoard.java:1174-1178`;
  `SearchTreeManager.clearance_compensation_used` is `false` outside the GUI,
  `SearchTreeManager.java:34`). `curr_half_width` is the raw half width everywhere, exactly
  what `:141` computes when the default tree's compensation is 0.
  *Why the compensated oracle was wrong*: its pre-inflated diagonal borders are one rounding
  step inside the DRC's two-step enlargement (`round(2251·√2) = 3183` vs
  `round(1250·√2) + round(1001·√2) = 3184` at half width 1250 / clearance 2002), so a
  pull-tight bisection could commit a trace at compensated tangency that the referee then
  flagged — Issue034's +1 violation with fanout on (trace /G2 vs /F, 2001.8 < 2002, and the
  jar's own `-drc` on our session flags the same pair). The old entry's Issue230 "+3 with the
  raw width" measurement was taken against the compensated tree — raw width *and* a tree that
  expected compensated queries — and does not apply to the symmetric oracle, which is strictly
  stricter than the compensated check (margin 16 plus touch-counts-as-overlap).
  *Effect*: pull-tight can no longer commit a position the DRC flags; the maze's compensated
  trees are untouched (search guidance only, as upstream).

- **A reported route must have copper to show for it** (`fr_router::batch`,
  `BatchRouter::discount_routes_that_left_no_copper` and the `board_changed` arm of
  `route_with_control`). **Report only — the board is bit-for-bit upstream's.**
  *Upstream* labels a connection `ROUTED` the moment `InsertFoundConnectionAlgo.get_instance`
  returns non-null, and never revisits the label. Three of its paths return non-null having left
  no copper: a located connection of a single corner is "already at the target" and inserts
  nothing (`InsertFoundConnectionAlgo:95-103`); `connect_to_trace` returns `true` for a corner
  already lying on the target trace's polyline (`RoutingBoard:1100-1105`), delegating the actual
  contact to the `normalize_traces` that follows; and a stub that *is* inserted may be deleted by
  the end-of-pass `remove_tails` (`:657-661`) or ripped out by a later connection in the same
  pass, both after the count was taken.
  *We* report `NOT_ROUTED` when the insertion left no live item behind, and move a connection
  out of `routed` when its copper is gone by the end of the pass (`PassOutcome::routes_undone`
  records how many). Nothing else changes: the pull-tight over the changed area still runs
  unconditionally — it also covers the region a ripup freed — and the ripup/requeue bookkeeping
  is untouched.
  *Why*: on `Issue229-display-8-digit-hc595` the whole `(wiring)` scope is `(type protect)`, i.e.
  `USER_FIXED`, and `PolylineTrace.split` refuses to split such a trace (`:707-709`). Contacts
  are exact endpoint equality (`Trace:172-176`, `DrillItem:266-283`), so no route the maze finds
  can ever attach to that copper. The pre-fix binary reported **702 routed connections at
  `-mp 20`** for a session whose copper is byte-identical to the input's — a number no harness
  could act on. It now reports 0 routed / 39 failed, with the same board. Java, for its part,
  declines the board outright (manifest class `java_degenerate`, "nothing to route", 0.03 s).
  *Effect*: `routed` falls on every board that ripped its own work — `Issue159` reports 23
  instead of 32 with an identical, complete board, the 9 being exactly its `ripped` count.
  Quality metrics (incompletes, violations, score) are unaffected everywhere, by construction.

- **A job wall budget, and a stop on it** (`fr_router::batch::schedule::JobDeadline`,
  `fr-cli --job-timeout`). *Upstream* has a per-job timeout in the scheduler; the headless entry
  point does not expose one, and `-mp` is the only bound a caller has. *We* add an optional wall
  budget, polled between connections, between passes, inside
  `PullTightAlgo::is_stop_requested`, and — since the G4-pipeline task — between the fanout
  stage's passes and pins (`FanoutConfig::deadline`, `FanoutStop::JobDeadline`; upstream arms
  no `TimeLimit` anywhere in `BatchFanout`, so a slow fanout stage used to run the budget to
  zero and beyond — 455.7 s wall on a 240 s budget measured on the easyeda-pcb1 neckdown
  repro). It ends the run through the same tail as every stop rule —
  best retained board restored, referee run, session written — and clamps each connection's
  `TimeLimit` to what the job has left so one connection cannot overrun it by its own 100 s. Off
  by default: a run without `--job-timeout` reads no clock and is unchanged (an unarmed
  `JobDeadline::ambient()` is one relaxed atomic load and no `Instant::now`). *Why*: a harness
  cap that `SIGKILL`s the process produces no session at all, which is strictly less information
  than the board we were holding. 26 of the 46 boards in the first full-suite G2.1 read failed
  exactly that way — and `Issue209-split10` failed it again once §6.4 landed, from *inside* a
  pull-tight that polled nothing.

- **The keep-point pull-tight inside forced insertion has a stall guard upstream leaves off**
  (`fr_router::insert_forced`, step 8 of `insert_forced_trace_polyline`).
  *Upstream* passes `p_time_limit = -1` to the `PullTightAlgo` it builds there
  (`RoutingBoard.java:833-834`), i.e. no `TimeLimit` at all (`PullTightAlgo.java:56-60`); that
  loop is bounded only by its own convergence.
  *We* arm `PULL_TIGHT_TIME_LIMIT` — 2000 ms, upstream's constant for its *other* non-batch
  pull-tight (`RoutingBoard.java:49`, used at `:144`) — spent in **optimization rounds** at
  `pull_tight::ROUNDS_PER_VIRTUAL_MS`, in both budget modes.
  *Why the round form and not the wall form*: this pull-tight runs once per *segment* of every
  forced insertion and moves copper the rest of the insertion then builds on, so a wall cutoff
  here would make what gets **inserted** depend on machine load — the failure `--deterministic`
  exists to prevent, one level below where the batch driver's regional pull-tight sits.
  *Effect*: none measurable. 200 000 rounds against a measured worst case of 17 229 for a whole
  *connection's* regional pull-tight, so it is a safety valve and no fixture reaches it. Without
  it the only bound was `DEFAULT_ROUND_BUDGET`'s two million.

- **The room-neighbour list is a sorted list, not a set**
  (`fr_router::rooms::neighbours::sort_neighbours`, `…::AutorouteEngine::obstacle_id`).
  *Upstream* keeps `SortedRoomNeighbours.sorted_neighbours` in a `TreeSet` ordered by
  `SortedRoomNeighbour.compareTo` (`autoroute/SortedRoomNeighbours.java:35, 307, 612-669`), whose
  final tie-break is the bare `search_tree_object.get_id_no()` — the board id for an item, with
  the shape index *not* in it. Two tile shapes of one item that touch the room with identical
  geometry therefore compare equal and the second is silently dropped, which `:643-646` argues is
  correct ("the 2 intersection pieces … are identical, so that only 1 obstacle is needed").
  *We* sort and keep every neighbour, and make the tie-break key `(item << 10) | shape_index`
  for items and `(1 << 48) + room_id` for rooms so that the collapse cannot happen by accident.
  *Why*: measured. Porting the exact key **and** the collapse costs 37,065 → 118,384 expansions
  on Issue026 (3.2×) and 163,986 → 1,865,184 on Issue034 (11.4×) at `-mp 3`, with the answer
  unchanged (0 violations, 0 incomplete on both). The exact key *without* the collapse is
  bit-for-bit inert on both boards — every counter identical — so it is the dropped neighbours
  that cost, not the ordering. Same treatment in the 45° arm (`neighbours45::compare_neighbours`).

- **`DrillPage.get_drills` splits the page with simplex arithmetic only**
  (`fr_router::maze::drills::AutorouteEngine::page_drills`). *Upstream* builds
  `PolylineArea(page_box, holes)` and calls `split_to_convex`
  (`geometry/planar/PolylineArea.java:165-196`), which is the same repeated-cutout loop we run —
  but its `piece.cutout(hole)` is a **double dispatch on the two `TileShape` subclasses**, so an
  `IntBox` page with an `IntBox` hole goes through `IntBox.cutout_from(IntBox)` (four rectangles
  chosen to minimise cumulative circumference, then `simplify()`), an `IntOctagon` hole through
  `IntOctagon.cutout_from(…)` (up to eight octagons), and only the general case through
  `Simplex.cutout_from`. *We* narrow every shape to a `Simplex` first and always take the last
  path. *Effect*: the same negative space, different pieces — and a drill sits at a piece's
  centre of gravity, so the candidate via positions differ. Measured hole-flavour mix on
  Issue110-RelayModule: 2,808 `Box` / 22,351 `Oct` / 24,841 `Simplex`, so the box-only case is
  6% and closing this needs `IntOctagon::cutout_from` and `Simplex::simplify` in `fr-geometry`
  (`geometry/planar/IntOctagon.java:1075-1400`, `Simplex.java:690-880`) before the dispatch can
  be written. Suspected cause of the Issue110 pair's one-connection G2.1 gaps.

## Optimizer

- **Entry is `getRunOptimizer()` and nothing else**
  (`fr_core::pipeline::run_maze_stage`, `fr_router::optimizer`).
  *Upstream* (`gui/RoutingJobSchedulerActionThread.java:179`) runs the stage with an unguarded
  `if (job.routerSettings.getRunOptimizer())` after the routing branch closes. It does not ask
  whether the board came out complete, what it scores, or how long the router took. The only
  routing outcome that reaches the decision is `maxItems`, which stops the *whole* job via
  `requestStop()` (`BatchAutorouter.autoroute_pass:496-505`, conformance #42).
  *We* match that: the stage runs when `MazeOptions::optimize` is on (the shipped default),
  unless the run stopped on `StopReason::ItemLimit` or the job budget is already spent. The
  board's own gates (near-maximum-score, improvement threshold, `maxPasses`, `maxItems`) live
  *inside* `runBatchLoop` and are reached only by entering.
  *Why this is recorded here*: skipping the stage on an incomplete board, or on a "good enough"
  score, is a tempting wall-clock saving — the stage is 80–84 % of the wall on the corpus's
  budget-burners — and it is not upstream. The test
  `the_optimizer_stage_entry_does_not_read_the_routing_result` has to fail before either skip
  can be introduced. See **Rejected divergences** below.

- **`$TMPDIR/freerouting/freerouting.json` is a measurement contaminant, not an upstream
  default.** The jar's `GlobalSettings.load()` reads `freerouting.json` from the user-data
  directory (`settings/sources/JsonFileSettings`). On this machine that file lived at
  `$TMPDIR/freerouting/freerouting.json` and had `router.fanout.enabled` and
  `router.optimizer.enabled` set to `false`. `-de` restores `router.enabled` only, so every
  Java invocation that did not pass a fresh `--user_data_path=` ran **without fanout and
  without the optimizer**, whatever the CLI flags said. That is why earlier campaign notes
  read "upstream does not run the optimizer": they were measuring a polluted settings file,
  not `getRunOptimizer()`. The entry condition above is what the release actually does.
  Bench Java calls must pass `--user_data_path=<empty dir>` (see `bench/README.md`); numbers
  taken without it are void. The G2.1 progress log's Java-side "default vs no-optimizer"
  ratios were taken under this contamination and must not be quoted as optimizer-on Java.

- **The improvement lattice compares weighted length against weighted length**
  (`fr_router::optimizer::LengthMetric`, default `WeightedBoth`).
  *Upstream* (`autoroute/BatchOptimizer.java:225`, `:381-393`; conformance #87) seeds
  `min_cumulative_trace_length` from `BoardStatistics.traces.totalWeightedLength` and then
  compares it against `boardStatisticsAfter.traces.**totalLength**` — the raw, unweighted sum.
  The weight is `half_width + clearance` per unit length, so on a real board the two differ by
  three or four orders of magnitude (Issue102 routed: raw 19 331 380 against weighted
  62 865 647 951, ratio 3 252). The length branch is therefore not a comparison at all, it is a
  constant `true`, and the lexicographic rule degenerates to "keep unless incompletes or vias
  got worse" — including reroutes that made the board longer and reroutes that changed nothing.
  *We* compare the weighted floor against the weighted result. `LengthMetric::UpstreamMixed`
  remains and remains tested (`optimizer_small_board.rs`,
  `optimizer_exit_rules.rs::the_release_metric_counts_a_reroute_that_changed_nothing_as_an_improvement`).
  *Why*: the release's number is an accounting artefact, and two of the optimizer's three brakes
  read it. On `Issue026-J2_reference -mp 3` the release metric reports **53 of 57 items
  improved** while leaving the score at 909.08 with 3 connections open — every phantom
  improvement resets `maxConsecutiveFailures` and holds the ×10 ripup surcharge on, so the pass
  can neither stop early nor get cheaper. Under the weighted metric the same board reports
  **9 of 41 improved**, finishes at **999.99 with 0 open**, and takes 64 s instead of 129 s.
  *Effect*: strictly fewer items are kept (`items_improved` falls), and the ones kept are the
  ones for which `(incompletes, vias, weighted length)` provably fell — which is what promise 2
  of `optimizer_small_board.rs` asserts. It is not uniformly better per board: on the same
  fixture at `-mp 1` the release metric's extra churn happens to land on 999.99 where the
  weighted metric stops at 969.69. What it is, is *accountable*.

- **The per-item reroute stops escalating at the first pass that routes nothing**
  (`fr_router::optimizer::escalation_continues`).
  *Upstream* (`BatchAutorouter.autoroute_passes_for_optimizing_item:146-167`) loops up to
  `optimizer.maxAutoroutePasses` (6) passes per optimized item while `still_unrouted_items`,
  which `autoroute_pass` returns as `routed > 0 || not_routed > 0` (`:688`). That is "did the
  pass attempt anything", not "did it achieve anything", so on any board carrying a connection
  the router could not close the guard is permanently true and every item pays all six passes —
  each of them re-deriving the whole board's work list and re-attempting the entire unroutable
  backlog at a ripup cost that grows with the pass number.
  *We* keep both of upstream's terms and add `routed > 0`.
  *Why*: measured on `Issue026-J2_reference -mp 3`, one optimizer pass over 57 items — inner
  pass 1 22.8 s / 449 connections queued / 78 routed; pass 2 21.2 s / 346 / 5; pass 3 22.2 s /
  343 / 3; passes 4, 5 and 6 21-22 s each for 337 queued and **0 routed**. Five sixths of the
  stage bought 8 connections out of 86. The loop's own javadoc says it exists to "auto-route
  ripup passes until the board is completed"; a pass that completed nothing has not moved
  towards that, and the only thing the next pass changes is the price of ripping copper that
  already refused to move.
  *Effect*: less wall clock, same board. Over the 21 suite fixtures that finish at `-mp 1`,
  every one produced a **byte-identical session** under the shipped metric, for 1.2x - 2.7x
  less wall — and `Issue190-processor.Z80` went from a 150 s timeout to finishing in 95 s.
  Sequentially: `Issue026 -mp 3` 129.1 s → 47.2 s and `-mp 1` 36.5 s → 21.7 s under the release
  metric; 64.2 s → 23.9 s and 27.7 s → 13.8 s under the weighted one. Under the release metric
  two fixtures (`Issue107`, both variants) do come out with different bytes and the same score,
  which is the expected consequence of that metric committing reroutes that changed nothing.

- **The inner reroute's incomplete floor is the ratsnest *before* the rip, not
  after it.** `MazeRerouter` used to seed `incomplete_floor` from the board the
  rip-out left, so restoring the ripped connection (7 → 6 on Issue110P, back to
  the pre-rip 6) looked like progress and earned a second inner pass. That pass
  queued the 10 leftover unrouted nets, routed 0, and cost another 8 s. Upstream
  does keep going through all six passes while `still_unrouted_items`; we still
  stop on a barren / net-destructive pass, and the floor is now the number
  `opt_route_item` measured *before* the rip (`RerouteRequest.incomplete_floor`).
  Restoring what we ripped is the job, not a reason to retry the backlog at 2×
  ripup. A pass that *closes a leftover* still drives the count below the floor
  and earns the next one.

- **Leftover unrouted nets get a probing retry, not the whole-board work list.**
  `autoroute_passes_for_optimizing_item` re-derives the whole board's work list,
  so every optimizer item on an incomplete board re-attempts every leftover
  connection. On Issue110P that is 10 `NotRouted` searches × ~1.4 s ≈ 7–9 s per
  item, against Java's 0.4 ms reroute. We restore the ripped connection at the
  full 100 s allowance, then — on the first item of a pass and after a keep
  that closed a connection — probe the leftovers at
  [`BACKLOG_CONNECTION_BUDGET_MS`] = 100 ms per connection and keep retrying
  with a doubling budget (cap [`BACKLOG_CONNECTION_BUDGET_CAP_MS`] = 1000 ms,
  the shape of upstream's `100000 * 2^(pass-1)`) only while the probing routes
  at least one connection. Via-only and length-only keeps do not re-open the
  backlog. Issue110P's leftovers never route, so they cost one 100 ms probe
  per pass (10 × 0.1 s, once); Issue155's do route, and the escalation closes
  all 5 (0 open, 0 violations, `905.65 → 999.99`, vs the flat-100 ms tiering
  that closed them but left 1 clearance violation, and vs 14.5 s at the old
  whole-board behaviour).

- **Items whose every net was already incomplete are not maze-searched.**
  The sweep includes traces that live on leftover-open nets. Ripping one and
  re-routing that net is a 2.5 s `q=4 nr=3` search on Issue110P that never
  closes anything — the first item's leftover try already attempted those nets.
  `attempt_item` returns `not_attempted` (counts toward consecutive failures)
  when every net of the item had `component_count > 1` before the rip. Fully
  routed nets (the optimizer's real work) are unchanged.

- **`maxConsecutiveFailures` resets only when an incomplete dropped.**
  Upstream (`BatchOptimizer.java:257-259`) clears the streak on any
  `improved()`. Under both length metrics a length-only micro-win is
  `improved()`, and a via-only keep is too; on Issue110P we kept 43 of 109
  items (3 of them via drops), score 911.91 → 911.92, and never hit the 50-item
  abort Java reaches in 0.55 s with 0 keeps. Length-only and via-only still
  commit — the board did get shorter / lose a via — but the streak counts them
  as failures. Tests:
  `length_only_keeps_do_not_reset_the_consecutive_failure_streak`,
  `via_only_keeps_do_not_reset_the_consecutive_failure_streak`.

- **... and it also stops at the first pass that opens more connections than it closes**
  (same function, second added term).
  *Why*: `routed > 0` is the right rule in the currency of attempts and the wrong one in the
  currency of results, because on a saturated board a connection is routed by breaking its
  neighbours. `Issue103-Board-Routed` arrives fully wired (0 unrouted, 63 violations, 982.02)
  and `FR_OPT_TRACE` over the first optimizer item — a via whose electrical connection is 16
  items — reads: inner pass 1, 2 queued, 1 routed, 43.2 s, board incompletes 1 → 8; inner pass
  2, 16 queued, 2 routed, 73.3 s, 8 → 13; inner pass 3, 25 queued, 0 routed. Every pass routes
  something and every pass leaves more open than it found — the per-connection records read
  `incomplete=1->15`, `8->19`, `19->22` with `ripped=0` throughout, so it is the *insertion*
  into full copper that shatters the neighbours, not the maze's ripup. `routed > 0` therefore
  holds all the way down and one item pays 296.6 s to arrive somewhere strictly worse than it
  started, at which point the improvement lattice rejects it and all of it is rolled back.
  *We* additionally require `incomplete_count < incomplete_floor`, the floor being the ratsnest
  count the pass began with (one extra ratsnest per item, against the two `stats::board_key`
  already takes). A pass that closes what it was given still earns the next one, so the
  multi-pass repair the escalation exists for is untouched.
  *Effect*: on `Issue103-Board-Routed` at a 300 s budget, **1 item / 0 improved → 37 items /
  7 improved**, and the pathological item costs 41.4 s instead of 296.6 s (7.2x). On
  `Issue230-CNH_Functional_Tester_1` the result is unchanged (14 of 185 improved, 959.60,
  16 violations, 5 open) for 195.9 s → 191.5 s. Over a 13-fixture A/B at `-mp 1` no fixture
  gained a violation or an open connection; `Issue034-Green14SegLED` came out *better*
  (2 violations → 1, 997.26 → 998.62) in 90.5 s → 61.0 s.
  *What it does not fix*: those numbers are still tens of seconds because one maze search into
  saturated copper costs 43 s on this board (`FR_OPT_TRACE` `conn` records), and because the
  insertion breaks neighbours at all. Both are outside the optimizer.

## Session file format

- **Half-unit trace corners are written with one decimal, not rounded away.**
  *Upstream* writes every SES coordinate as `(int) Math.round(...)` (`SesWriter.java`,
  `IndentFileWriter.write(Integer)`), and a `Polyline` corner is the intersection of two
  integer-coefficient `Line`s — which for two diagonals sits on the half-integer grid.
  Pull-tight produces exactly such corners, so upstream's writer (and a naive copy of it)
  moves that copper by up to 0.5 board units on the way out. Measured on
  `Issue102-Mars-64-revE-rot00`: a GND trace corner at (664434.5, -706045.5) rounded to
  (664435, -706045) narrowed a clearance-exact gap against a /VSS trace (layer 1, required
  2002) to 2001.9, i.e. the written-back session carried **one violation the in-memory board
  did not have** (65v vs 64v).
  *We* write an exact half-unit corner as one decimal (`15000.5`) when the session scale
  factor is 1 — every other value, and every value on a board with a coarser session scale,
  keeps the upstream rounded integer — and the reader rebuilds the exact polyline from the
  decimals (`Polyline::from_exact_corners`, falling back to upstream rounding for anything
  the kernel's `Vec<Line>` cannot hold).
  *Compatibility*: the file stays readable upstream — `SesReader` rounds each coordinate the
  same way it rounds its own sessions, so the jar/KiCad sees exactly what it would have seen
  from a jar-written file. The divergence is visible only as `0.5`-carrying coordinates in
  the text and as byte diffs against jar sessions on boards with half-unit corners.
  *Effect*: our own session round trip is now lossless for every polyline the router can
  produce; the G2.1 referee's "SES round-trip fidelity" check (in-memory referee vs
  file-based referee) goes to 0 moved boards. Regression tests:
  `fr_geometry/tests/exact_corners_test.rs`,
  `fr_io/tests/ses_format_test.rs::a_half_unit_corner_is_written_with_a_decimal_and_reads_back_exactly`.

## Session output

- **Empty identifiers are quoted** (`fr_io::specctra::ses_write::needs_quotes`). *Upstream*
  (`IdentifierType.write`, `IdentifierType.java:27-68`) writes an empty name as zero bytes: no
  reserved char, no non-ASCII byte, and `"".matches("^-?\\d.*")` is false. On a design whose DSN
  declares an empty-named image (`(image ""` — cla45-compute-hdi6cs, a real KiCad 10.0.5 export)
  the jar therefore emits `(component ` with no name, a session KiCad's specctra parser rejects
  outright (`ImportSpecctraSES` returns False, losing the whole import). *We* quote it:
  `(component "")`, which KiCad parses. *Why not "fix" it the other way* (dropping the component
  scope): the placement records are real and the importer needs them. Effect: sessions for
  designs with empty-named packages import into KiCad; byte-divergence from the jar is confined
  to files the jar itself renders unimportable.

## Referee

- **The G2.1 session overlay has no upstream counterpart.** Upstream reads a session back into
  whatever board is loaded, because interactively that board *is* the one the session came from.
  Our gate has to judge a file, so it reads it into
  `fr_io::specctra::strip_session_copper(dsn)` — the DSN with exactly the copper a session
  re-emits removed. Not a behaviour change in the product (nothing on the `-de/-do` path calls
  it); a measurement decision, and the one the numbers in `docs/ROADMAP.md` G2.1 are produced
  with. Effect if it is got wrong: reading into the *full* import double-inserts every
  pre-existing trace (Issue093: 192 judged violations → 322); reading into an *emptied*
  `(wiring …)` scope drops `(type fix)` copper no session can carry (Issue230: 5u/16v → 8u/14v,
  i.e. three connections lost and two violations silently forgiven). Normative description:
  `bench/README.md`, "Judging a session"; the module docs of `fr_io::specctra::overlay` carry
  the fixture evidence.

- **Parallel connection routing (`--parallel-connections N`, default 1)**
  (`docs/upstream-study/parallel-connections.md`). *Upstream behaviour*: Upstream's autoroute
  pass (`BatchAutorouter.autoroute_pass`) routes all connections in the work-list strictly
  sequentially on a single thread. *Our behaviour*: Under `--parallel-connections N` (N > 1),
  connections are evaluated concurrently on board snapshots across worker threads and committed
  in strict original work-list order on the master board with segment-level footprint conflict
  detection and transactional DRC clearance oracle verification. Conflicting attempts or
  ripped nets gracefully fall back to serial routing. For any fixed thread count N, output is
  bit-for-bit deterministic. Zero added violations are preserved across all configurations.

## Candidates (recorded ahead of any code change; **no behaviour change yet**)

- **Free-space room decomposition around an SMD pin can seal the pin's start
  room into a one-exit dead pocket, killing the fanout escape that upstream
  completes** (candidate owner: the tiling45 room work; evidence
  `~/fr-work/reports/REPORT-settle-diff.md`). *Upstream behaviour*: on
  Issue110-RelayModule the fanout escape of U1-32 (net /CAN_RX) succeeds in
  fanout pass 1 (verified with the rebuilt lab jar's `FRLAB fanout` lines).
  *Our behaviour*: the same escape fails on every fanout pass with the same
  signature — one start room, one expansion, frontier empty
  (`outcome=exhausted, expansions=1`), and the start door's heuristic is
  232 151.9 although the joined destination pad sits ~102.6K from the pin, i.e.
  the pin's start room has no door toward the destination at all. The cascade
  (pass-1 work-list/destination structure on /CAN_RX, and the same door-graph
  divergence on already-aligned connections) is mapped per connection by the
  settle-diff harness: lab jar `~/fr-ref/fr-lab-release/freerouting-lab.jar`
  (`-Dfrlab.conn_trace=1`), our `FR_CONN_TRACE` `FRCONN conn/aim/join` records,
  and `bench/tools/conn_diff.py`. Sharpest repro: RelayModule, fanout stage,
  net /CAN_RX, pin U1-32 — one expansion then exhaustion.
  Status: recorded, unfixed — the fix belongs to the room/door decomposition
  (`rooms/complete*.rs`, `neighbours*.rs`).

- **Edge.Cuts inner cutouts mapped to board_edge clearance class** (owner: `fr-io`, TASK-cutout-keepout).
  *Data* (REPORT-easyeda class (b), evidence `~/fr-work/reports/drc-delta-classification.md`):
  29 of the stable added violations in the OOS G4 KiCad-DRC delta — cla45-sensor-l6 13,
  cla45-sensor-hdi 16 — were `copper_edge_clearance` against **inner** cutouts, actual
  0.13–0.5 mm vs the required 0.5000 mm.
  *Upstream behaviour*: KiCad exports Edge.Cuts inner cutouts into the Specctra file as
  `(keepout (polygon signal …))` whose bbox is the cutout (measured). Upstream Freerouting
  treats keepouts as hard obstacles with zero/default clearance — traces may run flush against
  it — and the synthesized `board_edge` 500 µm clearance class covered only the outer
  `(boundary)`. KiCad, on the same geometry, enforces its built-in `copper_edge` default of
  0.5 mm (the fixture `(setup …)` declares no constraint fields).
  *Resolution*: KiCad 10's Specctra exporter source (`pcbnew/specctra_import_export/specctra_export.cpp`)
  was audited. Inner cutouts from `m_brd_outlines.HoleCount` in `fillBOUNDARY` are exported
  exclusively via `poly_ko->SetLayerId("signal")` as unnamed structure-level keepouts without explicit
  clearance class (i.e. `(keepout "" (polygon signal 0 ...))`). User rule areas in `FromBOARD` are
  iterated across concrete copper layers (`F.Cu`, `B.Cu`, etc.) via `AllCuMask` and never exported on
  the pseudo-layer `signal`, and named rule areas carry non-empty names. `fr-io` distinguishes inner cutouts
  unambiguously (`area.shapes.iter().all(|s| s.layer().is_signal_pseudo()) && area.name.is_empty() && area.clearance_class.is_none()`)
  and maps them to the `board_edge` clearance class by default (500 µm, matching outer board boundary).
  A CLI flag `--cutout-clearance <UM>` is also provided for explicit overrides. This resolves
  the 29 residual violations on sensor-l6 and sensor-hdi down to 0.

- **Cosmetic: `(board_edge, board_edge)` diagonal clearance cells are materialized by our
  import where the Java dump leaves them absent.** easyeda-pcb1 G1.0 golden compare
  (REPORT-easyeda §1): exactly 4 cells differ — `(board_edge,board_edge)` diagonal on
  layers 1/2/15/16, ours 19.686, Java absent. Edge-vs-edge self-clearance has no routing
  semantics and no fixture gates on it; recorded so the next golden compare does not
  re-investigate. Worth an `fr-io` look only if G1.1 ever gates this fixture.

- **Copper text is a KiCad-only obstacle class** (gate calibration, not a code divergence).
  KiCad DRC checks copper against `gr_text`/`fp_text` on copper layers (coldfire-unrouted:
  our `/CLKMOD1` trace vs text 'MOD1', actual 0.0000). The DSN carries no text at all, so
  neither the jar nor we can avoid it — unmodelable from the input, not a routing mistake.
  Dispatcher decision (TASK-short-hdi): exempt in the G4 hard-gate accounting.
  `bench/tools/g4_kicad.py` counts violations touching a text object as `kicad_only`,
  separate from `residual`.

## Rejected divergences (things we deliberately do NOT fix)

- **`SesReader` counting a wire it could not insert.** `SesReader.processWireScope` calls the
  `void` `BasicBoard.insert_trace`, which silently drops a polyline that collapses to fewer than
  two corners (`BasicBoard.java:244-255`), then increments `wiresImported` and returns `true`
  (`SesReader.java:293-297`); only a thrown exception reaches `errorsEncountered`. Our reader
  used to return `false` there and charge an error, which is arguably better and is still wrong:
  `SesImportSummary`'s counters are a contract compared against the jar's, and our own writer
  legitimately produces such wires (§12.3's duplicate-corner elision runs in session units, so
  two corners one session unit apart round back onto the same board coordinate —
  `Issue039-bug-design`). Replicated, with a warning the jar does not emit. Whether the board
  really received everything the file describes is answered by comparing boards, not counters:
  `bench/tools/g21_judge.py`'s fidelity check does that against the router's in-process referee
  line.

- `class_class` inner-iterator pairing bug: replicated (identical clearance matrix). No fixture
  exercises it; real-world files may — revisit only with a test case.
- `ignore_net_classes` no-op headless: replicated as intentionally dead.
- `ClearanceMatrix.getValue` out-of-range → 0: replicated.
- `splitInsideDrillPadProhibited` `&&`/`||` precedence bug: replicated (routing-visible).

- **Skipping the optimizer because the board is incomplete, or because the score is already
  near the maximum.** Tempting: the stage is most of the wall on the budget-burners, and a
  board that still has open connections will not be "finished" by a length-only optimizer.
  Rejected. Upstream's headless entry is one unguarded `getRunOptimizer()` after the router
  (`RoutingJobSchedulerActionThread.java:179`); the near-maximum-score break is an *inner*
  loop exit, not a reason not to enter. A skip here would be a real divergence, not a
  constant-factor, and it would make every incomplete G2.1 board incomparable to the jar.
  The clean-pipeline check on three boards (Issue026, Issue034, Issue230) under a fresh
  `--user_data_path` is that the jar *does* enter the stage whenever `optimizer.enabled` is
  true — including on boards the router did not complete. The earlier "jar never ran the
  optimizer" reading is the `$TMPDIR/freerouting/freerouting.json` contaminant above, not
  an entry-condition difference. `fr_core::pipeline::the_optimizer_stage_entry_does_not_read_the_routing_result`
  is the tripwire.

- **Shove/insert acceptance stays on the compensated tree, without the +16 safety
  margin.**
  - *Upstream behaviour*: reads clearance through `BasicBoard.clearance_value` = `rules.clearance_matrix.get_value(p_class_1, p_class_2, p_layer, true)`
  (`app/freerouting/board/BasicBoard.java:1600` / `src_v19/.../BasicBoard.java:1209`).
  `ClearanceMatrix.get_value(..., boolean p_add_safety_margin)` (`app/freerouting/rules/ClearanceMatrix.java:151`) adds:
  `int final_value = p_add_safety_margin ? value_from_the_matrix + clearance_safety_margin : value_from_the_matrix;`
  where `clearance_safety_margin = 16` (`:16`).
  Seven audited call sites in upstream pass `true` or call `clearance_value`:
  `RoutingBoard.check_trace_segment` (`app/freerouting/board/RoutingBoard.java:284`),
  `ShoveTraceAlgo.check` (`app/freerouting/board/ShoveTraceAlgo.java:105,149,545`),
  `ShapeTraceEntries` (`app/freerouting/board/ShapeTraceEntries.java:61,191,273,325-326`),
  `MoveDrillItemAlgo` (`app/freerouting/board/MoveDrillItemAlgo.java:194`),
  `ForcedViaAlgo.check_layer` (`app/freerouting/board/ForcedViaAlgo.java:40`),
  `InsertFoundConnectionAlgo` (`app/freerouting/autoroute/InsertFoundConnectionAlgo.java:356`),
  and candidate search query `ShapeSearchTree.overlapping_tree_entries_with_clearance` (`app/freerouting/board/ShapeSearchTree.java:459`).
  - *Our behaviour*: candidates are queried off the compensated tree, reading raw `ClearanceMatrix::get_value` without adding +16 in shove/insert.
  Pull-tight uses DRC-symmetric clearance verification.
  - *Evidence*: `REPORT-shove-oracle.md` (commit `da4f76f`), `REPORT-shove-remeasure.md` (margin model `c93830b` caused 14 of 24 boards to lose connections;
  retained model achieves 0 added violations and beats Java on Issue034).
  - *Status*: 有意分歧 (retained model strictly superior in quality and routability).

- **Fanout stage is capped to a share of the job budget** (`fr_router::fanout`, `FR_FANOUT_BUDGET_PCT`, intentional divergence).
  - *Upstream behaviour*: `BatchFanout.java:82-144` has no job-budget concept; the stage runs to its three
  stop rules regardless of wall clock.
  - *Our behaviour*: when a job deadline is armed (`--job-timeout`), the stage spends at most
  `FR_FANOUT_BUDGET_PCT` (default 25) percent of what remains when it starts, then stops between pins with
  `FanoutStop::StageBudget`. Without a deadline the stage is uncapped and byte-identical to upstream
  (verified: Issue102 det, no deadline, SES md5 equal).
  - *Why*: dropping the fanout no-progress stop (REPORT-full-relay.md) let dense boards' fanout stages
  consume most of the job budget before the main router could repair their output. Measured at 285 s
  (REPORT-fanout-budget.md): compute-hdi6cs fanout 280.7 s → 70.3 s and 614u → 607u; Issue219 77 s cap →
  299u → 275u; sensor-l6 96.5 s cap → 119u → 117u; Issue039 unaffected (its fanout needs only 24 s —
  its regression is escape-count-driven, not time-driven).
  - *Status*: 有意分歧（仅当存在 job 预算时生效）.
